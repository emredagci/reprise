//! `.reprise/vault.toml`: the vault's format version and settings. Only the fields the core reads are here;
//! unknown fields are ignored.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The vault format this binary reads and writes.
pub const FORMAT: u32 = 2;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub format: u32,
    #[serde(default)]
    pub projects: ProjectsConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub sweep: SweepConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            format: FORMAT,
            projects: ProjectsConfig::default(),
            hooks: HooksConfig::default(),
            sweep: SweepConfig::default(),
        }
    }
}

/// `[sweep]`: which transcripts `reprise sweep` distills and how.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SweepConfig {
    /// A transcript is distilled once it has not been written for this many minutes.
    pub settle_minutes: u64,
    /// At most this many sessions are distilled in one run.
    pub max_per_run: usize,
    /// Sessions with fewer messages from the user are skipped.
    pub min_user_turns: usize,
    /// Seconds one distilling run of the engine may take.
    pub timeout_seconds: u64,
    /// `host:port` tried before distilling; without a connection nothing is attempted. Empty skips
    /// the check.
    pub online_check: String,
    pub online_timeout_seconds: u64,
    /// Sessions whose working directory starts with one of these (tests, scratch folders) are skipped.
    pub skip_cwd_prefixes: Vec<String>,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            settle_minutes: 60,
            max_per_run: 4,
            min_user_turns: 2,
            timeout_seconds: 900,
            online_check: "api.anthropic.com:443".into(),
            online_timeout_seconds: 5,
            skip_cwd_prefixes: [
                "/tmp/",
                "/private/tmp/",
                "/var/folders/",
                "/private/var/folders/",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ProjectsConfig {
    /// Folders that hold unrelated sub-projects: the project name is the next segment.
    #[serde(default)]
    pub containers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HooksConfig {
    /// Model name fragment → context window in tokens; `default` is the fallback.
    #[serde(default)]
    pub context_windows: BTreeMap<String, u64>,
    /// Context usage percentages at which a handoff is suggested, once each.
    #[serde(default = "default_thresholds")]
    pub context_thresholds: Vec<u64>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            context_windows: BTreeMap::new(),
            context_thresholds: default_thresholds(),
        }
    }
}

fn default_thresholds() -> Vec<u64> {
    vec![70, 85]
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

impl HooksConfig {
    /// The window of the longest model fragment contained in `model`, else `default` (200k). A usage
    /// above the window means the config is outdated, so at least 1M is assumed.
    pub fn context_window(&self, model: &str, used: u64) -> u64 {
        let window = self
            .context_windows
            .iter()
            .filter(|(k, _)| k.as_str() != "default" && model.contains(k.as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, v)| *v)
            .or_else(|| self.context_windows.get("default").copied())
            .unwrap_or(200_000);
        if used > window {
            window.max(1_000_000)
        } else {
            window
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_uses_longest_fragment_and_default() {
        let cfg = Config::parse(
            "format = 2\n[hooks.context_windows]\nson = 5\nsonnet = 1000000\ndefault = 200000\n",
        )
        .unwrap();
        assert_eq!(cfg.hooks.context_window("claude-sonnet-5", 1), 1_000_000);
        assert_eq!(cfg.hooks.context_window("claude-son", 1), 5);
        assert_eq!(cfg.hooks.context_window("claude-haiku", 10), 200_000);
        assert_eq!(cfg.hooks.context_window("claude-haiku", 300_000), 1_000_000);
        assert_eq!(cfg.hooks.context_thresholds, [70, 85]);
        assert_eq!(Config::default().hooks.context_window("x", 1), 200_000);
    }

    #[test]
    fn template_parses() {
        let cfg = Config::parse(crate::scaffold::VAULT_TOML).unwrap();
        assert_eq!(cfg.format, FORMAT);
        assert!(cfg.projects.containers.is_empty());
        assert_eq!(cfg.hooks.context_window("claude-opus-5", 1), 1_000_000);
        assert_eq!(cfg.sweep.settle_minutes, 60);
        assert_eq!(cfg.sweep.max_per_run, 4);
    }

    #[test]
    fn sweep_settings_fall_back_to_defaults() {
        let cfg = Config::parse("format = 2\n[sweep]\nmax_per_run = 1\n").unwrap();
        assert_eq!(cfg.sweep.max_per_run, 1);
        assert_eq!(cfg.sweep.settle_minutes, 60);
        assert!(cfg.sweep.skip_cwd_prefixes.contains(&"/tmp/".to_owned()));
    }

    #[test]
    fn format_is_required() {
        assert!(Config::parse("[hooks]\n").is_err());
    }
}
