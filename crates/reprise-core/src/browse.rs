//! Read-only lists of every item, decision and handoff card in the vault, with their bodies split
//! into sections, for `reprise items`, `reprise decisions` and `reprise sessions`.

use std::path::PathBuf;

use serde::Serialize;

use crate::overview::id_number;
use crate::record::Record;
use crate::{Result, Vault};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub id: String,
    pub project: String,
    pub title: String,
    /// `accepted` or `superseded`, as written; empty when missing.
    pub status: String,
    pub superseded_by: Option<String>,
    pub created: String,
    pub related: Vec<String>,
    pub files: Vec<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: String,
    pub project: String,
    pub title: String,
    /// As written; empty when missing.
    pub status: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub created: String,
    pub updated: String,
    pub captured_during: String,
    pub related: Vec<String>,
    pub files: Vec<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    /// The file name without `.md`: unique within its project.
    pub id: String,
    pub project: String,
    pub session: String,
    pub date: String,
    /// `handoff` or `transcript`, as written; empty when missing.
    pub source: String,
    pub sections: Vec<Section>,
}

/// A `## ` section of a body. Text before the first heading has an empty heading. Sections with
/// nothing in them are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    pub heading: String,
    pub blocks: Vec<Block>,
}

/// Inline Markdown (`code`, `**bold**`) is left in the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "text", rename_all = "kebab-case")]
pub enum Block {
    Paragraph(String),
    List(Vec<String>),
}

/// Every item, newest first (by `created`, then id number).
pub fn items(vault: &Vault) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    for (project, path) in records(vault, "items") {
        let Ok(r) = Record::read(&path) else { continue };
        let id = r.text("id").to_owned();
        if id.is_empty() {
            continue;
        }
        let list = |k: &str| r.get(k).map(|v| v.items()).unwrap_or_default();
        out.push(Item {
            title: Some(r.text("title"))
                .filter(|t| !t.is_empty())
                .unwrap_or(&id)
                .to_owned(),
            status: r.text("status").to_owned(),
            kind: r.text("type").to_owned(),
            created: r.text("created").to_owned(),
            updated: r.text("updated").to_owned(),
            captured_during: r.text("captured_during").to_owned(),
            related: list("related"),
            files: list("files"),
            sections: sections(&r.body),
            project,
            id,
        });
    }
    out.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then_with(|| id_number(&b.id).cmp(&id_number(&a.id)))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Every decision, newest first (by `created`, then id number).
pub fn decisions(vault: &Vault) -> Result<Vec<Decision>> {
    let mut out = Vec::new();
    for (project, path) in records(vault, "decisions") {
        let Ok(r) = Record::read(&path) else { continue };
        let id = r.text("id").to_owned();
        if id.is_empty() {
            continue;
        }
        let list = |k: &str| r.get(k).map(|v| v.items()).unwrap_or_default();
        let superseded_by = Some(r.text("superseded_by").to_owned()).filter(|s| !s.is_empty());
        out.push(Decision {
            title: Some(r.text("title"))
                .filter(|t| !t.is_empty())
                .unwrap_or(&id)
                .to_owned(),
            status: r.text("status").to_owned(),
            superseded_by,
            created: r.text("created").to_owned(),
            related: list("related"),
            files: list("files"),
            sections: sections(&r.body),
            project,
            id,
        });
    }
    out.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then_with(|| id_number(&b.id).cmp(&id_number(&a.id)))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Every handoff card, newest first (by `date`, then file name).
pub fn sessions(vault: &Vault) -> Result<Vec<Session>> {
    let mut out = Vec::new();
    for (project, path) in records(vault, "sessions") {
        let Ok(r) = Record::read(&path) else { continue };
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(Session {
            id: id.to_owned(),
            session: r.text("session").to_owned(),
            date: r.text("date").to_owned(),
            source: r.text("source").to_owned(),
            sections: sections(&r.body),
            project,
        });
    }
    out.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| b.id.cmp(&a.id))
            .then_with(|| a.project.cmp(&b.project))
    });
    Ok(out)
}

/// `(project, path)` of each `*.md` in `projects/<project>/<folder>/`, hidden ones skipped.
fn records(vault: &Vault, folder: &str) -> Vec<(String, PathBuf)> {
    let Ok(projects) = std::fs::read_dir(vault.projects_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for project in projects.flatten() {
        let Some(name) = project.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') || !project.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path().join(folder)) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let hidden = file.file_name().to_str().is_none_or(|n| n.starts_with('.'));
            if !hidden && path.extension().is_some_and(|x| x == "md") && path.is_file() {
                out.push((name.clone(), path));
            }
        }
    }
    out
}

/// Splits a body at `## ` headings into paragraphs and lists. Deeper headings become paragraphs,
/// nested and numbered list items are flattened into one list, and indented lines continue the
/// item above.
pub fn sections(body: &str) -> Vec<Section> {
    let mut out = vec![Section {
        heading: String::new(),
        blocks: Vec::new(),
    }];
    let mut para: Vec<&str> = Vec::new();
    let mut list: Vec<String> = Vec::new();

    fn flush(section: &mut Section, para: &mut Vec<&str>, list: &mut Vec<String>) {
        if !para.is_empty() {
            section.blocks.push(Block::Paragraph(para.join(" ")));
            para.clear();
        }
        if !list.is_empty() {
            section.blocks.push(Block::List(std::mem::take(list)));
        }
    }

    for line in body.lines() {
        let trimmed = line.trim();
        let current = out.last_mut().expect("never empty");
        if let Some(heading) = line.strip_prefix("## ") {
            flush(current, &mut para, &mut list);
            out.push(Section {
                heading: heading.trim().to_owned(),
                blocks: Vec::new(),
            });
        } else if trimmed.is_empty() || trimmed.starts_with("<!--") {
            flush(current, &mut para, &mut list);
        } else if let Some(item) = list_item(trimmed) {
            if !para.is_empty() {
                current.blocks.push(Block::Paragraph(para.join(" ")));
                para.clear();
            }
            list.push(item.to_owned());
        } else if line.starts_with([' ', '\t']) && !list.is_empty() {
            let last = list.last_mut().expect("not empty");
            last.push(' ');
            last.push_str(trimmed);
        } else {
            if !list.is_empty() {
                current.blocks.push(Block::List(std::mem::take(&mut list)));
            }
            para.push(trimmed.trim_start_matches('#').trim_start());
        }
    }
    let last = out.last_mut().expect("never empty");
    flush(last, &mut para, &mut list);
    out.retain(|s| !s.blocks.is_empty());
    out
}

