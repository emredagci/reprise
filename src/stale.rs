//! Whether a record's `files` changed after it was last verified, else after its commit (or, without
//! one, after its date).
//! A project folder may hold several repos, so each file is asked in its own repo.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::record::Record;
use crate::{Access, AccessMode, Vault};

/// Hours after which the staleness the doctor found is checked again.
pub const CACHE_HOURS: i64 = 24;
const CACHE_FILE: &str = "stale.json";

/// What the last full doctor run found stale, in `.reprise/cache/stale.json`. Checking runs git for every
/// file of every verified item, so session start reads this instead of checking itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    /// Local time of the check, `YYYY-MM-DD HH:MM`.
    pub at: String,
    pub items: Vec<StaleItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleItem {
    pub project: String,
    pub id: String,
    pub changed_commits: usize,
}

impl Found {
    pub fn load(vault: &Vault) -> Option<Self> {
        let text = crate::safe::read_to_string(vault.cache_dir().join(CACHE_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Saving is best effort: the cache only spares session start the work.
    pub fn save(&self, vault: &Vault) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = crate::scaffold::write(&vault.cache_dir().join(CACHE_FILE), &json);
        }
    }

    /// Whether the check is older than `CACHE_HOURS` at `now`, or its time cannot be read.
    pub fn is_old(&self, now: NaiveDateTime) -> bool {
        NaiveDateTime::parse_from_str(&self.at, "%Y-%m-%d %H:%M")
            .map_or(true, |at| (now - at).num_hours() >= CACHE_HOURS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    /// The record names no files (or there is no project): nothing to check.
    NotApplicable,
    /// No repo, or none of the files is in a git repo.
    Unknown,
    Checked {
        changed_commits: usize,
    },
}

impl Staleness {
    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Checked { changed_commits } if *changed_commits > 0)
    }
}

/// The project's repository on this machine: the one last seen in a session, else (manual mode) the
/// first matching folder under the listed folders.
pub fn repo_for(vault: &Vault, access: &Access, project: &str) -> Option<PathBuf> {
    if let Some(path) = crate::cache::repo_path(vault, project) {
        return Some(path);
    }
    if access.mode == AccessMode::Auto {
        return None;
    }
    let containers = vault.config().projects.containers;
    access
        .folders
        .iter()
        .flat_map(|f| {
            std::iter::once(f.join(project))
                .chain(containers.iter().map(move |c| f.join(c).join(project)))
        })
        .find(|candidate| candidate.exists())
}

pub fn stale_check(record: &Record, repo: Option<&Path>) -> Staleness {
    let files = record.get("files").map(|v| v.items()).unwrap_or_default();
    if files.is_empty() {
        return Staleness::NotApplicable;
    }
    let Some(repo) = repo else {
        return Staleness::Unknown;
    };

    // Files grouped by the repo that holds them, in first-seen order.
    let mut groups: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for f in &files {
        let target = repo.join(f);
        let mut probe = if target.is_dir() {
            target.clone()
        } else {
            target.parent().map_or(target.clone(), Path::to_path_buf)
        };
        while !probe.exists() && probe != repo {
            match probe.parent() {
                Some(parent) => probe = parent.to_path_buf(),
                None => break,
            }
        }
        let Some(root) =
            git(&probe, &["rev-parse", "--show-toplevel"]).map(|o| PathBuf::from(o.trim()))
        else {
            continue;
        };
        let rel = if target.exists() {
            let real_target = target.canonicalize().unwrap_or(target.clone());
            let real_root = root.canonicalize().unwrap_or(root.clone());
            match real_target.strip_prefix(&real_root) {
                Ok(rel) => rel.to_string_lossy().into_owned(),
                Err(_) => f.clone(),
            }
        } else {
            f.clone()
        };
        match groups.iter_mut().find(|(r, _)| *r == root) {
            Some((_, list)) => list.push(rel),
            None => groups.push((root, vec![rel])),
        }
    }
    if groups.is_empty() {
        return Staleness::Unknown;
    }

    // A verified item counts from its last verification: what changed before it was verified again
    // is covered.
    let verified_at = record.text("verified_at");
    // A commit id only: anything else (a leading `-` would be read as a git option) is ignored.
    let commit = Some(record.text("commit"))
        .filter(|c| (4..=40).contains(&c.len()) && c.chars().all(|ch| ch.is_ascii_hexdigit()))
        .unwrap_or("");
    let since = record.first_truthy(&["verified_at", "updated", "created"]);
    let mut total = 0;
    for (root, rel) in &groups {
        let mut out = None;
        if verified_at.is_empty() && !commit.is_empty() {
            out = git_log(root, &format!("{commit}..HEAD"), rel);
        }
        if out.is_none()
            && let Some(since) = since
        {
            // The commit may belong to another repo of a multi-repo project: fall back to the date.
            out = git_log(root, &format!("--since={since}"), rel);
        }
        if let Some(out) = out {
            total += out.lines().filter(|l| !l.trim().is_empty()).count();
        }
    }
    Staleness::Checked {
        changed_commits: total,
    }
}

fn git_log(root: &Path, range: &str, rel: &[String]) -> Option<String> {
    let mut args = vec!["log", "--oneline", range, "--"];
    args.extend(rel.iter().map(String::as_str));
    git(root, &args)
}

/// stdout of a successful git command; `None` on failure.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn changed_after_commit_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap();
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.rs"), "1").unwrap();
        git_ok(&repo, &["add", "."]);
        git_ok(&repo, &["commit", "-q", "-m", "one"]);
        let head = git(&repo, &["rev-parse", "--short", "HEAD"]).unwrap();
        let record = Record::parse(&format!(
            "---\nfiles: [a.rs]\ncommit: {}\n---\n",
            head.trim()
        ))
        .unwrap();
        assert_eq!(
            stale_check(&record, Some(&repo)),
            Staleness::Checked { changed_commits: 0 }
        );

