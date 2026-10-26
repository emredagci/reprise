use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no vault selected")]
    VaultNotConfigured,

    #[error("vault folder not found: {0}")]
    VaultNotFound(PathBuf),

    #[error(
        "{0} has other files; sbrain only sets up a vault in an empty folder, so nothing there is changed"
    )]
    NotAVault(PathBuf),

    #[error(
        "{0} has other files; sbrain only sets up a vault in an empty folder, so nothing there is \
         changed. Its records can be imported into the new vault with `sbrain import`"
    )]
    Format1Vault(PathBuf),

    #[error("{path}: vault format {found} is not supported by this version of sbrain")]
    UnsupportedFormat { path: PathBuf, found: u32 },

    #[error("{path}: {message}")]
    ConfigInvalid { path: PathBuf, message: String },

    /// The job ledger (`.sbrain/cache/ledger.sqlite`) could not be read or written.
    #[error("{path}: {message}")]
    Ledger { path: PathBuf, message: String },

    #[error("{0} is not a format 1 vault: there is nothing to import")]
    NotFormat1Vault(PathBuf),

    #[error("{0} overlaps the vault; import from a folder outside it")]
    ImportOverlap(PathBuf),

    #[error(
        "{path} is inside the git repository {repo}; choose a folder outside it, so the vault keeps a \
         history of its own"
    )]
    InsideRepository { path: PathBuf, repo: PathBuf },

    #[error("{0}")]
    Git(String),

    #[error("accessible folders are not set up")]
    AccessNotConfigured,

    #[error("manual mode needs at least one accessible folder")]
    NoAccessibleFolders,

    #[error("folder not found: {0}")]
    FolderNotFound(PathBuf),

    #[error("{folder} is inside the vault; accessible folders hold code, not sbrain records")]
    FolderInsideVault { folder: PathBuf },

    #[error("could not find the settings directory")]
    NoConfigDir,

    #[error("{path}: settings file is invalid: {source}")]
    SettingsInvalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error("record not found: {0}")]
    RecordNotFound(String),

    /// A record change that the schema does not allow; the message says why.
    #[error("{0}")]
    InvalidChange(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Resolves an existing path to its real form (e.g. /var → /private/var on macOS).
pub(crate) fn canonical(path: &std::path::Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}
