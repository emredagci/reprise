//! Multi-query search over a project's records, notes and profile files.
//! Each query matches a record when at least 60% of its terms match; below that it is a partial
//! match, reported separately because the missing piece of an answer often sits there. Records linked
//! to a hit (its `related`, records whose `related` names it, records that mention its id) are
//! reported too, one step away: a question about "the login cache" finds the decision behind it even
//! when the decision never says "cache".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::record::Record;
use crate::report::markdown_files;
use crate::text::tokens;
use crate::{Result, Vault};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    pub kind: &'static str,
    pub project: Option<String>,
    pub id: String,
    pub title: String,
    pub status: String,
    pub path: PathBuf,
    pub score: usize,
    pub matched: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SearchResult {
    pub scanned: BTreeMap<&'static str, usize>,
    pub total_hits: usize,
    /// Best first, at most `limit`.
    pub hits: Vec<Hit>,
    pub partial_matches: Vec<Hit>,
    /// Records one link away from the hits, not hits themselves; `matched` says through which.
    pub linked: Vec<Hit>,
    pub terms_without_any_match: Vec<String>,
}

/// What a record links to: its id, its `related` ids and its text, to find links both ways.
struct Links {
    id: String,
    related: Vec<String>,
    body: String,
}

pub fn search(
    vault: &Vault,
    projects: &[String],
    queries: &[String],
    limit: usize,
) -> Result<SearchResult> {
    let queries: Vec<&String> = queries.iter().filter(|q| !q.trim().is_empty()).collect();
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();
    for q in &queries {
        for t in tokens(q) {
            seen.entry(t).or_insert(false);
        }
    }

    let mut result = SearchResult::default();
    let mut hits = Vec::new();
    let mut partial = Vec::new();
    // Every record, as a hit with no score, and its links.
    let mut all: Vec<(Hit, Links)> = Vec::new();
    for (kind, project, path) in corpus(vault, projects) {
        *result.scanned.entry(kind).or_insert(0) += 1;
        let record = Record::read(&path)?;
        let head = [
            record.text("id").to_owned(),
            record.text("title").to_owned(),
            record
                .get("aliases")
                .map(|v| v.joined())
                .unwrap_or_default(),
        ]
        .join(" ");
        let title_tokens: BTreeSet<String> = tokens(&head).into_iter().collect();
        let file_tokens: BTreeSet<String> =
            tokens(&record.get("files").map(|v| v.joined()).unwrap_or_default())
                .into_iter()
                .collect();
        let body_tokens: BTreeSet<String> = tokens(&record.body).into_iter().collect();
        let fields = [&title_tokens, &file_tokens, &body_tokens];

        let (mut score, mut matched, mut weak) = (0, BTreeSet::new(), BTreeSet::new());
        for q in &queries {
            let terms = tokens(q);
            if terms.is_empty() {
                continue;
            }
            let got: Vec<&String> = terms
                .iter()
                .filter(|t| fields.iter().any(|f| term_hits(t, f)))
                .collect();
            for t in &got {
                seen.insert((*t).clone(), true);
            }
            if got.len() * 5 < terms.len() * 3 {
                weak.extend(got.into_iter().cloned());
                continue;
            }
            for t in got {
                score += 3 * usize::from(term_hits(t, &title_tokens))
                    + 2 * usize::from(term_hits(t, &file_tokens))
                    + usize::from(term_hits(t, &body_tokens));
                matched.insert(t.clone());
            }
        }

        let id = record
            .first_truthy(&["id", "session"])
            .map(str::to_owned)
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let hit = |words: BTreeSet<String>, score| Hit {
            kind,
            project: project.clone(),
            id: id.clone(),
            title: record.text("title").to_owned(),
            status: record.text("status").to_owned(),
            path: path.clone(),
            score,
            matched: words.into_iter().collect(),
        };
        all.push((
            hit(BTreeSet::new(), 0),
            Links {
                id: id.clone(),
                related: record.get("related").map(|v| v.items()).unwrap_or_default(),
                body: record.body.clone(),
            },
        ));
        if matched.is_empty() {
            if !weak.is_empty() {
                partial.push(hit(weak, 0));
            }
            continue;
        }
        hits.push(hit(matched, score));
    }

    // Stable: equal scores keep corpus order.
    hits.sort_by_key(|h| std::cmp::Reverse(h.score));
    result.total_hits = hits.len();
    result.hits = hits.into_iter().take(limit).collect();
    result.partial_matches = partial.into_iter().take(limit).collect();
    result.linked = linked(&result.hits, &all, limit);
    result.terms_without_any_match = seen
        .into_iter()
        .filter(|(_, s)| !s)
        .map(|(t, _)| t)
        .collect();
    Ok(result)
}

