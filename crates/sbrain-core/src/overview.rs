//! Read-only summary of the vault for `sbrain overview`: per project, record counts, item statuses,
//! the most pressing open items, the latest decisions and a one-line description from `state.md`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::schema::ItemStatus;
use crate::{Result, Vault};

/// How many open items and decisions each project carries in the summary.
const LIST_LIMIT: usize = 3;
/// Longest description taken from `state.md`, in characters.
const ABOUT_LIMIT: usize = 180;
/// Open statuses, most pressing first.
const OPEN_ORDER: [ItemStatus; 4] = [
    ItemStatus::Doing,
    ItemStatus::DoneUnverified,
    ItemStatus::Planned,
    ItemStatus::Inbox,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Overview {
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectSummary {
    pub name: String,
    pub about: Option<String>,
    pub items: usize,
    pub decisions: usize,
    pub sessions: usize,
    /// Item count per status; statuses with no items are left out.
    pub statuses: BTreeMap<ItemStatus, usize>,
    pub open: Vec<RecordRef>,
    pub recent_decisions: Vec<RecordRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordRef {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
}

struct Record {
    id: String,
    title: String,
    status: Option<ItemStatus>,
}

pub fn overview(vault: &Vault) -> Result<Overview> {
    let mut projects = Vec::new();
    let Ok(entries) = std::fs::read_dir(vault.projects_dir()) else {
        return Ok(Overview { projects });
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        projects.push(summarize(&entry.path(), name));
    }
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Overview { projects })
}

fn summarize(dir: &Path, name: String) -> ProjectSummary {
    let items = read_records(&dir.join("items"));
    let mut decisions = read_records(&dir.join("decisions"));

    let mut statuses = BTreeMap::new();
    for status in items.iter().filter_map(|i| i.status) {
        *statuses.entry(status).or_insert(0) += 1;
    }

    let mut open: Vec<&Record> = items
        .iter()
        .filter(|i| i.status.is_some_and(|s| OPEN_ORDER.contains(&s)))
        .collect();
    open.sort_by_key(|i| {
        let rank = OPEN_ORDER.iter().position(|s| Some(*s) == i.status);
        (rank, std::cmp::Reverse(id_number(&i.id)))
    });

    decisions.sort_by_key(|d| std::cmp::Reverse(id_number(&d.id)));

    ProjectSummary {
        about: crate::safe::read_to_string(dir.join("state.md"))
            .ok()
            .and_then(|s| about(&s)),
        items: items.len(),
        decisions: decisions.len(),
        sessions: count_markdown(&dir.join("sessions")),
        statuses,
        open: open.into_iter().take(LIST_LIMIT).map(to_ref).collect(),
        recent_decisions: decisions
            .iter()
            .take(LIST_LIMIT)
            .map(|d| RecordRef {
                status: None,
                ..to_ref(d)
            })
            .collect(),
        name,
    }
}

fn to_ref(r: &Record) -> RecordRef {
    RecordRef {
        id: r.id.clone(),
        title: r.title.clone(),
        status: r.status,
    }
}

/// Records with a readable frontmatter that has an `id`. Others are skipped (the doctor reports them).
fn read_records(dir: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| crate::safe::read_to_string(e.path()).ok())
        .filter_map(|text| {
            let fm = frontmatter(&text)?;
            let field = |k: &str| fm.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
            let id = field("id").filter(|v| !v.is_empty())?.to_owned();
            Some(Record {
                title: field("title").unwrap_or(&id).to_owned(),
                status: field("status").and_then(|s| s.parse().ok()),
                id,
            })
        })
        .collect()
}

fn count_markdown(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .count()
    })
}

/// `key: value` pairs of a `---` delimited frontmatter block. Values are trimmed and unquoted.
pub(crate) fn frontmatter(text: &str) -> Option<Vec<(&str, &str)>> {
    let body = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = body.find("\n---")?;
    Some(
        body[..end]
            .lines()
            .filter_map(|line| {
                let (k, v) = line.split_once(':')?;
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(v);
                Some((k.trim(), v))
            })
            .collect(),
    )
}

/// The first paragraph of the `## What` section of `state.md`, as plain text.
fn about(state: &str) -> Option<String> {
    let mut lines = state.lines().skip_while(|l| l.trim() != "## What").skip(1);
    let mut words = Vec::new();
    for line in lines.by_ref() {
        let line = line.trim();
        if line.starts_with("## ") || (line.is_empty() && !words.is_empty()) {
            break;
        }
        if line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        words.push(line.replace("**", "").replace('`', ""));
    }
    let text = words.join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= ABOUT_LIMIT {
        return Some(text);
    }
    let cut: String = text.chars().take(ABOUT_LIMIT).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    Some(format!("{}…", cut.trim_end_matches([',', ';', ':', '.'])))
}

