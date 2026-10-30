//! Coding agent adapters. Each client has its own module: detection, connecting sbrain (hooks and
//! instructions that call the `sbrain` CLI), hook format, transcripts and its CLI runner.
//! The core does not know this crate; a new client is a new module and one line in `all()`.

pub mod claude;
pub mod codex;
mod digest;
mod error;
mod path;
mod process;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

pub use claude::install::runnable_sbrain;
pub use error::{Error, Result};
use path::{find_program, full_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientId {
    Claude,
    Codex,
}

impl ClientId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// A client's state on this machine.
#[derive(Debug, Clone, Serialize)]
pub struct Detection {
    pub client: ClientId,
    pub binary: Option<PathBuf>,
    pub version: Option<String>,
}

impl Detection {
    pub fn installed(&self) -> bool {
        self.binary.is_some()
    }
}

pub trait Client: Send + Sync {
    fn id(&self) -> ClientId;
    fn display_name(&self) -> &'static str;
    fn detect(&self) -> Detection;

    /// Model aliases the CLI documents. A full model name is always accepted as well.
    fn model_aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Effort levels the installed CLI accepts, lowest first, as the CLI names them; empty when it
    /// has no effort setting.
    fn effort_levels(&self) -> Vec<String> {
        Vec::new()
    }

    /// How sbrain is connected to this client; `None` when the client has no connector.
    fn connector(&self) -> Option<&dyn Connector> {
        None
    }

    /// Reading this client's transcripts and distilling with its CLI, for `sbrain sweep`; `None` when
    /// the client has no sweeper.
    fn sweeper(&self) -> Option<&dyn Sweeper> {
        None
    }
}

/// A session transcript the client keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// The client's session id.
    pub session: String,
    pub path: PathBuf,
    pub size: u64,
    /// Unix seconds of the last write.
    pub modified: i64,
}

/// What `sbrain sweep` needs of a transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Digest {
    /// The session's working directory, when the transcript names one.
    pub cwd: Option<String>,
    /// First and last activity, and the user's last message, `YYYY-MM-DD HH:MM` local time.
    pub first: String,
    pub last: String,
    pub last_user: String,
    /// Messages the user wrote (tool results and notifications are not counted).
    pub user_turns: usize,
    /// The session, condensed for distilling: the user's messages in full, the assistant's text,
    /// one line per tool call, errors and the results of commands like tests and commits.
    pub text: String,
    /// Every message the user wrote, in order, as `text` has them (secrets masked); kept whole even
    /// when `text` leaves out the middle of a long session.
    pub user_messages: Vec<UserMessage>,
}

/// One message the user wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserMessage {
    /// `YYYY-MM-DD HH:MM` local time.
    pub at: String,
    pub text: String,
}

/// One distilling run of the client's CLI: no tools, no hooks or user settings, no saved session;
/// the answer is JSON matching `schema`.
#[derive(Debug, Clone)]
pub struct DistillRun<'a> {
    pub system: &'a str,
    pub input: &'a str,
    pub schema: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// Where the CLI runs (`sweep::distill_dir`, outside the vault), so hooks recognise the distiller's own session.
    pub cwd: &'a Path,
    pub timeout: Duration,
}

pub trait Sweeper: Send + Sync {
    /// The transcripts on this machine, except those of sessions that ran in `skip` (the
    /// distiller's own folder).
    fn transcripts(&self, skip: &Path) -> Result<Vec<Transcript>>;
    fn digest(&self, path: &Path) -> Result<Digest>;
    /// Runs the CLI and returns its JSON answer.
    fn distill(&self, run: &DistillRun) -> Result<serde_json::Value>;
}

/// One change connecting makes, shown to the user before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub title: String,
    pub detail: String,
}

/// What `refresh` did to a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Up to date, or not connected.
    Unchanged,
    /// Hooks the connection was missing were added.
    Updated,
    /// The sbrain the hooks ran is gone; they now run `to`, the `sbrain` on PATH.
    Moved { from: PathBuf, to: PathBuf },
    /// The sbrain the hooks run is gone and the `sbrain` on PATH is not this one, so nothing was changed.
    Broken { sbrain: PathBuf },
}

pub trait Connector: Send + Sync {
    /// Exactly what `connect` will do, printed before `connect --yes` applies it. `sbrain` is the running CLI; the hooks
    /// run it through the `sbrain` on PATH when that is the same program.
    fn plan(&self, sbrain: &Path) -> Result<Vec<Change>>;
    fn is_connected(&self) -> Result<bool>;
    /// Connects, or points an existing connection at `sbrain`.
    fn connect(&self, sbrain: &Path) -> Result<()>;
    /// Keeps a connection working: adds the hooks it is missing, and moves one whose sbrain is gone to
    /// `sbrain` when PATH finds it.
    fn refresh(&self, sbrain: &Path) -> Result<Refresh>;
    /// Removes only what `connect` added.
    fn disconnect(&self) -> Result<()>;
    /// The sbrain the connection's hooks run; `None` when not connected.
    fn hooks_run(&self) -> Result<Option<PathBuf>> {
        Ok(None)
    }
}

pub fn all() -> Vec<Box<dyn Client>> {
    vec![Box::new(claude::Claude), Box::new(codex::Codex)]
}

pub fn get(id: &str) -> Option<Box<dyn Client>> {
    all().into_iter().find(|c| c.id().as_str() == id)
}

/// Finds `name` as the user's terminal would (see [`find_program`]) and reads the first line of
/// `<name> --version`.
fn detect_binary(client: ClientId, name: &str) -> Detection {
    let binary = find_program(name);
    let version = binary.as_deref().and_then(version_of);
    Detection {
        client,
        binary,
        version,
    }
}

/// The values listed in parentheses after `flag` in a CLI's help text, e.g.
/// `--effort <level>  Effort level … (low, medium, high)` → `["low", "medium", "high"]`.
/// The list may continue on the lines after the flag's own line.
pub fn help_choices(help: &str, flag: &str) -> Vec<String> {
    let Some(start) = help
        .lines()
        .position(|l| l.trim_start().starts_with(&format!("{flag} ")))
    else {
        return Vec::new();
    };
    // The flag's description: its own line and the indented lines that continue it.
    let mut text = String::new();
    for (k, line) in help.lines().skip(start).enumerate() {
        if k > 0 && line.trim_start().starts_with('-') {
            break;
        }
        text.push_str(line.trim());
        text.push(' ');
    }
    let Some(open) = text.rfind('(') else {
        return Vec::new();
    };
    let Some(close) = text[open..].find(')') else {
        return Vec::new();
    };
    text[open + 1..open + close]
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(str::to_owned)
        .collect()
}

/// `<bin> --help` on stdout, if it runs.
fn help_of(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--help").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn version_of(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (out.status.success() && !line.is_empty()).then(|| line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_from_help_text() {
        let help = "Options:\n  --effort <level>     Effort level for the current session\n                       (low, medium, high, xhigh, max)\n  --environment <id>   Create (a cloud session)\n";
        assert_eq!(
            help_choices(help, "--effort"),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(help_choices(help, "--missing").is_empty());
        assert!(
            help_choices("  --effort <l>  Effort level\n  --x  (a, b)\n", "--effort").is_empty(),
            "a list belonging to the next flag is not taken"
        );
    }
}
