//! Open work per project: status counts, items by status, verified items whose
//! files changed since (as the last full doctor run found them; checking takes git per file), and the
//! latest handoff card.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::record::Record;
use crate::text::section;
use crate::{Result, Vault};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    /// Days since `updated`; set for done-unverified items only.
    pub age_days: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSession {
    pub path: PathBuf,
    pub date: String,
    pub next: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectReport {
    /// Item count per status string (`?` when missing), sorted by status.
    pub counts: BTreeMap<String, usize>,
    pub doing: Vec<Row>,
    pub done_unverified: Vec<Row>,
    pub planned: Vec<Row>,
    pub inbox: Vec<Row>,
    pub stale_verified: Vec<Row>,
    pub last_session: Option<LastSession>,
}

pub fn report(vault: &Vault, project: &str, today: NaiveDate) -> Result<ProjectReport> {
    let root = vault.project_dir(project);
    let found = crate::stale::Found::load(vault).unwrap_or_default();
    let stale: Vec<&str> = found
        .items
        .iter()
        .filter(|i| i.project == project)
        .map(|i| i.id.as_str())
        .collect();
    let mut r = ProjectReport::default();
    for path in markdown_files(&root.join("items")) {
        let record = Record::read(&path)?;
        let status = match record.get("status") {
            Some(v) => v.as_text().unwrap_or("?").to_owned(),
            None => "?".to_owned(),
        };
        *r.counts.entry(status.clone()).or_insert(0) += 1;
        let row = Row {
            id: record
                .get("id")
                .and_then(|v| v.as_text())
                .map_or_else(|| stem(&path), str::to_owned),
            title: record.text("title").to_owned(),
            age_days: (status == "done-unverified")
                .then(|| {
                    NaiveDate::parse_from_str(record.text("updated"), "%Y-%m-%d")
                        .ok()
                        .map(|d| (today - d).num_days())
                })
                .flatten(),
        };
        match status.as_str() {
            "doing" => r.doing.push(row),
            "done-unverified" => r.done_unverified.push(row),
            "planned" => r.planned.push(row),
            "inbox" => r.inbox.push(row),
            // Still verified and found stale: an item verified again since is no longer listed.
            "verified"
                if stale.contains(&row.id.as_str())
                    && record.text("verified_at") <= found.at.as_str() =>
            {
                r.stale_verified.push(row)
            }
            _ => {}
        }
    }
    if let Some(path) = markdown_files(&root.join("sessions")).pop() {
        let record = Record::read(&path)?;
        r.last_session = Some(LastSession {
            date: record.text("date").to_owned(),
            next: section(&record.body, "Next step"),
            path,
        });
    }
    Ok(r)
}

/// `*.md` files in `dir`, sorted by name, without hidden files.
pub(crate) fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "md")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.starts_with('.'))
                && p.is_file()
        })
        .collect();
    files.sort();
    files
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_counts_ages_and_last_session() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        let w = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        w(
            "projects/p/items/p-0001.md",
            "---\nid: p-0001\ntitle: A\nstatus: inbox\n---\n",
        );
        w(
            "projects/p/items/p-0002.md",
            "---\nid: p-0002\ntitle: B\nstatus: done-unverified\nupdated: 2026-10-20\n---\n",
        );
        w("projects/p/items/p-0003.md", "---\ntitle: C\n---\n");
        w(
            "projects/p/sessions/2026-10-01-a.md",
            "---\ndate: 2026-10-01\n---\n## Next step\nold\n",
        );
        w(
            "projects/p/sessions/2026-10-24-b.md",
            "---\ndate: 2026-10-24 10:00\n---\n## Next step\nship it\n## Touched records\n",
        );
        let today = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let r = report(&vault, "p", today).unwrap();
        assert_eq!(
            r.counts
                .iter()
                .map(|(k, v)| (k.as_str(), *v))
                .collect::<Vec<_>>(),
            [("?", 1), ("done-unverified", 1), ("inbox", 1)]
        );
        assert_eq!(r.done_unverified[0].age_days, Some(5));
        assert_eq!(r.inbox[0].age_days, None);
        let last = r.last_session.unwrap();
        assert_eq!(last.date, "2026-10-24 10:00");
        assert_eq!(last.next, "ship it");
    }

    #[test]
    fn stale_items_come_from_the_doctors_last_check() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        let w = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        let verified = |id: &str, at: &str| {
            format!("---\nid: {id}\ntitle: T\nstatus: verified\nverified_at: {at}\n---\n")
        };
        w(
            "projects/p/items/p-0001.md",
            &verified("p-0001", "2026-10-20 10:00"),
        );
        w(
            "projects/p/items/p-0002.md",
            &verified("p-0002", "2026-10-26 09:00"),
        );
        w(
            "projects/p/items/p-0003.md",
            "---\nid: p-0003\ntitle: T\nstatus: done-unverified\n---\n",
        );
        let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        assert!(
            report(&vault, "p", today)
                .unwrap()
                .stale_verified
                .is_empty(),
            "no check yet"
        );

        let item = |id: &str| crate::stale::StaleItem {
            project: "p".into(),
            id: id.into(),
            changed_commits: 2,
        };
        crate::stale::Found {
            at: "2026-10-25 12:00".into(),
            items: vec![item("p-0001"), item("p-0002"), item("p-0003")],
        }
        .save(&vault);
        let r = report(&vault, "p", today).unwrap();
        let ids: Vec<&str> = r.stale_verified.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["p-0001"],
            "verified again after the check, or no longer verified, is not stale"
        );
        let found = crate::stale::Found::load(&vault).unwrap();
        let at = |s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        assert!(!found.is_old(at("2026-10-26 11:59")));
        assert!(found.is_old(at("2026-10-26 12:00")));
    }
}
