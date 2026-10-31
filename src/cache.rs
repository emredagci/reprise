//! Machine-local state in `.reprise/cache/` (gitignored). Only what cannot live in tracked files:
//! where each project's repository is on this machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Result, Vault};

fn repos_file(vault: &Vault) -> PathBuf {
    vault.cache_dir().join("repos.json")
}

fn read_repos(vault: &Vault) -> BTreeMap<String, PathBuf> {
    crate::safe::read_to_string(repos_file(vault))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The local repository last seen for `project`, if it still exists.
pub fn repo_path(vault: &Vault, project: &str) -> Option<PathBuf> {
    read_repos(vault).remove(project).filter(|p| p.is_dir())
}

/// Remembers where `project`'s repository is on this machine. Writes only when it changed.
pub fn remember_repo(vault: &Vault, project: &str, repo: &Path) -> Result<()> {
    let mut repos = read_repos(vault);
    if repos.get(project).map(PathBuf::as_path) == Some(repo) {
        return Ok(());
    }
    repos.insert(project.to_owned(), repo.to_path_buf());
    let mut text = serde_json::to_string_pretty(&repos).expect("a map of paths serializes");
    text.push('\n');
    crate::scaffold::write(&repos_file(vault), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_existing_repos_only() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(&tmp.path().join("v"));
        let repo = tmp.path().join("api");
        std::fs::create_dir_all(&repo).unwrap();

        assert_eq!(repo_path(&vault, "api"), None);
        remember_repo(&vault, "api", &repo).unwrap();
        assert_eq!(repo_path(&vault, "api"), Some(repo.clone()));

        std::fs::remove_dir(&repo).unwrap();
        assert_eq!(repo_path(&vault, "api"), None, "a moved repo is not used");
    }
}
