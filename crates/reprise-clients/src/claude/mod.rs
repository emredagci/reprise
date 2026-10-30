//! Claude Code adapter.

pub mod hooks;
pub mod install;
pub mod runner;
pub mod transcript;

use std::path::Path;

use crate::{
    Change, Client, ClientId, Connector, Detection, Digest, DistillRun, Error, Refresh, Result,
    Sweeper, Transcript, detect_binary,
};

pub struct Claude;

impl Client for Claude {
    fn id(&self) -> ClientId {
        ClientId::Claude
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn detect(&self) -> Detection {
        detect_binary(ClientId::Claude, "claude")
    }

    /// `claude --help` names 'fable', 'opus' and 'sonnet' as examples of model aliases; 'haiku' is
    /// an alias as well.
    fn model_aliases(&self) -> &'static [&'static str] {
        &["fable", "opus", "sonnet", "haiku"]
    }

    /// Read from the installed CLI's `--help` (`--effort <level> … (low, medium, …)`), so the list is
    /// whatever this version supports. Without a readable help text, the list below is used.
    fn effort_levels(&self) -> Vec<String> {
        let from_help = self
            .detect()
            .binary
            .as_deref()
            .and_then(crate::help_of)
            .map(|help| crate::help_choices(&help, "--effort"))
            .unwrap_or_default();
        if from_help.is_empty() {
            ["low", "medium", "high", "xhigh", "max"]
                .map(String::from)
                .to_vec()
        } else {
            from_help
        }
    }

    fn connector(&self) -> Option<&dyn Connector> {
        Some(self)
    }

    fn sweeper(&self) -> Option<&dyn Sweeper> {
        Some(self)
    }
}

impl Sweeper for Claude {
    fn transcripts(&self, skip: &Path) -> Result<Vec<Transcript>> {
        transcript::list(&install::Targets::from_env()?.claude_dir, skip)
    }

    fn digest(&self, path: &Path) -> Result<Digest> {
        transcript::digest(path)
    }

    /// `REPRISE_CLAUDE_BIN` replaces the installed `claude`, so the sweep can be tried without a model.
    fn distill(&self, run: &DistillRun) -> Result<serde_json::Value> {
        let bin = reprise_core::env_var("CLAUDE_BIN")
            .map(std::path::PathBuf::from)
            .or_else(|| self.detect().binary)
            .ok_or_else(|| Error::Engine("claude is not installed or not on PATH".into()))?;
        runner::distill(&bin, run)
    }
}

impl Connector for Claude {
    fn plan(&self, reprise: &Path) -> Result<Vec<Change>> {
        install::plan(&install::Targets::from_env()?, reprise)
    }

    fn is_connected(&self) -> Result<bool> {
        Ok(install::Targets::from_env()?.manifest.exists())
    }

    fn connect(&self, reprise: &Path) -> Result<()> {
        install::connect(&install::Targets::from_env()?, reprise)
    }

    fn refresh(&self, reprise: &Path) -> Result<Refresh> {
        install::refresh(&install::Targets::from_env()?, reprise)
    }

    fn disconnect(&self) -> Result<()> {
        install::disconnect(&install::Targets::from_env()?)
    }

    fn hooks_run(&self) -> Result<Option<std::path::PathBuf>> {
        install::hooks_run(&install::Targets::from_env()?)
    }
}
