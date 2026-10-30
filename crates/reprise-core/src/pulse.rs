//! What reprise last did in a session, in a few words, for a status line: each hook notes it in
//! `.reprise/cache/pulse/<session>.json`, and `reprise statusline` reads it back. Nothing leaves the
//! machine and nothing is computed when the status line is drawn.

use std::path::PathBuf;

use crate::Vault;
use crate::context::is_session_id;

/// Seconds after which what reprise last did is no longer news.
pub const FRESH_SECS: i64 = 600;

fn file(vault: &Vault, session: &str) -> Option<PathBuf> {
    is_session_id(session).then(|| {
        vault
            .cache_dir()
            .join("pulse")
            .join(format!("{session}.json"))
    })
}

/// Notes what reprise just did in `session`. Never fails: a status line is not worth an error.
pub fn note(vault: &Vault, session: &str, words: &str, now: i64) {
    let Some(path) = file(vault, session) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = crate::safe::create_dir_all(dir);
    }
    let text = serde_json::json!({"at": now, "words": words}).to_string();
    let _ = crate::safe::write(&path, &text);
}

/// What reprise last did in `session` and how many seconds ago.
pub fn last(vault: &Vault, session: &str, now: i64) -> Option<(String, i64)> {
    let text = crate::safe::read_to_string(file(vault, session)?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((v["words"].as_str()?.to_owned(), now - v["at"].as_i64()?))
}

/// The status line: `reprise · <project> · <what it last did>`; the last part only while it is fresh.
pub fn line(project: Option<&str>, last: Option<(String, i64)>) -> String {
    let mut out = format!("reprise · {}", project.unwrap_or("no project"));
    if let Some((words, age)) = last.filter(|(_, age)| *age <= FRESH_SECS) {
        let ago = if age < 60 {
            "just now".to_owned()
        } else {
            format!("{} min ago", age / 60)
        };
        out.push_str(&format!(" · {words} ({ago})"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_thing_done_while_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        assert_eq!(line(Some("app"), last(&vault, "s-1", 100)), "reprise · app");
        note(&vault, "s-1", "2 related records recalled", 100);
        assert_eq!(
            line(Some("app"), last(&vault, "s-1", 110)),
            "reprise · app · 2 related records recalled (just now)"
        );
        assert_eq!(
            line(Some("app"), last(&vault, "s-1", 400)),
            "reprise · app · 2 related records recalled (5 min ago)"
        );
        assert_eq!(
            line(None, last(&vault, "s-1", 2000)),
            "reprise · no project"
        );
        note(&vault, "../x", "no", 1);
        assert!(last(&vault, "../x", 1).is_none());
    }
}
