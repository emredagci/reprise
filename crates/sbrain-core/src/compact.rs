//! Keeping the user's own words through a compaction. When the agent compacts a conversation into a
//! summary, the summary can drop what the user asked for along the way ("from now on…", "don't touch
//! that folder"), and those words are not in the vault yet: the sweep distills a session after it ends.
//! Before the compaction the user's messages are saved word for word, secrets masked, to
//! `.sbrain/cache/compact/<session>.md`, which never leaves the machine; the session start that
//! follows the compaction hands them back to the agent.
//!
//! The block has a budget, so not every message fits: messages that read like instructions or
//! corrections go first, then the newest; they are shown in the order they were written. The
//! instructions among them also ride along with every prompt after the compaction, in a smaller
//! block, so they stay in front of the agent for the rest of the session.

use std::cmp::Reverse;
use std::path::PathBuf;

use crate::context::is_session_id;
use crate::text::{clip, tokens};
use crate::{Result, Vault};

/// Characters of the kept messages handed back after a compaction. With the session-start block
/// (`context::SESSION_START_BUDGET`) it stays under the 10,000 characters a hook may add.
pub const KEPT_BUDGET: usize = 3500;
/// Characters of the instructions added to every prompt after a compaction.
pub const INSTRUCTIONS_BUDGET: usize = 1500;
/// Characters kept of one message.
const MESSAGE_LIMIT: usize = 600;
/// A message shorter than this ("ok", "evet", "devam") says nothing on its own.
const MIN_CHARS: usize = 12;
/// Days a session's kept messages stay in the cache.
const KEEP_DAYS: u64 = 14;

/// Words that mark an instruction, a prohibition or a correction, English and Turkish, folded to
/// ASCII like `text::tokens`. Sorted.
const CUES: &[&str] = &[
    "always",
    "artik",
    "asla",
    "avoid",
    "dikkat",
    "don",
    "dont",
    "etme",
    "hayir",
    "instead",
    "kesinlikle",
    "kural",
    "must",
    "mutlaka",
    "never",
    "olmasin",
    "only",
    "prefer",
    "rule",
    "sadece",
    "sakin",
    "should",
    "stop",
    "unutma",
    "wrong",
    "yalniz",
    "yanlis",
    "yerine",
];
/// Phrases that mark an instruction, as runs of folded tokens.
const CUE_PHRASES: &[&str] = &[
    "from now on",
    "going forward",
    "bundan sonra",
    "her zaman",
    "bir daha",
];

/// Whether a message reads like an instruction or a correction. A Turkish negative imperative
/// (`yazma`, `ekleme`, `silme`) counts too; a noun that ends the same way only moves a message up.
pub fn is_directive(text: &str) -> bool {
    let words = tokens(text);
    let joined = format!(" {} ", words.join(" "));
    words.iter().any(|w| {
        CUES.binary_search(&w.as_str()).is_ok()
            || (w.len() >= 5 && (w.ends_with("ma") || w.ends_with("me")))
    }) || CUE_PHRASES
        .iter()
        .any(|p| joined.contains(&format!(" {p} ")))
}

fn file(vault: &Vault, session: &str) -> PathBuf {
    vault
        .cache_dir()
        .join("compact")
        .join(format!("{session}.md"))
}

fn instructions_file(vault: &Vault, session: &str) -> PathBuf {
    vault
        .cache_dir()
        .join("compact")
        .join(format!("{session}.instructions.md"))
}

/// Indexes of `candidates` that fit `budget`, instructions before the rest, then the newest; in the
/// order written. With `only_instructions` the rest are left out.
fn fit(candidates: &[(usize, bool, String)], budget: usize, only_instructions: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len())
        .filter(|&k| !only_instructions || candidates[k].1)
        .collect();
    order.sort_by_key(|&k| (Reverse(candidates[k].1), Reverse(candidates[k].0)));
    let mut chosen = Vec::new();
    let mut used = 0;
    for k in order {
        let len = candidates[k].2.chars().count() + 1;
        if used + len <= budget {
            used += len;
            chosen.push(k);
        }
    }
    chosen.sort_unstable();
    chosen
}

/// Saves the user's messages of `session` (time, text; in the order written) for the session start
/// after the compaction. Returns how many fit; 0 when none says anything, and the file is removed then.
pub fn keep(vault: &Vault, session: &str, messages: &[(&str, &str)]) -> Result<usize> {
    if !is_session_id(session) {
        return Ok(0);
    }
    // Index, whether it is an instruction, and its line.
    let candidates: Vec<(usize, bool, String)> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, (at, text))| {
            let text = text.trim();
            // Tool notices and slash commands come wrapped in tags or start with `/`.
            if text.chars().count() < MIN_CHARS || text.starts_with('<') || text.starts_with('/') {
                return None;
            }
            let text = clip(&crate::secret::redact(text), MESSAGE_LIMIT);
            let line = format!(
                "- [{at}] {}",
                text.replace("\r\n", "\n").replace('\n', "\n  ")
            );
            Some((i, is_directive(&text), line))
        })
        .collect();
    let chosen = fit(&candidates, KEPT_BUDGET, false);
    let instructions = fit(&candidates, INSTRUCTIONS_BUDGET, true);

    let path = file(vault, session);
    let dir = path.parent().expect("the file is in a folder");
    crate::safe::create_dir_all(dir)?;
    prune(dir);
    let instructions_path = instructions_file(vault, session);
    if instructions.is_empty() {
        let _ = std::fs::remove_file(&instructions_path);
    } else {
        let lines: Vec<&str> = instructions
            .iter()
            .map(|k| candidates[*k].2.as_str())
            .collect();
        crate::safe::write(&instructions_path, &format!("{}\n", lines.join("\n")))?;
    }
    if chosen.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(0);
    }
    let left = candidates.len() - chosen.len();
    let mut text = String::new();
    if left > 0 {
        text.push_str(&format!(
            "({left} more messages did not fit; instructions and the newest were kept first.)\n\n"
        ));
    }
    for k in &chosen {
        text.push_str(&candidates[*k].2);
        text.push('\n');
    }
    crate::safe::write(&path, &text)?;
    Ok(chosen.len())
}