        std::fs::write(repo.join("a.rs"), "2").unwrap();
        git_ok(&repo, &["commit", "-qam", "two"]);
        assert!(stale_check(&record, Some(&repo)).is_stale());
    }

    #[test]
    fn verifying_again_covers_what_changed_before() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap();
        let commit_on = |date: &str, text: &str| {
            std::fs::write(repo.join("a.rs"), text).unwrap();
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["commit", "-qam", text])
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        };
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.rs"), "0").unwrap();
        git_ok(&repo, &["add", "."]);
        commit_on("2026-01-01T12:00:00", "1");
        let work = git(&repo, &["rev-parse", "--short", "HEAD"]).unwrap();
        commit_on("2026-02-01T12:00:00", "2");

        let record = |verified_at: &str| {
            Record::parse(&format!(
                "---\nfiles: [a.rs]\ncommit: {}\nverified_at: {verified_at}\n---\n",
                work.trim()
            ))
            .unwrap()
        };
        assert!(
            stale_check(&record(""), Some(&repo)).is_stale(),
            "changed after the work's commit"
        );
        assert_eq!(
            stale_check(&record("2026-03-01 00:00"), Some(&repo)),
            Staleness::Checked { changed_commits: 0 },
            "verified again after that change"
        );
        commit_on("2026-04-01T12:00:00", "3");
        assert_eq!(
            stale_check(&record("2026-03-01 00:00"), Some(&repo)),
            Staleness::Checked { changed_commits: 1 }
        );
    }

    #[test]
    fn no_files_or_no_repo() {
        let record = Record::parse("---\nfiles: []\n---\n").unwrap();
        assert_eq!(stale_check(&record, None), Staleness::NotApplicable);
        let record = Record::parse("---\nfiles: [a.rs]\n---\n").unwrap();
        assert_eq!(stale_check(&record, None), Staleness::Unknown);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(stale_check(&record, Some(tmp.path())), Staleness::Unknown);
    }

    /// A `commit` that is no commit id (here an option git would act on) is not given to git.
    #[test]
    fn a_commit_field_is_never_a_git_option() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap();
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.rs"), "1").unwrap();
        git_ok(&repo, &["add", "."]);
        git_ok(&repo, &["commit", "-q", "-m", "one"]);
        let out = repo.join("written-by-git");
        let record = Record::parse(&format!(
            "---\nfiles: [a.rs]\ncommit: --output={}\ncreated: 2000-01-01\n---\n",
            out.display()
        ))
        .unwrap();
        stale_check(&record, Some(&repo));
        // Taken as an option, git would write `written-by-git..HEAD`.
        let written: Vec<_> = std::fs::read_dir(&repo)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("written-by-git")
            })
            .collect();
        assert!(
            written.is_empty(),
            "git did not take the field as an option"
        );
    }
}