/// The text of a `- `, `* ` or `1. ` list line.
fn list_item(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return Some(rest.trim());
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(rest.trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn splits_a_body_into_sections() {
        let body = "\nIntro line\nwraps here.\n\n## Context\n\n## Decision\n\nKeep `x`.\n\n- one\n  continues\n- two\n  - nested\n1. three\n### Sub\nAfter\n\n## Why\n<!-- hint -->\n";
        assert_eq!(
            sections(body),
            vec![
                Section {
                    heading: String::new(),
                    blocks: vec![Block::Paragraph("Intro line wraps here.".into())],
                },
                Section {
                    heading: "Decision".into(),
                    blocks: vec![
                        Block::Paragraph("Keep `x`.".into()),
                        Block::List(vec![
                            "one continues".into(),
                            "two".into(),
                            "nested".into(),
                            "three".into()
                        ]),
                        Block::Paragraph("Sub After".into()),
                    ],
                },
            ]
        );
    }

    #[test]
    fn lists_decisions_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        write(
            root,
            "projects/app/decisions/app-D-0002-b.md",
            "---\nid: app-D-0002\ntitle: Second\nstatus: superseded\nsuperseded_by: app-D-0010\ncreated: 2026-10-20\nrelated: [app-0001]\nfiles: [src/a.rs, src/b.rs]\n---\n\n## Decision\n\nUse a cookie.\n",
        );
        write(
            root,
            "projects/app/decisions/app-D-0010-c.md",
            "---\nid: app-D-0010\ntitle: Tenth\nstatus: accepted\nsuperseded_by:\ncreated: 2026-10-20\n---\n",
        );
        write(
            root,
            "projects/web/decisions/web-D-0001-a.md",
            "---\nid: web-D-0001\ntitle:\nstatus: accepted\ncreated: 2026-10-24\n---\n",
        );
        write(
            root,
            "projects/web/decisions/no-id.md",
            "---\ntitle: x\n---\n",
        );
        write(
            root,
            "projects/web/decisions/.hidden.md",
            "---\nid: h\n---\n",
        );

        let list = decisions(&vault).unwrap();
        let ids: Vec<_> = list.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["web-D-0001", "app-D-0010", "app-D-0002"]);
        // A missing title falls back to the id; an empty superseded_by is none.
        assert_eq!(list[0].title, "web-D-0001");
        assert_eq!(list[0].project, "web");
        assert_eq!(list[1].superseded_by, None);
        let second = &list[2];
        assert_eq!(second.status, "superseded");
        assert_eq!(second.superseded_by.as_deref(), Some("app-D-0010"));
        assert_eq!(second.related, ["app-0001"]);
        assert_eq!(second.files, ["src/a.rs", "src/b.rs"]);
        assert_eq!(second.sections.len(), 1);
    }

    #[test]
    fn lists_sessions_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        write(
            root,
            "projects/app/sessions/2026-10-24-1430-1a2b3c4d.md",
            "---\nsession: 1a2b3c4d\nproject: app\ndate: 2026-10-24 14:30\nsource: handoff\n---\n\n## Done\n- Shipped\n\n## Next step\n- Test it\n",
        );
        write(
            root,
            "projects/app/sessions/2026-10-25-0900-99999999.md",
            "---\nsession: 99999999\ndate: 2026-10-25 09:00\nsource: transcript\n---\n\n## Done\n- More\n",
        );
        write(root, "projects/app/sessions/notes.txt", "not a card");

        let list = sessions(&vault).unwrap();
        let ids: Vec<_> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["2026-10-25-0900-99999999", "2026-10-24-1430-1a2b3c4d"]
        );
        assert_eq!(list[0].source, "transcript");
        let card = &list[1];
        assert_eq!(card.session, "1a2b3c4d");
        assert_eq!(card.project, "app");
        let headings: Vec<_> = card.sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, ["Done", "Next step"]);
    }

    #[test]
    fn lists_items_with_their_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault(root);
        let store = |title: &str| {
            crate::store::add_item(
                &vault,
                "app",
                &crate::store::NewItem::note(title),
                chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
            )
            .unwrap()
        };
        fs::create_dir_all(vault.project_dir("app")).unwrap();
        store("First");
        store("Second");
        let list = items(&vault).unwrap();
        let ids: Vec<_> = list.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["app-0002", "app-0001"]);
        let second = &list[0];
        assert_eq!(second.title, "Second");
        assert_eq!(second.status, "inbox");
        assert_eq!(second.kind, "feature");
        assert_eq!(second.created, "2026-10-25");
        // Template headings with nothing under them are left out.
        assert!(second.sections.is_empty());
    }

    #[test]
    fn a_vault_without_projects_has_no_records() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        assert!(items(&vault).unwrap().is_empty());
        assert!(decisions(&vault).unwrap().is_empty());
        assert!(sessions(&vault).unwrap().is_empty());
    }
}