/// The trailing number of an id like `api-D-0115`; ids without one sort first.
pub(crate) fn id_number(id: &str) -> u32 {
    id.rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn item(id: &str, title: &str, status: &str) -> String {
        format!(
            "---\nid: {id}\ntitle: {title}\ntype: bug\nstatus: {status}\nrelated: [a, b]\n---\n\n## Problem\n"
        )
    }

    fn decision(id: &str, title: &str) -> String {
        format!("---\nid: {id}\ntitle: {title}\nstatus: accepted\n---\n")
    }

    #[test]
    fn summarizes_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        write(
            root,
            "projects/app/items/app-0001-a.md",
            &item("app-0001", "Old inbox", "inbox"),
        );
        write(
            root,
            "projects/app/items/app-0002-b.md",
            &item("app-0002", "New inbox", "inbox"),
        );
        write(
            root,
            "projects/app/items/app-0003-c.md",
            &item("app-0003", "Shipping: build 2", "doing"),
        );
        write(
            root,
            "projects/app/items/app-0004-d.md",
            &item("app-0004", "Done", "verified"),
        );
        write(
            root,
            "projects/app/items/app-0005-e.md",
            &item("app-0005", "Check it", "done-unverified"),
        );
        write(root, "projects/app/items/broken.md", "no frontmatter");
        write(root, "projects/app/items/.gitkeep", "");
        write(
            root,
            "projects/app/decisions/app-D-0002-x.md",
            &decision("app-D-0002", "Second"),
        );
        write(
            root,
            "projects/app/decisions/app-D-0010-y.md",
            &decision("app-D-0010", "Tenth"),
        );
        write(
            root,
            "projects/app/decisions/app-D-0001-z.md",
            &decision("app-D-0001", "First"),
        );
        write(root, "projects/app/sessions/2026-10-24-abc.md", "card");
        write(
            root,
            "projects/app/state.md",
            "# app\n\n## What\n<!-- comment -->\nAn **iOS** app,\nbuilt with `SwiftUI`.\n\nMore.\n\n## Architecture\nx\n",
        );
        write(
            root,
            "projects/empty/state.md",
            "# empty\n\n## What\n<!-- What the project is for. -->\n\n## Architecture\n",
        );
        fs::create_dir_all(root.join("projects/.hidden")).unwrap();

        let o = overview(&vault).unwrap();
        assert_eq!(
            o.projects
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["app", "empty"]
        );

        let app = &o.projects[0];
        assert_eq!(
            app.about.as_deref(),
            Some("An iOS app, built with SwiftUI.")
        );
        assert_eq!((app.items, app.decisions, app.sessions), (5, 3, 1));
        assert_eq!(app.statuses.get(&ItemStatus::Inbox), Some(&2));
        assert_eq!(app.statuses.get(&ItemStatus::Planned), None);
        assert_eq!(
            app.open.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["app-0003", "app-0005", "app-0002"]
        );
        assert_eq!(app.open[0].title, "Shipping: build 2");
        assert_eq!(
            app.recent_decisions
                .iter()
                .map(|r| r.title.as_str())
                .collect::<Vec<_>>(),
            ["Tenth", "Second", "First"]
        );

        let empty = &o.projects[1];
        assert_eq!(empty.about, None);
        assert_eq!((empty.items, empty.decisions, empty.sessions), (0, 0, 0));
    }

    #[test]
    fn about_is_truncated_at_a_word() {
        let long = format!("## What\n{}\n", "word ".repeat(60));
        let text = about(&long).unwrap();
        assert!(text.ends_with('…'));
        assert!(text.chars().count() <= ABOUT_LIMIT + 1);
        assert!(!text.contains("wor…"));
    }

    #[test]
    fn frontmatter_needs_delimiters_and_unquotes() {
        assert_eq!(frontmatter("id: x\n"), None);
        assert_eq!(
            frontmatter("---\nid: x\ntitle: \"a: b\"\n---\nbody"),
            Some(vec![("id", "x"), ("title", "a: b")])
        );
    }

    #[test]
    fn serializes_statuses_by_name() {
        let mut statuses = BTreeMap::new();
        statuses.insert(ItemStatus::DoneUnverified, 2);
        assert_eq!(
            serde_json::to_string(&statuses).unwrap(),
            r#"{"done-unverified":2}"#
        );
    }
}