/// The block for the session start after `session` was compacted; `None` when nothing was kept.
pub fn kept_block(vault: &Vault, session: &str) -> Option<String> {
    if !is_session_id(session) {
        return None;
    }
    let text = crate::safe::read_to_string(file(vault, session)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(format!(
        "## Kept through compaction\n\nThis conversation was just compacted. These are the user's own messages from \
         before it, word for word (secrets masked). What they ask still holds unless the user changed it later; \
         the summary may have dropped it.\n\n{text}"
    ))
}

/// The block added to every prompt of `session` after it was compacted: the user's instructions from
/// before it; `None` when there were none or it was never compacted.
pub fn instructions_block(vault: &Vault, session: &str) -> Option<String> {
    if !is_session_id(session) {
        return None;
    }
    let text = crate::safe::read_to_string(instructions_file(vault, session)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| {
        format!(
            "Still in effect from before this conversation was compacted (the user's words):\n{text}"
        )
    })
}

/// Removes kept messages older than `KEEP_DAYS`.
fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let limit = std::time::Duration::from_secs(KEEP_DAYS * 24 * 3600);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > limit);
        if old && entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_sorted_ascii() {
        assert!(CUES.windows(2).all(|w| w[0] < w[1]));
        assert!(
            CUES.iter()
                .all(|c| c.chars().all(|ch| ch.is_ascii_lowercase()))
        );
    }

    #[test]
    fn directives_in_both_languages() {
        for text in [
            "From now on, use pnpm.",
            "Never touch the migrations folder",
            "bundan sonra commit mesajları İngilizce olsun",
            "migration yazma, gerek yok",
            "README'ye uzun anlatım ekleme",
            "Hayır, öyle değil",
            "push etme",
        ] {
            assert!(is_directive(text), "{text}");
        }
        for text in ["what does this function return?", "tests pass now, nice"] {
            assert!(!is_directive(text), "{text}");
        }
    }

    #[test]
    fn keeps_instructions_first_within_the_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        let long = "x".repeat(MESSAGE_LIMIT);
        let key = format!("ghp_{}", "q1W2".repeat(9));
        let mut messages: Vec<(String, String)> = vec![
            ("10:00".into(), "bundan sonra migration yazma".into()),
            ("10:01".into(), "ok".into()),
            ("10:02".into(), "<command-name>/model</command-name>".into()),
            ("10:03".into(), format!("deploy with token {key}")),
        ];
        for i in 0..10 {
            messages.push((format!("11:{i:02}"), format!("{i} {long}")));
        }
        let refs: Vec<(&str, &str)> = messages
            .iter()
            .map(|(a, t)| (a.as_str(), t.as_str()))
            .collect();
        let kept = keep(&vault, "s-1", &refs).unwrap();
        let block = kept_block(&vault, "s-1").unwrap();
        assert!(
            block.chars().count() <= KEPT_BUDGET + 400,
            "{}",
            block.len()
        );
        assert!(
            block.contains("- [10:00] bundan sonra migration yazma"),
            "{block}"
        );
        assert!(
            !block.contains("[10:01]") && !block.contains("[10:02]"),
            "{block}"
        );
        assert!(!block.contains(&key), "{block}");
        assert!(block.contains("[11:09]"), "the newest is kept: {block}");
        assert!(
            !block.contains("[11:00]"),
            "the oldest long one is not: {block}"
        );
        assert!(block.contains("more messages did not fit"), "{block}");
        let first = block.find("[10:00]").unwrap();
        assert!(
            first < block.find("[11:09]").unwrap(),
            "in the order written"
        );
        assert!(kept >= 2);

        let every = instructions_block(&vault, "s-1").unwrap();
        assert!(every.contains("bundan sonra migration yazma"), "{every}");
        assert!(
            !every.contains("[11:09]"),
            "only instructions ride along: {every}"
        );
        assert!(every.chars().count() <= INSTRUCTIONS_BUDGET + 100);

        assert_eq!(keep(&vault, "s-1", &[("10:00", "ok")]).unwrap(), 0);
        assert!(instructions_block(&vault, "s-1").is_none());
        assert!(
            kept_block(&vault, "s-1").is_none(),
            "nothing left to hand back"
        );
        assert!(kept_block(&vault, "../x").is_none());
    }
}
