//! Meaning search: records and the question turned into vectors by an embeddings service, ranked by
//! cosine similarity, so "the login was slow" finds "per-request cache" without a shared word. Off
//! until an endpoint is set (`reprise embeddings set`): any OpenAI-compatible `/embeddings` URL, such as
//! a local Ollama or OpenRouter. Requests go through the system's `curl`, the key on its stdin.
//! Vectors are kept in `.reprise/cache/vectors.sqlite`, by file and content, so only records that
//! changed are sent again.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::search::Hit;
use crate::{Error, Result, Vault};

/// Characters of a record sent to be embedded.
const TEXT_LIMIT: usize = 2000;
/// Texts per request.
const BATCH: usize = 32;
/// Similarity under which a record is not reported.
const MIN_SIMILARITY: f32 = 0.3;

/// Where vectors come from. Per machine, in reprise's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Embeddings {
    /// An OpenAI-compatible embeddings endpoint, e.g. `http://localhost:11434/v1/embeddings`.
    pub url: String,
    pub model: String,
    /// The variable that holds the key, when the service needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_env: Option<String>,
}

fn engine_error(msg: impl Into<String>) -> Error {
    Error::InvalidChange(msg.into())
}

/// The vectors of `texts`, from the service, in order.
pub fn embed(config: &Embeddings, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let curl = crate::env_var("CURL_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("curl"));
    let mut out = Vec::with_capacity(texts.len());
    for batch in texts.chunks(BATCH) {
        let body = json!({"model": config.model, "input": batch});
        let mut headers = String::from("header = \"Content-Type: application/json\"\n");
        if let Some(var) = &config.key_env {
            let key = std::env::var(var)
                .map_err(|_| engine_error(format!("the embeddings key: {var} is not set")))?;
            headers.push_str(&format!(
                "header = \"Authorization: Bearer {}\"\n",
                key.trim()
            ));
        }
        let mut child = Command::new(&curl)
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                "120",
                "--config",
                "-",
            ])
            .args(["--data-binary", &body.to_string(), &config.url])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| engine_error(format!("curl could not be started: {e}")))?;
        use std::io::Write as _;
        let _ = child
            .stdin
            .take()
            .expect("piped")
            .write_all(headers.as_bytes());
        let output = child
            .wait_with_output()
            .map_err(|e| engine_error(e.to_string()))?;
        if !output.status.success() {
            return Err(engine_error(format!(
                "the embeddings request failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        out.extend(parse(
            &String::from_utf8_lossy(&output.stdout),
            batch.len(),
        )?);
    }
    Ok(out)
}

/// The vectors in an OpenAI-style reply, in input order.
fn parse(reply: &str, expected: usize) -> Result<Vec<Vec<f32>>> {
    let v: Value = serde_json::from_str(reply).map_err(|_| {
        engine_error(format!(
            "the embeddings reply is not JSON: {}",
            crate::text::clip(reply, 200)
        ))
    })?;
    if let Some(msg) = v["error"]["message"]
        .as_str()
        .or_else(|| v["error"].as_str())
    {
        return Err(engine_error(format!("embeddings: {msg}")));
    }
    let mut items: Vec<(u64, Vec<f32>)> = v["data"]
        .as_array()
        .ok_or_else(|| engine_error("the embeddings reply has no data"))?
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let index = d["index"].as_u64().unwrap_or(i as u64);
            let vec = d["embedding"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_f64)
                        .map(|x| x as f32)
                        .collect()
                })
                .unwrap_or_default();
            (index, vec)
        })
        .collect();
    items.sort_by_key(|(i, _)| *i);
    if items.len() != expected || items.iter().any(|(_, v)| v.is_empty()) {
        return Err(engine_error(format!(
            "asked for {expected} vectors, got {} usable",
            items.iter().filter(|(_, v)| !v.is_empty()).count()
        )));
    }
    Ok(items.into_iter().map(|(_, v)| v).collect())
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn open_cache(vault: &Vault) -> Result<rusqlite::Connection> {
    let dir = vault.cache_dir();
    crate::safe::create_dir_all(&dir)?;
    let db = rusqlite::Connection::open(dir.join("vectors.sqlite"))
        .map_err(|e| engine_error(format!("the vector cache: {e}")))?;
    // Two searches at once (a hook and the CLI) wait for each other instead of failing.
    db.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| engine_error(format!("the vector cache: {e}")))?;
    db.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| engine_error(format!("the vector cache: {e}")))?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS vectors (path TEXT NOT NULL, model TEXT NOT NULL, hash TEXT NOT NULL, \
         vec BLOB NOT NULL, PRIMARY KEY (path, model));",
    )
    .map_err(|e| engine_error(format!("the vector cache: {e}")))?;
    Ok(db)
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Turns texts into vectors, in order.
pub type Embedder<'a> = dyn Fn(&[String]) -> Result<Vec<Vec<f32>>> + 'a;

