//! Which folders sbrain may work in. Checked when a request arrives; nothing is scanned.
//!
//! - `auto`: every folder is allowed; there is no folder list, and a project is named after its git
//!   repository.
//! - `manual`: only folders inside one of the listed folders are allowed; the listed folders also
//!   name projects (see `project`).
//!
//! The vault is sbrain's own folder and is always allowed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::canonical;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessMode {
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    pub mode: AccessMode,
    #[serde(default)]
    pub folders: Vec<PathBuf>,
}

impl Access {
    pub fn auto() -> Self {
        Self {
            mode: AccessMode::Auto,
            folders: Vec::new(),
        }
    }

    pub fn manual(folders: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            mode: AccessMode::Manual,
            folders: folders.into_iter().collect(),
        }
    }

    /// The deepest listed folder that contains `path`. `path` should be canonical.
    /// Always `None` in auto mode, even when folders are listed.
    pub fn folder_for(&self, path: &Path) -> Option<&Path> {
        if self.mode == AccessMode::Auto {
            return None;
        }
        self.folders
            .iter()
            .filter(|f| path.starts_with(f))
            .max_by_key(|f| f.components().count())
            .map(PathBuf::as_path)
    }

    pub fn allows(&self, path: &Path) -> bool {
        self.mode == AccessMode::Auto || self.folder_for(path).is_some()
    }

    /// Checks the folders and returns a normalized copy: canonical, deduplicated, sorted.
    /// Folders must exist and must not be inside the vault; manual mode needs at least one.
    /// Auto mode has no folder list, so any listed folders are dropped.
    pub fn validated(&self, vault: &Path) -> Result<Self> {
        if self.mode == AccessMode::Auto {
            return Ok(Self::auto());
        }
        let mut folders = Vec::with_capacity(self.folders.len());
        for folder in &self.folders {
            if !folder.is_dir() {
                return Err(Error::FolderNotFound(folder.clone()));
            }
            let folder = canonical(folder)?;
            if folder.starts_with(vault) {
                return Err(Error::FolderInsideVault { folder });
            }
            folders.push(folder);
        }
        folders.sort();
        folders.dedup();
        if self.mode == AccessMode::Manual && folders.is_empty() {
            return Err(Error::NoAccessibleFolders);
        }
        Ok(Self {
            mode: self.mode,
            folders,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for d in ["Vault/projects", "Code/app", "Work"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        (tmp, root)
    }

    #[test]
    fn auto_allows_everything_manual_only_listed() {
        let (_t, root) = root();
        assert!(Access::auto().allows(Path::new("/anywhere")));

        let manual = Access::manual([root.join("Code")]);
        assert!(manual.allows(&root.join("Code/app/src")));
        assert!(manual.allows(&root.join("Code")));
        assert!(!manual.allows(&root.join("Work")));
        assert!(
            !manual.allows(&root.join("CodeX")),
            "prefix of a name is not containment"
        );
    }

    #[test]
    fn deepest_folder_wins() {
        let (_t, root) = root();
        let access = Access::manual([root.join("Code"), root.join("Code/app")]);
        assert_eq!(
            access.folder_for(&root.join("Code/app/src")),
            Some(root.join("Code/app").as_path())
        );
    }

    #[test]
    fn validated_normalizes_and_rejects_bad_folders() {
        let (_t, root) = root();
        let vault = root.join("Vault");

        let ok = Access::manual([
            root.join("Work"),
            root.join("Code/../Code"),
            root.join("Code"),
        ])
        .validated(&vault)
        .unwrap();
        assert_eq!(ok.folders, [root.join("Code"), root.join("Work")]);

        assert!(matches!(
            Access::manual([root.join("missing")]).validated(&vault),
            Err(Error::FolderNotFound(_))
        ));
        assert!(matches!(
            Access::manual([root.join("Vault/projects")]).validated(&vault),
            Err(Error::FolderInsideVault { .. })
        ));
        assert!(matches!(
            Access::manual([]).validated(&vault),
            Err(Error::NoAccessibleFolders)
        ));
        assert_eq!(Access::auto().validated(&vault).unwrap(), Access::auto());
    }

    #[test]
    fn auto_mode_drops_and_ignores_folders() {
        let (_t, root) = root();
        let auto_with_folders = Access {
            mode: AccessMode::Auto,
            folders: vec![root.join("Code"), root.join("missing")],
        };
        assert_eq!(auto_with_folders.folder_for(&root.join("Code/app")), None);
        assert_eq!(
            auto_with_folders.validated(&root.join("Vault")).unwrap(),
            Access::auto()
        );
    }

    #[test]
    fn serializes_as_mode_and_folders() {
        let json = serde_json::to_string(&Access::manual([PathBuf::from("/c")])).unwrap();
        assert_eq!(json, r#"{"mode":"manual","folders":["/c"]}"#);
        let back: Access = serde_json::from_str(r#"{"mode":"auto"}"#).unwrap();
        assert_eq!(back, Access::auto());
    }
}
