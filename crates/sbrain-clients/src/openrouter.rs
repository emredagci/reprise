//! OpenRouter as the engine: sbrain's own work (distilling) through OpenRouter's chat API, with any
//! model it serves, `typesafe/jev-router` included. It keeps no transcripts and cannot be connected; it
//! only distills. The request goes through the system's `curl`, so sbrain carries no HTTP or TLS code;
//! the key reaches curl on stdin, never on its command line, where other users could see it.
//!
//! The key is `OPENROUTER_API_KEY`, else the file `openrouter.key` beside sbrain's settings (the
//! scheduled sweep does not see the shell's variables).

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::{Client, ClientId, Detection, Digest, DistillRun, Error, Result, Sweeper, Transcript};

const URL: &str = "https://openrouter.ai/api/v1/chat/completions";
/// The key file's name, beside sbrain's settings.
pub const KEY_FILE: &str = "openrouter.key";

pub struct OpenRouter;

/// The key: the variable, else the file; `None` when neither has one.
pub fn key() -> Option<String> {
    if let Some(k) = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
    {
        return Some(k);
    }
    let file = key_file()?;
    std::fs::read_to_string(file)
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

/// `openrouter.key` beside sbrain's settings.
pub fn key_file() -> Option<PathBuf> {
    sbrain_core::Settings::default_path()
        .ok()?
        .parent()
        .map(|d| d.join(KEY_FILE))
}

fn curl() -> Option<PathBuf> {
    sbrain_core::env_var("CURL_BIN")
        .map(PathBuf::from)
        .or_else(|| crate::find_program("curl"))
}

impl Client for OpenRouter {
    fn id(&self) -> ClientId {
        ClientId::OpenRouter
    }

    fn display_name(&self) -> &'static str {
        "OpenRouter"
    }

    /// Usable when curl is there and a key is set.
    fn detect(&self) -> Detection {
        Detection {
            client: ClientId::OpenRouter,
            binary: curl().filter(|_| key().is_some()),
            version: None,
        }
    }

    fn sweeper(&self) -> Option<&dyn Sweeper> {
        Some(self)
    }
}

impl Sweeper for OpenRouter {
    fn transcripts(&self, _skip: &std::path::Path) -> Result<Vec<Transcript>> {
        Ok(Vec::new())
    }

    fn digest(&self, path: &std::path::Path) -> Result<Digest> {
        Err(Error::Engine(format!(
            "OpenRouter keeps no transcripts: {}",
            path.display()
        )))
    }

    fn distill(&self, run: &DistillRun) -> Result<Value> {
        let model = run.model.filter(|m| !m.is_empty()).ok_or_else(|| {
            Error::Engine(
                "OpenRouter needs a model: `sbrain engine set openrouter --model <id>`".into(),
            )
        })?;
        let key = key().ok_or_else(|| {
            Error::Engine(format!(
                "no OpenRouter key: set OPENROUTER_API_KEY or write it to {}",
                key_file().map_or_else(|| KEY_FILE.into(), |f| f.display().to_string())
            ))
        })?;
        let curl = curl().ok_or_else(|| Error::Engine("curl is not installed".into()))?;
        let schema: Value = serde_json::from_str(run.schema)
            .map_err(|e| Error::Engine(format!("the answer schema: {e}")))?;
        let body = json!({
            "model": model,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": run.system},
                {"role": "user", "content": run.input},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "distill", "strict": true, "schema": schema},
            },
        });
        // The body in a file of the distiller's folder, the key and headers on stdin.
        std::fs::create_dir_all(run.cwd).map_err(|e| Error::Engine(e.to_string()))?;
        let body_file = run
            .cwd
            .join(format!("openrouter-{}.json", std::process::id()));
        std::fs::write(&body_file, body.to_string()).map_err(|e| Error::Engine(e.to_string()))?;
        let config = format!(
            "header = \"Authorization: Bearer {key}\"\nheader = \"Content-Type: application/json\"\n"
        );
        let url = sbrain_core::env_var("OPENROUTER_URL")
            .map_or_else(|| URL.to_owned(), |u| u.to_string_lossy().into_owned());
        let mut cmd = std::process::Command::new(&curl);
        cmd.args(["--silent", "--show-error", "--config", "-", "--data-binary"])
            .arg(format!("@{}", body_file.display()))
            .arg(&url)
            .current_dir(run.cwd);
        let out = crate::process::run(cmd, "curl", &config, run.timeout);
        let _ = std::fs::remove_file(&body_file);
        let out = out?;
        if !out.success {
            return Err(Error::Engine(format!("curl failed: {}", out.stderr.trim())));
        }
        answer(&out.stdout)
    }
}

/// The JSON the model wrote, from OpenRouter's reply; its error message when the reply is one.
fn answer(reply: &str) -> Result<Value> {
    let reply: Value = serde_json::from_str(reply).map_err(|_| {
        Error::Engine(format!(
            "OpenRouter answered with something that is not JSON: {}",
            sbrain_core::text::clip(reply, 300)
        ))
    })?;
    if let Some(msg) = reply["error"]["message"].as_str() {
        return Err(Error::Engine(format!("OpenRouter: {msg}")));
    }
    let content = reply["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| Error::Engine("OpenRouter's reply has no message".into()))?;
    let content = content.trim();
    let content = content
        .strip_prefix("```json")
        .or_else(|| content.strip_prefix("```"))
        .and_then(|c| c.strip_suffix("```"))
        .unwrap_or(content)
        .trim();
    serde_json::from_str(content)
        .map_err(|e| Error::Engine(format!("the model's answer is not JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_read_from_the_reply() {
        let ok = r#"{"choices":[{"message":{"content":"```json\n{\"card\":1}\n```"}}]}"#;
        assert_eq!(answer(ok).unwrap(), json!({"card": 1}));
        let plain = r#"{"choices":[{"message":{"content":"{\"a\":2}"}}]}"#;
        assert_eq!(answer(plain).unwrap(), json!({"a": 2}));
        let err = r#"{"error":{"message":"No auth credentials found","code":401}}"#;
        assert!(
            answer(err)
                .unwrap_err()
                .to_string()
                .contains("No auth credentials")
        );
        assert!(answer("<html>").is_err());
        assert!(answer(r#"{"choices":[{"message":{"content":"nope"}}]}"#).is_err());
    }
}
