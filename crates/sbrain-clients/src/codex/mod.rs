//! Codex adapter: detection, transcripts and distilling with `codex exec`. It has no connector, so
//! `sbrain connect` does not support Codex.

pub mod runner;
pub mod transcript;

use std::path::{Path, PathBuf};

use crate::{
    Client, ClientId, Detection, Digest, DistillRun, Error, Result, Sweeper, Transcript,
    detect_binary,
};

pub struct Codex;

impl Client for Codex {
    fn id(&self) -> ClientId {
        ClientId::Codex
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn detect(&self) -> Detection {
        detect_binary(ClientId::Codex, "codex")
    }

    /// Codex's `model_reasoning_effort` setting; `xhigh` only some models take.
    fn effort_levels(&self) -> Vec<String> {
        ["minimal", "low", "medium", "high", "xhigh"]
            .map(String::from)
            .to_vec()
    }

    fn sweeper(&self) -> Option<&dyn Sweeper> {
        Some(self)
    }
}

/// Codex's folder: `CODEX_HOME`, or `~/.codex`.
pub fn home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|h| h.join(".codex"))
        .ok_or_else(|| Error::Engine("no home folder to find Codex's sessions in".into()))
}

impl Sweeper for Codex {
    fn transcripts(&self, skip: &Path) -> Result<Vec<Transcript>> {
        transcript::list(&home()?, skip)
    }

    fn digest(&self, path: &Path) -> Result<Digest> {
        transcript::digest(path)
    }

    /// `SBRAIN_CODEX_BIN` replaces the installed `codex`, so the sweep can be tried without a model.
    fn distill(&self, run: &DistillRun) -> Result<serde_json::Value> {
        let bin = sbrain_core::env_var("CODEX_BIN")
            .map(PathBuf::from)
            .or_else(|| self.detect().binary)
            .ok_or_else(|| Error::Engine("codex is not installed or not on PATH".into()))?;
        runner::distill(&bin, run)
    }
}