/// Records one link away from `hits` that are not hits: named in a hit's `related`, naming a hit in
/// theirs, or mentioning a hit's id; each with the hit it came through in `matched`.
fn linked(hits: &[Hit], all: &[(Hit, Links)], limit: usize) -> Vec<Hit> {
    let is_id = |id: &str| id.contains('-') && id.chars().any(|c| c.is_ascii_digit());
    let hit_ids: BTreeSet<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    let mut out: Vec<Hit> = Vec::new();
    for h in hits {
        let Some((_, from)) = all.iter().find(|(r, _)| r.path == h.path) else {
            continue;
        };
        for (record, links) in all {
            if hit_ids.contains(links.id.as_str()) || out.iter().any(|o| o.path == record.path) {
                continue;
            }
            let link = from.related.contains(&links.id)
                || links.related.contains(&h.id)
                || (is_id(&h.id) && mentions(&links.body, &h.id))
                || (is_id(&links.id) && mentions(&from.body, &links.id));
            if link {
                let mut r = record.clone();
                r.matched = vec![format!("via {}", h.id)];
                out.push(r);
            }
        }
    }
    out.truncate(limit);
    out
}

/// Whether `text` names `id` as a whole word (`app-0001`, not `app-00010`).
fn mentions(text: &str, id: &str) -> bool {
    text.match_indices(id).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + id.len()..].chars().next();
        let part = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-');
        !part(before) && !part(after)
    })
}

/// A rough stem match for Turkish suffixes. Terms of 4+ characters match any
/// token starting with their first 5 characters; shorter ones must match exactly.
pub fn term_hits(term: &str, tokens: &BTreeSet<String>) -> bool {
    if term.chars().count() < 4 {
        return tokens.contains(term);
    }
    let stem = crate::text::take_chars(term, 5);
    tokens.iter().any(|t| t.starts_with(stem))
}

/// (kind, project, path): the projects' files, then notes and profile files, which are always included.
fn corpus(vault: &Vault, projects: &[String]) -> Vec<(&'static str, Option<String>, PathBuf)> {
    let mut out = Vec::new();
    for project in projects {
        let root = vault.project_dir(project);
        for (name, kind) in [("state.md", "state"), ("gotchas.md", "gotchas")] {
            if root.join(name).exists() {
                out.push((kind, Some(project.clone()), root.join(name)));
            }
        }
        for f in markdown_files(&root) {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name != "state.md" && name != "gotchas.md" {
                out.push(("doc", Some(project.clone()), f));
            }
        }
        for (sub, kind) in [
            ("items", "item"),
            ("decisions", "decision"),
            ("sessions", "session"),
        ] {
            for f in markdown_files(&root.join(sub)) {
                out.push((kind, Some(project.clone()), f));
            }
        }
    }
    let mut notes = Vec::new();
    collect_markdown(&vault.notes_dir(), &mut notes);
    // Sorted by components, so `a/b.md` comes before `a.md`.
    notes.sort_by(|a, b| a.components().cmp(b.components()));
    out.extend(notes.into_iter().map(|f| ("note", None, f)));
    for f in markdown_files(&vault.profile_dir()) {
        out.push(("profile", None, f));
    }
    out
}

