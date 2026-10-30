//! Per-machine user settings: where the vault is and which folders reprise may access.
//! Stored outside the vault, in the OS settings directory (macOS: ~/Library/Application Support/reprise),
//! because the vault's location cannot be read from inside the vault.
//! No path is assumed; until the user chooses, nothing is configured.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::canonical;
use crate::{Access, Error, Result, Vault};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
    /// The coding CLI (and model) that runs reprise's own work, such as distilling sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
    /// How often the sweep runs on this computer (`reprise sweep --due`, started by `reprise sweep schedule`).
    #[serde(default, skip_serializing_if = "SweepSettings::is_default")]
    pub sweep: SweepSettings,
    /// The embeddings service meaning search uses; off when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<crate::vectors::Embeddings>,
}

/// The sweep's schedule. Per machine: another computer on the same vault keeps its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepSettings {
    /// Hours between runs; 0 runs it only by hand.
    #[serde(default = "SweepSettings::default_every")]
    pub every_hours: u32,
    /// Sessions distilled in one run; more waiting ones follow a few minutes later.
    #[serde(default = "SweepSettings::default_per_run")]
    pub per_run: usize,
}

impl SweepSettings {
    /// The intervals offered, in hours; 0 is by hand.
    pub const EVERY_HOURS: [u32; 6] = [1, 3, 6, 12, 24, 0];
    pub const PER_RUN: [usize; 3] = [2, 4, 8];

    fn default_every() -> u32 {
        1
    }

    fn default_per_run() -> usize {
        4
    }

    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The nearest offered values, so a hand-edited file cannot ask for a run every minute.
    pub fn normalized(self) -> Self {
        let every_hours = if Self::EVERY_HOURS.contains(&self.every_hours) {
            self.every_hours
        } else {
            Self::default_every()
        };
        let per_run = *Self::PER_RUN
            .iter()
            .min_by_key(|n| n.abs_diff(self.per_run))
            .expect("not empty");
        Self {
            every_hours,
            per_run,
        }
    }
}

