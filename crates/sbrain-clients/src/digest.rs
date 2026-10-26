//! What every client's transcript digest shares: the user's messages under `## User` headings, other
//! text quoted so it cannot pass for the user, and a budget that shortens tool lines first.

use chrono::{DateTime, Local};

use crate::Digest;

/// Characters a digest may have; past it tool lines, then the assistant's text are shortened, and at
/// last the middle of the session is left out.
pub(crate) const BUDGET: usize = 120_000;
/// Words in a shell command whose result is kept in the digest.
const SIGNAL_WORDS: [&str; 10] = [
    "git commit",
    "git push",
    "test",
    "pytest",
    "jest",
    "vitest",
    "verify",
    "build",
    "deploy",
    "migrat",
];

/// Whether a shell command's result belongs in the digest: commits, tests, builds.
pub(crate) fn is_signal(command: &str) -> bool {
    let command = command.to_lowercase();
    SIGNAL_WORDS.iter().any(|w| command.contains(w))
}

/// A digest being built: lines with their weight, 0 the user's words (kept), 1 the assistant's text,
/// 2 tool calls and results (shortened first).
#[derive(Default)]
pub(crate) struct Builder {
    pub digest: Digest,
    lines: Vec<(u8, String)>,
}

impl Builder {
    /// Notes the time of an entry, `YYYY-MM-DD HH:MM` local.
    pub fn at(&mut self, ts: &str) {
        if ts.is_empty() {
            return;
        }
        if self.digest.first.is_empty() {
            self.digest.first = ts.to_owned();
        }
        self.digest.last = ts.to_owned();
    }

    pub fn cwd(&mut self, cwd: Option<&str>) {
        if self.digest.cwd.is_none() {
            self.digest.cwd = cwd.filter(|c| !c.is_empty()).map(str::to_owned);
        }
    }

    /// A message the user wrote; notifications the agent shows as the user's are kept as tool lines.
    pub fn user(&mut self, ts: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if text.starts_with("<task-notification>") || text.starts_with("<local-command") {
            self.tool(format!("[notification {ts}] {}", clip(text, 200)));
            return;
        }
        self.digest.user_turns += 1;
        self.digest.last_user = ts.to_owned();
        self.lines
            .push((0, format!("\n## User [{ts}]\n{}", clip(text, 4000))));
    }

    pub fn assistant(&mut self, text: &str) {
        let text = text.trim();
        if !text.is_empty() {
            self.lines
                .push((1, quoted(&format!("Assistant: {}", clip(text, 2500)))));
        }
    }

    /// A tool call or its result.
    pub fn tool(&mut self, line: String) {
        self.lines.push((2, quoted(&line)));
    }

    pub fn finish(mut self) -> Digest {
        self.digest.text = self.fit();
        self.digest
    }

    fn render(&self) -> String {
        self.lines
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Shortens tool lines, then the assistant's text; past that keeps the start and the end.
    fn fit(&mut self) -> String {
        let text = self.render();
        if text.chars().count() <= BUDGET {
            return text;
        }
        for (weight, t) in &mut self.lines {
            *t = match weight {
                2 => clip(t, 100),
                1 => clip(t, 700),
                _ => std::mem::take(t),
            };
        }
        let text = self.render();
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= BUDGET {
            return text;
        }
        let head: String = chars[..BUDGET / 4].iter().collect();
        let tail: String = chars[chars.len() - BUDGET * 3 / 4..].iter().collect();
        format!("{head}\n\n…[the middle of the session is left out]…\n\n{tail}")
    }
}

/// Text that is not the user's own (tool output, the assistant's words, notifications), indented on
/// every line after the first: a line of it can then never start with `## User`, the heading only a
/// user's message gets, so it cannot pass for the user when the distiller looks for corrections.
fn quoted(text: &str) -> String {
    text.replace("\r\n", "\n").replace(['\n', '\r'], "\n    ")
}

/// An RFC 3339 time (UTC in transcripts) as local `YYYY-MM-DD HH:MM`; empty when it does not parse.
pub(crate) fn local_time(ts: &str) -> String {
    DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub(crate) fn clip(text: &str, limit: usize) -> String {
    sbrain_core::text::clip(text, limit)
}

/// Unix seconds of a file's last write; 0 when unknown.
pub(crate) fn modified(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
