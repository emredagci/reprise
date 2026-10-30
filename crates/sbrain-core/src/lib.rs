//! sbrain core: every operation on the vault. Knows nothing about coding agents.

pub mod access;
pub mod browse;
pub mod cache;
pub mod compact;
pub mod config;
pub mod context;
pub mod dates;
pub mod distill;
pub mod error;
pub mod events;
pub mod evidence;
pub mod facts;
pub mod git;
pub mod graph;
pub mod import;
pub mod overview;
pub mod project;
pub mod record;
pub mod safe;
pub mod scaffold;
pub mod schema;
pub mod settings;
pub mod text;
pub mod vault;

pub mod doctor;
pub mod index;
pub mod ledger;
pub mod report;
pub mod reset;
pub mod search;
pub mod secret;
pub mod stale;
pub mod store;
pub mod sweep;
pub mod table;
pub mod verify;

pub use access::{Access, AccessMode};
pub use error::{Error, Result};
pub use settings::{Engine, Settings};
pub use vault::{FolderKind, Vault};

/// The variable `SBRAIN_<name>`.
pub fn env_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("SBRAIN_{name}"))
}
