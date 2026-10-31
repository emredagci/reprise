//! What the sweep keeps of each session it distills, on this machine only
//! (`.reprise/cache/sessions/<session>.json`): the digest (secrets already masked), the user's messages,
//! and the engine's answer. Agents delete old transcripts (Claude Code after a month by default); this
//! copy outlives them. With the answer kept, the records of a session can be written again without
//! calling the engine (`reprise sweep replay`), and what the engine said can be checked later.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::is_session_id;
use crate::{Error, Result, Vault};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    pub session: String,
    pub project: String,
    pub cwd: String,
    /// First and last activity, `YYYY-MM-DD HH:MM`.
    pub first: String,
    pub last: String,
    pub digest: String,
    pub user_text: String,
    /// The engine's answer, once there is one.
    #[serde(default)]
    pub answer: Option<serde_json::Value>,
}

fn path(vault: &Vault, session: &str) -> Result<PathBuf> {
    if !is_session_id(session) {
        return Err(Error::InvalidChange(format!(
            "{session:?} is not a session id"
        )));
    }
    Ok(vault
        .cache_dir()
        .join("sessions")
        .join(format!("{session}.json")))
}

/// Saves `kept`; its answer is masked like everything else that is written.
pub fn save(vault: &Vault, kept: &Kept) -> Result<()> {
    let path = path(vault, &kept.session)?;
    crate::safe::create_dir_all(path.parent().expect("in a folder"))?;
    let text = serde_json::to_string_pretty(kept).expect("serializable");
    crate::safe::write(&path, &crate::secret::redact(&text))
}

/// What was kept of `session`, if anything.
pub fn load(vault: &Vault, session: &str) -> Option<Kept> {
    let text = crate::safe::read_to_string(path(vault, session).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_sessions_round_trip_masked() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        let key = format!("ghp_{}", "aB3".repeat(12));
        let mut kept = Kept {
            session: "s-1".into(),
            project: "app".into(),
            cwd: "/w".into(),
            first: "2026-10-30 09:00".into(),
            last: "2026-10-30 10:00".into(),
            digest: "## User [2026-10-30 09:00]\nhi".into(),
            user_text: "hi".into(),
            answer: None,
        };
        save(&vault, &kept).unwrap();
        assert_eq!(load(&vault, "s-1").unwrap(), kept);
        kept.answer = Some(serde_json::json!({"card": {"done": [format!("used {key}")]}}));
        save(&vault, &kept).unwrap();
        let back = load(&vault, "s-1").unwrap();
        assert!(!back.answer.unwrap().to_string().contains(&key));
        assert!(load(&vault, "missing").is_none());
        assert!(
            save(
                &vault,
                &Kept {
                    session: "../x".into(),
                    ..kept
                }
            )
            .is_err()
        );
    }
}
