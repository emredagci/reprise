//! The vault's history in git. With git installed, a new vault is a repository from its
//! first save; reprise commits only what it writes, only where the vault is the repository's root, and
//! never pushes. Without git, nothing here does anything.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::stale::git;

/// The first commit of a new vault.
const FIRST_COMMIT: &str = "reprise vault";
/// Author for a vault whose user has no git identity; written to the vault's own git config only.
const FALLBACK_NAME: &str = "reprise";
const FALLBACK_EMAIL: &str = "reprise@localhost";

/// A folder's place in git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// git is not installed.
    NoGit,
    /// Not in a repository.
    Outside,
    /// The root of a repository.
    Root,
    /// Inside the repository rooted at this folder.
    Inside(PathBuf),
}

/// Whether the `git` command runs.
pub fn available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn place(dir: &Path) -> Place {
    if !available() {
        return Place::NoGit;
    }
    let Some(top) = git(dir, &["rev-parse", "--show-toplevel"]) else {
        return Place::Outside;
    };
    let top = PathBuf::from(top.trim());
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canonical(&top) == canonical(dir) {
        Place::Root
    } else {
        Place::Inside(top)
    }
}

/// Makes a new vault a repository (unless it already is one) and commits everything in it. Does
/// nothing without git or inside another repository.
pub fn start_history(root: &Path) -> crate::Result<()> {
    match place(root) {
        Place::NoGit | Place::Inside(_) => return Ok(()),
        Place::Outside => {
            git(root, &["init", "--quiet"]).ok_or_else(|| failed(root, "git init"))?;
        }
        Place::Root => {}
    }
    // Without a name or email of the user's, commits would fail; give this vault its own.
    let has_identity = ["user.name", "user.email"]
        .iter()
        .all(|key| git(root, &["config", key]).is_some_and(|v| !v.trim().is_empty()));
    if !has_identity {
        git(root, &["config", "user.name", FALLBACK_NAME]);
        git(root, &["config", "user.email", FALLBACK_EMAIL]);
    }
    git(root, &["add", "--all"]).ok_or_else(|| failed(root, "git add"))?;
    // Nothing staged (an existing repository that already holds it all): no commit needed.
    if git(root, &["diff", "--cached", "--quiet"]).is_none() {
        git(root, &["commit", "--quiet", "-m", FIRST_COMMIT])
            .ok_or_else(|| failed(root, "git commit"))?;
    }
    Ok(())
}

/// Ledger kind of a vault commit that failed and waits to be tried again.
pub const COMMIT: &str = "commit";

/// Commits exactly `files` (relative to the vault) when the vault is a repository's root, and returns
/// the short sha. When git fails (a lock another git holds, a hook that refuses), the commit waits in
/// the job ledger and `retry_commits` makes it later; nothing written is left uncommitted for good.
/// `None` when there is nothing to commit, the vault is not a repository's root, or git failed.
pub fn commit(root: &Path, files: &[PathBuf], message: &str) -> Option<String> {
    match try_commit(root, files, message) {
        Ok(sha) => sha,
        Err(error) => {
            let now = chrono::Local::now().timestamp();
            let payload = serde_json::json!({"files": files, "message": message});
            if let Ok(ledger) = crate::ledger::Ledger::open_root(root) {
                let _ = ledger.fail(
                    COMMIT,
                    &commit_key(files, message, now),
                    &payload,
                    &error,
                    now,
                );
            }
            None
        }
    }
}

/// Makes the commits that failed and are due again, and tries the ones given up on too (the doctor
/// reports those; they are dropped once git takes them); returns how many it made. A commit whose
/// files were committed meanwhile, or are gone, is dropped.
pub fn retry_commits(root: &Path, now: i64) -> crate::Result<usize> {
    if !root.join(".reprise/cache/ledger.sqlite").exists() {
        return Ok(0);
    }
    let ledger = crate::ledger::Ledger::open_root(root)?;
    let mut made = 0;
    for job in ledger.list(COMMIT, crate::ledger::State::GaveUp)? {
        let files: Vec<PathBuf> =
            serde_json::from_value(job.payload["files"].clone()).unwrap_or_default();
        let message = job.payload["message"].as_str().unwrap_or("reprise");
        if let Ok(sha) = try_commit(root, &files, message) {
            made += usize::from(sha.is_some());
            ledger.remove(COMMIT, &job.key)?;
        }
    }
    for job in ledger.due(COMMIT, now)? {
        let files: Vec<PathBuf> =
            serde_json::from_value(job.payload["files"].clone()).unwrap_or_default();
        let message = job.payload["message"].as_str().unwrap_or("reprise");
        match try_commit(root, &files, message) {
            Ok(sha) => {
                made += usize::from(sha.is_some());
                ledger.remove(COMMIT, &job.key)?;
            }
            Err(error) => {
                ledger.fail(COMMIT, &job.key, &job.payload, &error, now)?;
            }
        }
    }
    Ok(made)
}

fn commit_key(files: &[PathBuf], message: &str, now: i64) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (files, message).hash(&mut h);
    format!("{now}-{:016x}", h.finish())
}

