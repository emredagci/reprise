use std::path::{Path, PathBuf};

use crate::config::{Config, FORMAT};
use crate::error::canonical;
use crate::{Error, Result, Settings, scaffold};

/// Vault root and its known sub-paths. Layout: docs/vault-format.md.
#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

/// What a chosen folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderKind {
    /// Has `.reprise/vault.toml`: an reprise vault.
    Vault,
    /// Has `projects/` but no `.reprise/vault.toml`: a format 1 vault. It is not opened; `reprise import`
    /// reads its records.
    Format1,
    /// Empty (may contain only `.git` or `.DS_Store`): can become a new vault.
    Empty,
    /// Has other content: not used as a vault, and the user's files are left alone.
    Other,
    /// Empty, but inside another git repository: refused, so the vault keeps a history of its own.
    InsideRepository,
}

/// Entries ignored when deciding whether a folder is empty.
const IGNORED_IN_EMPTY: &[&str] = &[".git", ".DS_Store"];

/// The file that makes a folder a vault.
const MARKER: &str = ".reprise/vault.toml";
impl Vault {
    /// Opens an existing vault. Fails if the folder is missing, is not a vault, or has a format this
    /// binary does not read.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        match Self::inspect(root)? {
            FolderKind::Vault => {}
            FolderKind::Format1 => return Err(Error::Format1Vault(root.to_path_buf())),
            FolderKind::Empty | FolderKind::Other | FolderKind::InsideRepository => {
                return Err(Error::NotAVault(root.to_path_buf()));
            }
        }
        let vault = Self {
            root: canonical(root)?,
        };
        let config = vault.read_config()?;
        if config.format != FORMAT {
            return Err(Error::UnsupportedFormat {
                path: vault.config_path(),
                found: config.format,
            });
        }
        Ok(vault)
    }

    /// Whether a folder can be used as a vault.
    pub fn inspect(path: &Path) -> Result<FolderKind> {
        if !path.is_dir() {
            return Err(Error::VaultNotFound(path.to_path_buf()));
        }
        if path.join(MARKER).is_file() {
            return Ok(FolderKind::Vault);
        }
        if path.join("projects").is_dir() {
            return Ok(FolderKind::Format1);
        }
        let entries = std::fs::read_dir(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut names = entries.flatten().map(|e| e.file_name());
        let empty = names.all(|n| IGNORED_IN_EMPTY.iter().any(|i| n == *i));
        if !empty {
            return Ok(FolderKind::Other);
        }
        Ok(match crate::git::place(path) {
            crate::git::Place::Inside(_) => FolderKind::InsideRepository,
            _ => FolderKind::Empty,
        })
    }

    /// Makes the chosen folder the vault: an empty folder gets the whole layout from the templates, an
    /// existing vault is opened and its agent guide brought up to date, anything else is refused.
    pub fn choose(path: &Path) -> Result<Self> {
        match Self::inspect(path)? {
            FolderKind::Vault => {
                let vault = Self::open(path)?;
                scaffold::refresh_guide(vault.root())?;
                Ok(vault)
            }
            FolderKind::Empty => {
                scaffold::init_vault(path)?;
                // With git, the history starts with the first save.
                crate::git::start_history(path)?;
                Self::open(path)
            }
            FolderKind::Format1 => Err(Error::Format1Vault(path.to_path_buf())),
            FolderKind::Other => Err(Error::NotAVault(path.to_path_buf())),
            FolderKind::InsideRepository => {
                let repo = match crate::git::place(path) {
                    crate::git::Place::Inside(repo) => repo,
                    _ => path.to_path_buf(),
                };
                Err(Error::InsideRepository {
                    path: path.to_path_buf(),
                    repo,
                })
            }
        }
    }

    /// Order: `REPRISE_VAULT`, then the settings file. There is no default path.
    pub fn discover() -> Result<Self> {
        if let Some(path) = crate::env_var("VAULT") {
            return Self::open(PathBuf::from(path));
        }
        Self::from_settings(&Settings::load()?)
    }

    pub fn from_settings(settings: &Settings) -> Result<Self> {
        match &settings.vault {
            Some(path) => Self::open(path),
            None => Err(Error::VaultNotConfigured),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn project_dir(&self, name: &str) -> PathBuf {
        self.projects_dir().join(name)
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join(MARKER)
    }

    pub fn profile_dir(&self) -> PathBuf {
        self.root.join("profile")
    }

    pub fn notes_dir(&self) -> PathBuf {
        self.root.join("notes")
    }

    /// Machine-local state and logs (gitignored).
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(".reprise/cache")
    }

    fn read_config(&self) -> Result<Config> {
        let path = self.config_path();
        let text = crate::safe::read_to_string(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        Config::parse(&text).map_err(|source| Error::ConfigInvalid {
            path,
            message: source.message().to_owned(),
        })
    }

    /// The vault's settings. A file that became unreadable after the vault was opened counts as the
    /// default, so a hook never fails over it (the doctor reports it).
    pub fn config(&self) -> Config {
        self.read_config().unwrap_or_default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// A new format 2 vault in `path` (created if missing).
    pub(crate) fn new_vault(path: &Path) -> Vault {
        fs::create_dir_all(path).unwrap();
        Vault::choose(path).unwrap()
    }

    /// A vault set up as on a machine without git.
    pub(crate) fn new_vault_without_git(path: &Path) -> Vault {
        fs::create_dir_all(path).unwrap();
        scaffold::init_vault(path).unwrap();
        Vault::open(path).unwrap()
    }

    #[test]
    fn inspect_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        new_vault(&root.join("vault"));
        assert_eq!(
            Vault::inspect(&root.join("vault")).unwrap(),
            FolderKind::Vault
        );

        let format1 = root.join("format1");
        fs::create_dir_all(format1.join("projects")).unwrap();
        assert_eq!(Vault::inspect(&format1).unwrap(), FolderKind::Format1);

        let empty = root.join("empty");
        fs::create_dir_all(empty.join(".git")).unwrap();
        fs::write(empty.join(".DS_Store"), "").unwrap();
        assert_eq!(Vault::inspect(&empty).unwrap(), FolderKind::Empty);

        let other = root.join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("notes.txt"), "").unwrap();
        assert_eq!(Vault::inspect(&other).unwrap(), FolderKind::Other);

        assert!(matches!(
            Vault::inspect(&root.join("missing")),
            Err(Error::VaultNotFound(_))
        ));
    }

    #[test]
    fn choose_scaffolds_empty_and_refuses_other_and_format1() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(empty.join(".git")).unwrap();
        let v = Vault::choose(&empty).unwrap();
        assert!(v.projects_dir().is_dir());
        assert!(v.profile_dir().join("user.md").is_file());
        assert_eq!(v.config().format, FORMAT);

        let other = tmp.path().join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("a.md"), "x").unwrap();
        assert!(matches!(Vault::choose(&other), Err(Error::NotAVault(_))));
        assert_eq!(
            fs::read_dir(&other).unwrap().count(),
            1,
            "a folder with other content must not be touched"
        );

        let format1 = tmp.path().join("format1");
        fs::create_dir_all(format1.join("projects")).unwrap();
        assert!(matches!(
            Vault::choose(&format1),
            Err(Error::Format1Vault(_))
        ));
        assert!(!format1.join(".reprise").exists());
    }

    #[test]
    fn choose_existing_refreshes_guide_and_keeps_user_files() {
        let tmp = tempfile::tempdir().unwrap();
        let v = new_vault(tmp.path());
        fs::write(v.root().join("AGENTS.md"), "stale").unwrap();
        fs::write(v.profile_dir().join("user.md"), "me").unwrap();
        Vault::choose(tmp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(v.root().join("AGENTS.md")).unwrap(),
            scaffold::AGENTS_MD
        );
        assert_eq!(
            fs::read_to_string(v.profile_dir().join("user.md")).unwrap(),
            "me"
        );
    }

    #[test]
    fn open_checks_marker_and_format() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(Vault::open(tmp.path()), Err(Error::NotAVault(_))));

        let v = new_vault(&tmp.path().join("v"));
        fs::write(v.config_path(), "format = 3\n").unwrap();
        assert!(matches!(
            Vault::open(v.root()),
            Err(Error::UnsupportedFormat { found: 3, .. })
        ));
        fs::write(v.config_path(), "not toml [").unwrap();
        assert!(matches!(
            Vault::open(v.root()),
            Err(Error::ConfigInvalid { .. })
        ));
    }

    #[test]
    fn unconfigured_settings_have_no_default() {
        assert!(matches!(
            Vault::from_settings(&Settings::default()),
            Err(Error::VaultNotConfigured)
        ));
        let tmp = tempfile::tempdir().unwrap();
        new_vault(tmp.path());
        let s = Settings {
            vault: Some(tmp.path().to_path_buf()),
            ..Settings::default()
        };
        assert_eq!(
            Vault::from_settings(&s).unwrap().root(),
            tmp.path().canonicalize().unwrap()
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[test]
    fn a_new_vault_starts_its_history() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("vault");
        new_vault(&root);
        assert!(root.join(".git").is_dir());
        assert_eq!(git_out(&root, &["log", "--format=%s"]), "reprise vault");
        assert_eq!(
            git_out(&root, &["status", "--porcelain"]),
            "",
            "everything is committed"
        );
    }

    #[test]
    fn an_empty_cloned_repository_keeps_its_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("clone");
        fs::create_dir_all(&root).unwrap();
        git_out(&root, &["init", "-q"]);
        git_out(
            &root,
            &["remote", "add", "origin", "git@github.com:me/vault.git"],
        );
        assert_eq!(Vault::inspect(&root).unwrap(), FolderKind::Empty);
        Vault::choose(&root).unwrap();
        assert_eq!(
            git_out(&root, &["remote", "get-url", "origin"]),
            "git@github.com:me/vault.git"
        );
        assert_eq!(git_out(&root, &["rev-list", "--count", "HEAD"]), "1");
    }

    #[test]
    fn a_folder_inside_another_repository_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().canonicalize().unwrap();
        git_out(&outer, &["init", "-q"]);
        let inner = outer.join("notes");
        fs::create_dir(&inner).unwrap();
        assert_eq!(
            Vault::inspect(&inner).unwrap(),
            FolderKind::InsideRepository
        );
        match Vault::choose(&inner) {
            Err(Error::InsideRepository { repo, .. }) => assert_eq!(repo, outer),
            other => panic!("{other:?}"),
        }
        assert!(
            fs::read_dir(&inner).unwrap().next().is_none(),
            "nothing is written"
        );

        // A repository that has other files is Other.
        fs::write(outer.join("README.md"), "x").unwrap();
        assert_eq!(Vault::inspect(&outer).unwrap(), FolderKind::Other);
    }
}
