//! Claude Code hooks: SessionStart and UserPromptSubmit read a JSON payload on stdin and answer with
//! `additionalContext` (a session start after a compaction also gets the user's messages PreCompact
//! kept, `compact`); PreCompact and SessionEnd answer nothing and notes the session as ended, so the sweep distills it
//! without waiting for it to settle; PostToolUse masks secrets in a file the agent just wrote to the
//! vault and checks the record, tells the agent what was masked or what to fix, and commits a file that
//! is fine.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use sbrain_core::context::{is_distiller_session, related_records, session_start, usage_notice};
use sbrain_core::{Access, Vault};
use serde_json::Value;

/// How much of the transcript's end is read to find the latest context size.
const TRANSCRIPT_TAIL: u64 = 400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    SessionStart,
    Prompt,
    SessionEnd,
    ToolUse,
    PreCompact,
}

impl Event {
    fn hook_name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::Prompt => "UserPromptSubmit",
            Self::SessionEnd => "SessionEnd",
            Self::ToolUse => "PostToolUse",
            Self::PreCompact => "PreCompact",
        }
    }
}

/// The line to print for this hook call, or `None` when there is nothing to add.
pub fn run(
    event: Event,
    payload: &str,
    vault: &Vault,
    access: &Access,
    today: NaiveDate,
) -> sbrain_core::Result<Option<String>> {
    let payload: Value = serde_json::from_str(if payload.trim().is_empty() {
        "{}"
    } else {
        payload
    })
    .map_err(|e| sbrain_core::Error::Io {
        path: PathBuf::from("<stdin>"),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let cwd = match field("cwd") {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir().map_err(|source| sbrain_core::Error::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    if is_distiller_session(&cwd) {
        return Ok(None);
    }

    let text = match event {
        Event::ToolUse => {
            let written = payload
                .get("tool_input")
                .and_then(|i| i.get("file_path"))
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let Some(written) = written else {
                return Ok(None);
            };
            let written = written.canonicalize().unwrap_or(written);
            let rel = written
                .strip_prefix(vault.root())
                .unwrap_or(&written)
                .display()
                .to_string();
            // Masked before anything is committed: the vault is pushed to a remote.
            let masked = sbrain_core::doctor::mask_secrets(vault, &written).unwrap_or_default();
            let problems = sbrain_core::doctor::record_problems(vault, &written);
            if problems.is_empty() {
                commit_edit(vault, &written);
            }
            let mut notes = Vec::new();
            if !masked.is_empty() {
                notes.push(format!(
                    "sbrain: {rel} held what looks like a secret ({}); it was replaced with {}. \
                     Never write keys, tokens or passwords into the vault; tell the user to rotate it if it was real.",
                    masked.join(", "),
                    sbrain_core::secret::MASK
                ));
            }
            if !problems.is_empty() {
                notes.push(format!(
                    "sbrain: {rel} does not follow the vault format. Fix it now:\n- {}",
                    problems.join("\n- ")
                ));
            }
            if notes.is_empty() {
                return Ok(None);
            }
            let reason = notes.join("\n\n");
            // `block` after a tool ran stops nothing: Claude Code hands the reason to the agent.
            let s = |v: &str| serde_json::to_string(v).expect("strings always serialize");
            return Ok(Some(format!(
                "{{\"decision\": \"block\", \"reason\": {}}}",
                s(&reason)
            )));
        }
        Event::PreCompact => {
            if let (Some(session), Some(transcript)) =
                (field("session_id"), field("transcript_path"))
            {
                let path = Path::new(transcript);
                let digest = super::transcript::digest(path).map_err(|e| match e {
                    crate::Error::Core(e) => e,
                    other => sbrain_core::Error::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::other(other.to_string()),
                    },
                })?;
                let messages: Vec<(&str, &str)> = digest
                    .user_messages
                    .iter()
                    .map(|m| (m.at.as_str(), m.text.as_str()))
                    .collect();
                sbrain_core::compact::keep(vault, session, &messages)?;
            }
            None
        }
        Event::SessionEnd => {
            if let Some(session) = field("session_id") {
                sbrain_core::sweep::mark_ended(vault, session, chrono::Local::now().timestamp())?;
            }
            None
        }
        Event::SessionStart => {
            refresh_staleness(vault);
            let mut text = session_start(vault, access, &cwd, field("session_id"), today)?;
            if field("source") == Some("compact")
                && let Some(kept) =
                    field("session_id").and_then(|s| sbrain_core::compact::kept_block(vault, s))
            {
                text.push_str("\n\n");
                text.push_str(&kept);
            }
            Some(text)
        }
        Event::Prompt => {
            let mut parts = Vec::new();
            let session = field("session_id").unwrap_or("");
            if let Some(instructions) = sbrain_core::compact::instructions_block(vault, session) {
                parts.push(instructions);
            }
            let usage = field("transcript_path").and_then(|t| context_usage(Path::new(t)));
            if let (Some((used, model)), false) = (usage, session.is_empty())
                && let Some(notice) = usage_notice(vault, session, used, &model)?
            {
                parts.push(notice);
            }
            if let Some(related) =
                related_records(vault, access, &cwd, field("prompt").unwrap_or(""))?
            {
                parts.push(related);
            }
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
    };
    Ok(text
        .filter(|t| !t.is_empty())
        .map(|t| output_line(event, &t)))
}

/// Has `sbrain doctor` check staleness again in the background when what it last found is older than
/// `stale::CACHE_HOURS` (or missing): checking takes git per file, too slow for session start, which reads
/// the result instead. The doctor runs detached, in a process group of its own, with nothing
/// attached to the hook's output.
fn refresh_staleness(vault: &Vault) {
    let now = chrono::Local::now().naive_local();
    if sbrain_core::stale::Found::load(vault).is_some_and(|f| !f.is_old(now)) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--vault")
        .arg(vault.root())
        .args(["doctor", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    // Not waited for; it is the next session start that reads what it finds.
    let _ = cmd.spawn();
}

/// Commits a file the agent edited in the vault, as sbrain's own writes are; a commit git refuses waits
/// in the job ledger. Not the machine's `.sbrain/` folder, and not a record that is still wrong: that one
/// is committed with the edit that fixes it.
fn commit_edit(vault: &Vault, path: &Path) {
    let Ok(rel) = path.strip_prefix(vault.root()) else {
        return;
    };
    if rel.starts_with(".sbrain") || rel.starts_with(".git") {
        return;
    }
    // A link could put a file outside the vault into its history.
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return;
    }
    let mut parts = rel.components().filter_map(|c| c.as_os_str().to_str());
    let scope = match (parts.next(), parts.next()) {
        (Some("projects"), Some(project)) => project.to_owned(),
        (Some(top), _) => top.trim_end_matches(".md").to_owned(),
        _ => return,
    };
    sbrain_core::git::commit(
        vault.root(),
        &[rel.to_path_buf()],
        &format!("edit({scope}): {} by the agent", rel.display()),
    );
}

/// The hook's JSON answer on one line, non-ASCII text kept as it is.
fn output_line(event: Event, text: &str) -> String {
    let s = |v: &str| serde_json::to_string(v).expect("strings always serialize");
    format!(
        "{{\"hookSpecificOutput\": {{\"hookEventName\": {}, \"additionalContext\": {}}}}}",
        s(event.hook_name()),
        s(text)
    )
}

/// Context size (input + cache read + cache creation tokens) and model of the latest main-chain
/// assistant message in the transcript.
pub fn context_usage(transcript: &Path) -> Option<(u64, String)> {
    let mut file = std::fs::File::open(transcript).ok()?;
    let len = file.seek(SeekFrom::End(0)).ok()?;
    file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let tail = String::from_utf8_lossy(&bytes);
    for line in tail.lines().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = entry.get("message").filter(|m| m.is_object());
        let usage = message
            .and_then(|m| m.get("usage"))
            .filter(|u| u.as_object().is_some_and(|o| !o.is_empty()));
        let sidechain = entry["isSidechain"].as_bool() == Some(true);
        if entry.get("type").and_then(Value::as_str) == Some("assistant")
            && !sidechain
            && let Some(usage) = usage
        {
            let used = [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .iter()
            .map(|k| usage.get(*k).and_then(Value::as_u64).unwrap_or(0))
            .sum();
            let model = message
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            return Some((used, model));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_comes_from_the_last_main_chain_assistant_message() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("t.jsonl");
        std::fs::write(
            &t,
            [
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","usage":{"input_tokens":10,"cache_read_input_tokens":5}}}"#,
                r#"{"type":"assistant","isSidechain":true,"message":{"model":"x","usage":{"input_tokens":999}}}"#,
                r#"{"type":"user","message":{"content":"hi"}}"#,
                "not json",
            ]
            .join("\n"),
        )
        .unwrap();
        assert_eq!(context_usage(&t), Some((15, "claude-sonnet-5".into())));
        assert_eq!(context_usage(&tmp.path().join("missing")), None);
    }

    #[test]
    fn output_is_one_line_with_raw_unicode() {
        assert_eq!(
            output_line(Event::Prompt, "a \"b\"\nç"),
            "{\"hookSpecificOutput\": {\"hookEventName\": \"UserPromptSubmit\", \"additionalContext\": \"a \\\"b\\\"\\nç\"}}"
        );
    }
}