/// `**/*.md` below `dir`, skipping hidden files and folders.
fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|x| x == "md") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn records_one_link_away_are_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        w(
            root,
            "projects/p/items/p-0001-x.md",
            "---\nid: p-0001\ntitle: Login cache\nstatus: doing\nrelated: [p-D-0002]\n---\nCache it.\n",
        );
        w(
            root,
            "projects/p/decisions/p-D-0002-x.md",
            "---\nid: p-D-0002\ntitle: Per request only\nstatus: accepted\n---\nNo global state.\n",
        );
        w(
            root,
            "projects/p/items/p-0003-x.md",
            "---\nid: p-0003\ntitle: Measure it\nstatus: inbox\nrelated: [p-0001]\n---\nAfter p-0001.\n",
        );
        w(
            root,
            "projects/p/items/p-0004-x.md",
            "---\nid: p-0004\ntitle: Unrelated\nstatus: inbox\n---\nSee p-00010, not it.\n",
        );
        let r = search(&vault, &["p".into()], &["login cache".into()], 10).unwrap();
        assert_eq!(r.hits.len(), 1);
        let linked: Vec<(&str, &str)> = r
            .linked
            .iter()
            .map(|h| (h.id.as_str(), h.matched[0].as_str()))
            .collect();
        assert_eq!(
            linked,
            [("p-0003", "via p-0001"), ("p-D-0002", "via p-0001")],
            "both directions, and not a longer id"
        );
    }

    #[test]
    fn scores_partial_and_unmatched_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        w(
            root,
            "projects/p/items/p-0001.md",
            "---\nid: p-0001\ntitle: Ödeme ekranı çöküyor\nstatus: inbox\naliases: [payment crash]\nfiles: [src/pay.rs]\n---\nStripe callback.\n",
        );
        w(
            root,
            "projects/p/decisions/p-D-0001.md",
            "---\nid: p-D-0001\ntitle: Ödemeler Stripe ile\nstatus: accepted\n---\n",
        );
        w(root, "projects/p/state.md", "# p\n");
        w(root, "notes/misc.md", "nothing\n");
        let q = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        let r = search(&vault, &q(&["p"]), &q(&["odeme", "stripe", "zzzz"]), 50).unwrap();
        // The decision matches both terms in its title.
        let got: Vec<_> = r.hits.iter().map(|h| (h.id.as_str(), h.score)).collect();
        assert_eq!(got, [("p-D-0001", 6), ("p-0001", 4)]);
        assert_eq!(r.hits[1].matched, ["odeme", "stripe"]);
        assert_eq!(r.terms_without_any_match, ["zzzz"]);
        assert_eq!(r.scanned.get("note"), Some(&1));
        assert_eq!(r.scanned.get("profile"), Some(&2));

        let r = search(&vault, &q(&["p"]), &q(&["stripe callback missing"]), 50).unwrap();
        // 2 of 3 terms is above the 60% bar; the decision has 1 of 3, a partial match.
        assert_eq!(r.hits.len(), 1);
        assert_eq!((r.hits[0].id.as_str(), r.hits[0].score), ("p-0001", 2));
        assert_eq!(r.partial_matches.len(), 1);
        assert_eq!(r.partial_matches[0].id, "p-D-0001");
        assert_eq!(r.partial_matches[0].matched, ["stripe"]);
    }

    #[test]
    fn short_terms_match_exactly() {
        let tokens: BTreeSet<String> = ["api", "apikey"].iter().map(|s| s.to_string()).collect();
        assert!(term_hits("api", &tokens));
        assert!(!term_hits("ap", &tokens));
        assert!(!term_hits("apiler", &tokens));
        assert!(term_hits("apike", &tokens));
    }
}