impl Default for SweepSettings {
    fn default() -> Self {
        Self {
            every_hours: Self::default_every(),
            per_run: Self::default_per_run(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Engine {
    /// Client id, e.g. `claude`.
    pub client: String,
    /// Model alias or full name passed to the CLI; `None` uses the CLI's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Effort level passed to the CLI (one the client lists); `None` uses the CLI's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Engine {
    /// Trims the values and treats an empty model or effort as the CLI default.
    pub fn normalized(&self) -> Self {
        let clean = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
        };
        Self {
            client: self.client.trim().to_owned(),
            model: clean(&self.model),
            effort: clean(&self.effort),
        }
    }
}

impl Settings {
    /// Settings file path. `REPRISE_SETTINGS` overrides it.
    pub fn default_path() -> Result<PathBuf> {
        if let Some(path) = crate::env_var("SETTINGS") {
            return Ok(PathBuf::from(path));
        }
        let base = dirs::config_dir().ok_or(Error::NoConfigDir)?;
        Ok(base.join("reprise/settings.json"))
    }

    pub fn access(&self) -> Result<&Access> {
        self.access.as_ref().ok_or(Error::AccessNotConfigured)
    }

    /// Validates the vault and access choices together and applies them (write with `save`).
    /// Everything is checked before an empty vault folder is initialized; on error nothing changes.
    pub fn configure(&mut self, vault: &Path, access: &Access) -> Result<Vault> {
        Vault::inspect(vault)?;
        let access = access.validated(&canonical(vault)?)?;
        let vault = Vault::choose(vault)?;
        self.vault = Some(vault.root().to_path_buf());
        self.access = Some(access);
        Ok(vault)
    }

    /// Changes only the access choice; the vault must already be set.
    pub fn set_access(&mut self, access: &Access) -> Result<()> {
        let vault = self.vault.as_deref().ok_or(Error::VaultNotConfigured)?;
        self.access = Some(access.validated(&canonical(vault)?)?);
        Ok(())
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::default_path()?)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::default_path()?)
    }

    /// A missing file means nothing is configured. An invalid file is an error: silently resetting
    /// it would lose the user's choices.
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        serde_json::from_str(&text).map_err(|source| Error::SettingsInvalid {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Writes to a temporary file and renames it, so a half-written settings file never remains.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let io = |source| Error::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut text = serde_json::to_string_pretty(self).expect("Settings always serializes");
        text.push('\n');
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccessMode;

    fn dirs(root: &Path, names: &[&str]) {
        for n in names {
            std::fs::create_dir_all(root.join(n)).unwrap();
        }
    }

    fn tmp_root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        (tmp, root)
    }

    #[test]
    fn missing_file_is_unconfigured() {
        let (_t, root) = tmp_root();
        let s = Settings::load_from(&root.join("missing.json")).unwrap();
        assert_eq!(s, Settings::default());
        assert!(matches!(s.access(), Err(Error::AccessNotConfigured)));
    }

    #[test]
    fn save_and_load_round_trip_creates_parent() {
        let (_t, root) = tmp_root();
        let path = root.join("reprise/settings.json");
        let s = Settings {
            vault: Some(root.join("Vault")),
            access: Some(Access::manual([root.join("Code")])),
            engine: Some(Engine {
                client: "claude".into(),
                model: Some("sonnet".into()),
                effort: Some("high".into()),
            }),
            sweep: SweepSettings {
                every_hours: 6,
                per_run: 8,
            },
            embeddings: Some(crate::vectors::Embeddings {
                url: "http://localhost:11434/v1/embeddings".into(),
                model: "nomic-embed-text".into(),
                key_env: None,
            }),
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), s);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn invalid_file_is_an_error_not_a_reset() {
        let (_t, root) = tmp_root();
        let path = root.join("settings.json");
        std::fs::write(&path, "{broken").unwrap();
        assert!(matches!(
            Settings::load_from(&path),
            Err(Error::SettingsInvalid { .. })
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let (_t, root) = tmp_root();
        let path = root.join("settings.json");
        std::fs::write(&path, r#"{"vault": "/v", "later": true}"#).unwrap();
        assert_eq!(
            Settings::load_from(&path).unwrap().vault,
            Some(PathBuf::from("/v"))
        );
    }

    #[test]
    fn configure_sets_both_and_initializes_empty_vault() {
        let (_t, root) = tmp_root();
        dirs(&root, &["Vault", "Code"]);
        let mut s = Settings::default();
        let v = s
            .configure(&root.join("Vault"), &Access::manual([root.join("Code")]))
            .unwrap();
        assert!(v.projects_dir().is_dir());
        assert_eq!(s.vault, Some(root.join("Vault")));
        assert_eq!(s.access().unwrap().folders, [root.join("Code")]);

        let mut s = Settings::default();
        s.configure(&root.join("Vault"), &Access::auto()).unwrap();
        assert_eq!(s.access().unwrap().mode, AccessMode::Auto);
    }

    #[test]
    fn configure_failure_leaves_disk_and_settings_untouched() {
        let (_t, root) = tmp_root();
        dirs(&root, &["Vault"]);
        let mut s = Settings::default();
        assert!(matches!(
            s.configure(&root.join("Vault"), &Access::manual([root.join("missing")])),
            Err(Error::FolderNotFound(_))
        ));
        assert!(matches!(
            s.configure(&root.join("Vault"), &Access::manual([])),
            Err(Error::NoAccessibleFolders)
        ));
        assert!(!root.join("Vault/projects").exists());
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn engine_normalizes_values_and_model_and_effort_are_optional() {
        let h = Engine {
            client: " claude ".into(),
            model: Some("  ".into()),
            effort: Some(" high ".into()),
        };
        assert_eq!(
            h.normalized(),
            Engine {
                client: "claude".into(),
                model: None,
                effort: Some("high".into()),
            }
        );
        let json = serde_json::to_string(&h.normalized()).unwrap();
        assert_eq!(json, r#"{"client":"claude","effort":"high"}"#);
        let bare: Engine = serde_json::from_str(r#"{"client":"claude"}"#).unwrap();
        assert_eq!(bare.effort, None, "model and effort are optional");
    }

    #[test]
    fn set_access_needs_a_vault() {
        let (_t, root) = tmp_root();
        dirs(&root, &["Vault/projects", "Code"]);
        let mut s = Settings::default();
        assert!(matches!(
            s.set_access(&Access::auto()),
            Err(Error::VaultNotConfigured)
        ));
        s.vault = Some(root.join("Vault"));
        s.set_access(&Access::manual([root.join("Code")])).unwrap();
        assert_eq!(s.access().unwrap().mode, AccessMode::Manual);
    }

    #[test]
    fn sweep_schedule_defaults_and_normalizes() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            s.sweep,
            SweepSettings {
                every_hours: 1,
                per_run: 4
            }
        );
        let s: Settings = serde_json::from_str(r#"{"sweep":{"every_hours":6}}"#).unwrap();
        assert_eq!(s.sweep.every_hours, 6);
        assert_eq!(s.sweep.per_run, 4);
        let odd = SweepSettings {
            every_hours: 5,
            per_run: 7,
        }
        .normalized();
        assert_eq!(
            odd,
            SweepSettings {
                every_hours: 1,
                per_run: 8
            }
        );
        let by_hand = SweepSettings {
            every_hours: 0,
            per_run: 2,
        };
        assert_eq!(by_hand.normalized(), by_hand);
        assert!(
            !serde_json::to_string(&Settings::default())
                .unwrap()
                .contains("sweep")
        );
    }
}