/// The records of `projects` (with notes and profile) most like `question`, best first, with their
/// similarity in `matched`. `embedder` turns texts into vectors (`embed` with the settings).
pub fn semantic(
    vault: &Vault,
    projects: &[String],
    question: &str,
    limit: usize,
    model: &str,
    embedder: &Embedder,
) -> Result<Vec<Hit>> {
    let records = crate::search::all_records(vault, projects)?;
    let db = open_cache(vault)?;
    let key = |p: &Path| {
        p.strip_prefix(vault.root())
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(records.len());
    let mut missing: Vec<(usize, String, String)> = Vec::new();
    for (i, (hit, text)) in records.iter().enumerate() {
        let text = crate::secret::redact(&crate::text::clip(text, TEXT_LIMIT)).into_owned();
        let hash = content_hash(&text);
        let cached: Option<Vec<u8>> = db
            .query_row(
                "SELECT vec FROM vectors WHERE path = ?1 AND model = ?2 AND hash = ?3",
                rusqlite::params![key(&hit.path), model, hash],
                |r| r.get(0),
            )
            .ok();
        match cached {
            Some(b) => vectors.push(Some(from_blob(&b))),
            None => {
                vectors.push(None);
                missing.push((i, hash, text));
            }
        }
    }
    if !missing.is_empty() {
        let texts: Vec<String> = missing.iter().map(|(_, _, t)| t.clone()).collect();
        let fresh = embedder(&texts)?;
        for ((i, hash, _), v) in missing.into_iter().zip(fresh) {
            let _ = db.execute(
                "INSERT OR REPLACE INTO vectors (path, model, hash, vec) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![key(&records[i].0.path), model, hash, to_blob(&v)],
            );
            vectors[i] = Some(v);
        }
    }
    let q = embedder(&[question.to_owned()])?
        .pop()
        .ok_or_else(|| engine_error("no vector for the question"))?;
    let mut scored: Vec<(f32, Hit)> = records
        .into_iter()
        .zip(vectors)
        .filter_map(|((hit, _), v)| Some((cosine(&q, &v?), hit)))
        .filter(|(s, _)| *s >= MIN_SIMILARITY)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(s, mut hit)| {
            hit.score = (s * 1000.0) as usize;
            hit.matched = vec![format!("similarity {s:.2}")];
            hit
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toy embedder: how often each of a few words occurs.
    fn toy(texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let words = ["slow", "cache", "login", "deploy", "port"];
        Ok(texts
            .iter()
            .map(|t| {
                let t = t.to_lowercase();
                words.iter().map(|w| t.matches(w).count() as f32).collect()
            })
            .collect())
    }

    #[test]
    fn the_reply_is_read_in_order() {
        let reply = r#"{"data":[{"index":1,"embedding":[0,1]},{"index":0,"embedding":[1,0]}]}"#;
        assert_eq!(
            parse(reply, 2).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );
        assert!(parse(reply, 3).is_err());
        assert!(
            parse(r#"{"error":{"message":"bad key"}}"#, 1)
                .unwrap_err()
                .to_string()
                .contains("bad key")
        );
        assert!(parse("<html>", 1).is_err());
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn records_are_ranked_by_meaning_and_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        let write = |rel: &str, text: &str| {
            let p = vault.root().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write(
            "projects/p/items/p-0001-a.md",
            "---\nid: p-0001\ntitle: Login cache\n---\nThe login was slow; cache it.\n",
        );
        write(
            "projects/p/items/p-0002-b.md",
            "---\nid: p-0002\ntitle: Deploy port\n---\nDeploy on port 9090.\n",
        );
        let calls = std::cell::Cell::new(0);
        let counting = |t: &[String]| {
            calls.set(calls.get() + t.len());
            toy(t)
        };
        let hits = semantic(&vault, &["p".into()], "slow login", 5, "toy", &counting).unwrap();
        assert_eq!(hits[0].id, "p-0001");
        assert!(hits.iter().all(|h| h.id != "p-0002"), "nothing in common");
        let first = calls.get();
        semantic(&vault, &["p".into()], "slow login", 5, "toy", &counting).unwrap();
        assert_eq!(calls.get() - first, 1, "only the question is sent again");
    }
}