/// The commit itself: `Ok(None)` when there is nothing to commit or no repository root to commit in,
/// `Err` with git's own words when git fails.
pub fn try_commit(
    root: &Path,
    files: &[PathBuf],
    message: &str,
) -> std::result::Result<Option<String>, String> {
    if place(root) != Place::Root {
        return Ok(None);
    }
    // A file deleted and never tracked would make `git add` fail every time; it has nothing to commit.
    let paths: Vec<&str> = files
        .iter()
        .filter(|f| {
            root.join(f).exists()
                || f.to_str()
                    .is_some_and(|p| run(root, &["ls-files", "--error-unmatch", "--", p]).is_ok())
        })
        .filter_map(|f| f.to_str())
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    let with = |head: &[&'static str]| {
        let mut args: Vec<&str> = head.to_vec();
        args.extend(&paths);
        args
    };
    run(root, &with(&["add", "--"]))?;
    if run(root, &with(&["diff", "--cached", "--quiet", "--"])).is_ok() {
        return Ok(None);
    }
    let mut commit = vec!["commit", "--quiet", "-m", message, "--"];
    commit.extend(&paths);
    run(root, &commit)?;
    run(root, &["rev-parse", "--short", "HEAD"]).map(|s| Some(s.trim().to_owned()))
}

/// git's stdout, or what it said on stderr when it failed.
fn run(root: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git did not run: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    Err(if said.is_empty() {
        format!("git {} failed", args.first().unwrap_or(&""))
    } else {
        said
    })
}

fn failed(root: &Path, what: &str) -> crate::Error {
    crate::Error::Git(format!("{what} failed in {}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn places() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert_eq!(place(&root), Place::Outside);
        run(&root, &["init", "-q"]);
        assert_eq!(place(&root), Place::Root);
        let inner = root.join("notes");
        std::fs::create_dir(&inner).unwrap();
        assert_eq!(place(&inner), Place::Inside(root.clone()));
    }

    #[test]
    fn history_starts_with_one_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.md"), "a").unwrap();
        start_history(root).unwrap();
        let log = git(root, &["log", "--format=%s"]).unwrap();
        assert_eq!(log.trim(), FIRST_COMMIT);
        assert_eq!(git(root, &["status", "--porcelain"]).unwrap(), "");
        // Again: nothing new, no second commit.
        start_history(root).unwrap();
        assert_eq!(
            git(root, &["rev-list", "--count", "HEAD"]).unwrap().trim(),
            "1"
        );

        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join("c.md"), "c").unwrap();
        let sha = commit(root, &[PathBuf::from("b.md")], "b").unwrap();
        assert!(!sha.is_empty());
        let status = git(root, &["status", "--porcelain"]).unwrap();
        assert_eq!(status.trim(), "?? c.md", "only the named file is committed");
    }

    #[test]
    fn nothing_is_committed_inside_another_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        run(outer, &["init", "-q"]);
        let vault = outer.join("vault");
        std::fs::create_dir(&vault).unwrap();
        std::fs::write(vault.join("a.md"), "a").unwrap();
        start_history(&vault).unwrap();
        assert!(!vault.join(".git").exists());
        assert!(commit(&vault, &[PathBuf::from("a.md")], "x").is_none());
        assert!(
            git(outer, &["rev-parse", "HEAD"]).is_none(),
            "the outer repository has no commit"
        );
    }

    #[test]
    fn a_failed_commit_waits_in_the_ledger_and_is_made_later() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        start_history(&root).unwrap();
        std::fs::write(root.join(".gitignore"), ".reprise/\n").unwrap();
        super::run(&root, &["add", ".gitignore"]).unwrap();
        super::run(&root, &["commit", "-qm", "ignore"]).unwrap();

        // Another git holds the index: the commit fails and is kept.
        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join(".git/index.lock"), "").unwrap();
        assert_eq!(commit(&root, &[PathBuf::from("b.md")], "add b"), None);
        let ledger = crate::ledger::Ledger::open_root(&root).unwrap();
        let waiting = ledger.list(COMMIT, crate::ledger::State::Pending).unwrap();
        assert_eq!(waiting.len(), 1);
        assert!(
            waiting[0]
                .last_error
                .as_deref()
                .unwrap_or("")
                .contains("index.lock")
        );

        // Not due yet; once due and the lock is gone, it is made.
        let now = chrono::Local::now().timestamp();
        assert_eq!(retry_commits(&root, now).unwrap(), 0);
        std::fs::remove_file(root.join(".git/index.lock")).unwrap();
        assert_eq!(retry_commits(&root, now + 3600).unwrap(), 1);
        assert_eq!(
            git(&root, &["log", "-1", "--format=%s"]).unwrap().trim(),
            "add b"
        );
        assert!(
            ledger
                .list(COMMIT, crate::ledger::State::Pending)
                .unwrap()
                .is_empty()
        );
        assert_eq!(git(&root, &["status", "--porcelain"]).unwrap(), "");
    }

    #[test]
    fn nothing_to_commit_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        start_history(&root).unwrap();
        assert_eq!(
            try_commit(&root, &[PathBuf::from("a.md")], "again"),
            Ok(None)
        );
        assert_eq!(
            try_commit(&root, &[PathBuf::from("gone.md")], "gone"),
            Ok(None)
        );
    }
}
