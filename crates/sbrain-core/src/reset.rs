//! Resetting a project: from a day on, what came before stops being brought up (a new job, a rewrite,
//! a month better forgotten). Nothing is deleted or rewritten: a reset is one more row in
//! `projects/<name>/resets.md`, and an undo another. While a reset is in effect, session start treats
//! the facts that held before it as no longer current, and leaves out the events before it and the
//! work, handoffs and records not touched since; every record stays in the vault and in search.

use std::path::PathBuf;

use chrono::NaiveDate;

use crate::{Error, Result, Vault};

pub const FILE: &str = "resets.md";
const COLUMNS: [&str; 3] = ["Date", "Action", "Note"];
const HEAD: &str = "# Resets\n\nFrom a reset on, session start leaves out what came before it; nothing is deleted. \
An undo cancels the latest reset still in effect. Written with `sbrain reset`.";

fn path(vault: &Vault, project: &str) -> Result<PathBuf> {
    let dir = vault.project_dir(project);
    if project.is_empty()
        || project.contains(['/', '\\'])
        || project.starts_with('.')
        || !dir.is_dir()
    {
        return Err(Error::ProjectNotFound(project.to_owned()));
    }
    Ok(dir.join(FILE))
}

fn rows(vault: &Vault, project: &str) -> Result<Vec<Vec<String>>> {
    let text = crate::safe::read_to_string(path(vault, project)?).unwrap_or_default();
    Ok(crate::table::rows(&text))
}

/// The resets in effect, oldest first: each undo cancels the latest one before it.
fn in_effect(rows: &[Vec<String>]) -> Vec<NaiveDate> {
    let mut stack = Vec::new();
    for r in rows {
        match r[1].as_str() {
            "reset" => {
                if let Ok(d) = NaiveDate::parse_from_str(&r[0], "%Y-%m-%d") {
                    stack.push(d);
                }
            }
            "undo" => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
}

/// The day of the latest reset in effect for `project`; `None` when there is none.
pub fn active(vault: &Vault, project: &str) -> Option<NaiveDate> {
    in_effect(&rows(vault, project).ok()?).last().copied()
}

fn append(vault: &Vault, project: &str, row: Vec<String>) -> Result<PathBuf> {
    let path = path(vault, project)?;
    let mut all = rows(vault, project)?;
    all.push(row);
    crate::scaffold::write(&path, &crate::table::render(HEAD, &COLUMNS, &all))?;
    Ok(path
        .strip_prefix(vault.root())
        .unwrap_or(&path)
        .to_path_buf())
}

/// Resets `project` from `day` on. Returns the file, relative to the vault.
pub fn reset(vault: &Vault, project: &str, day: NaiveDate, note: &str) -> Result<PathBuf> {
    if active(vault, project).is_some_and(|d| d > day) {
        return Err(Error::InvalidChange(format!(
            "{project} was reset later than {day}; undo that first"
        )));
    }
    append(
        vault,
        project,
        vec![
            day.format("%Y-%m-%d").to_string(),
            "reset".into(),
            note.into(),
        ],
    )
}

/// Cancels the latest reset in effect; returns its day and the file.
pub fn undo(vault: &Vault, project: &str, today: NaiveDate) -> Result<(NaiveDate, PathBuf)> {
    let Some(day) = active(vault, project) else {
        return Err(Error::InvalidChange(format!(
            "{project} has no reset in effect"
        )));
    };
    let path = append(
        vault,
        project,
        vec![
            today.format("%Y-%m-%d").to_string(),
            "undo".into(),
            format!("cancels the reset of {day}"),
        ],
    )?;
    Ok((day, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn resets_stack_and_undo() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        crate::scaffold::ensure_project(&vault, "app", None, d("2026-10-01")).unwrap();
        assert_eq!(active(&vault, "app"), None);
        assert!(undo(&vault, "app", d("2026-10-02")).is_err());
        let path = reset(&vault, "app", d("2026-10-10"), "new team").unwrap();
        assert_eq!(path, PathBuf::from("projects/app/resets.md"));
        reset(&vault, "app", d("2026-10-20"), "").unwrap();
        assert!(
            reset(&vault, "app", d("2026-10-15"), "").is_err(),
            "not before the latest"
        );
        assert_eq!(active(&vault, "app"), Some(d("2026-10-20")));
        assert_eq!(
            undo(&vault, "app", d("2026-10-21")).unwrap().0,
            d("2026-10-20")
        );
        assert_eq!(active(&vault, "app"), Some(d("2026-10-10")));
        undo(&vault, "app", d("2026-10-22")).unwrap();
        assert_eq!(active(&vault, "app"), None);
        let text = std::fs::read_to_string(vault.project_dir("app").join(FILE)).unwrap();
        assert_eq!(text.matches("| reset |").count(), 2, "{text}");
        assert_eq!(
            text.matches("| undo |").count(),
            2,
            "nothing is removed: {text}"
        );
    }
}
