//! Every record in the vault and how they link, for `reprise graph`: projects, items,
//! decisions, handoff cards and notes as nodes; containment, `related` / `superseded_by` fields and
//! record ids mentioned in a body as links.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::Serialize;

use crate::record::Record;
use crate::report::markdown_files;
use crate::{Result, Vault};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    Project,
    Item,
    Decision,
    Session,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    /// Record id; `<project>` for a project, `<project>/sessions/<file stem>` for a handoff card,
    /// `notes/<path>` for a note.
    pub id: String,
    pub kind: NodeKind,
    pub project: Option<String>,
    pub title: String,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkKind {
    /// A project holds the record.
    Contains,
    /// `related` or `superseded_by`.
    Related,
    /// The record's body names the other record's id.
    Mentions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Link {
    /// Indices into `nodes`.
    pub source: usize,
    pub target: usize,
    pub kind: LinkKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
}

/// A record's text, kept until every id is known and links can be resolved.
struct Pending {
    index: usize,
    references: Vec<String>,
    body: String,
}

pub fn graph(vault: &Vault) -> Result<Graph> {
    let mut g = Graph::default();
    let mut pending = Vec::new();

    let mut projects: Vec<String> = std::fs::read_dir(vault.projects_dir())
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    projects.sort();

    for project in &projects {
        let p = g.nodes.len();
        g.nodes.push(Node {
            id: project.clone(),
            kind: NodeKind::Project,
            project: Some(project.clone()),
            title: project.clone(),
            status: None,
        });
        let dir = vault.project_dir(project);
        for (folder, kind) in [
            ("items", NodeKind::Item),
            ("decisions", NodeKind::Decision),
            ("sessions", NodeKind::Session),
        ] {
            for path in markdown_files(&dir.join(folder)) {
                let record = Record::read(&path)?;
                let stem = stem(&path);
                let (id, title) = match kind {
                    NodeKind::Session => (
                        format!("{project}/sessions/{stem}"),
                        format!(
                            "Handoff {}",
                            non_empty(record.text("date")).unwrap_or(&stem)
                        ),
                    ),
                    _ => {
                        let id = non_empty(record.text("id")).unwrap_or(&stem).to_owned();
                        let title = non_empty(record.text("title")).unwrap_or(&id).to_owned();
                        (id, title)
                    }
                };
                let index = g.nodes.len();
                g.nodes.push(Node {
                    id,
                    kind,
                    project: Some(project.clone()),
                    title,
                    status: non_empty(record.text("status")).map(str::to_owned),
                });
                g.links.push(Link {
                    source: p,
                    target: index,
                    kind: LinkKind::Contains,
                });
                let references = ["related", "superseded_by"]
                    .iter()
                    .filter_map(|k| record.get(k))
                    .flat_map(|v| v.items())
                    .collect();
                pending.push(Pending {
                    index,
                    references,
                    body: record.body,
                });
            }
        }
    }

    let notes_dir = vault.notes_dir();
    for path in note_files(&notes_dir) {
        let record = Record::read(&path)?;
        let rel = path.strip_prefix(&notes_dir).unwrap_or(&path);
        let title = record
            .body
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(|| stem(&path), str::to_owned);
        let index = g.nodes.len();
        g.nodes.push(Node {
            id: format!("notes/{}", rel.display()),
            kind: NodeKind::Note,
            project: None,
            title,
            status: None,
        });
        pending.push(Pending {
            index,
            references: Vec::new(),
            body: record.body,
        });
    }

    // Only records with an id can be linked to; sessions and notes only link out.
    let ids: HashMap<String, usize> = g
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| matches!(n.kind, NodeKind::Item | NodeKind::Decision))
        .map(|(i, n)| (n.id.clone(), i))
        .collect();
    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    for p in pending {
        let mentioned = id_candidates(&p.body)
            .into_iter()
            .filter(|c| !p.references.contains(c));
        let targets = p
            .references
            .iter()
            .map(|r| (r.clone(), LinkKind::Related))
            .chain(mentioned.map(|m| (m, LinkKind::Mentions)));
        for (id, kind) in targets {
            let Some(&target) = ids.get(&id) else {
                continue;
            };
            let pair = (p.index.min(target), p.index.max(target));
            if target != p.index && seen.insert(pair) {
                g.links.push(Link {
                    source: p.index,
                    target,
                    kind,
                });
            }
        }
    }
    Ok(g)
}

/// Words that look like record ids (`<prefix>-0001`, `<prefix>-D-0001`), in order, without repeats.
fn id_candidates(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        let word = word.trim_matches('-');
        let looks_like_id = word.rsplit_once('-').is_some_and(|(head, n)| {
            !head.is_empty() && n.len() >= 4 && n.chars().all(|c| c.is_ascii_digit())
        });
        if looks_like_id && !out.iter().any(|w| w == word) {
            out.push(word.to_owned());
        }
    }
    out
}

/// `**/*.md` under `dir`, sorted, without hidden files and folders.
fn note_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn non_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn nodes_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        let root = vault.root();
        w(
            root,
            "projects/api/items/api-0001-a.md",
            "---\nid: api-0001\ntitle: Login\nstatus: doing\nrelated: [api-D-0001, gone-0009]\n---\nSee api-0002 and api-D-0001.\n",
        );
        w(
            root,
            "projects/api/items/api-0002-b.md",
            "---\nid: api-0002\ntitle: Logout\nstatus: inbox\n---\n",
        );
        w(
            root,
            "projects/api/decisions/api-D-0001-c.md",
            "---\nid: api-D-0001\ntitle: Cookie\nstatus: superseded\nsuperseded_by: api-D-0002\nrelated: [api-0001]\n---\n",
        );
        w(
            root,
            "projects/api/sessions/2026-10-24-1000-aa.md",
            "---\ndate: 2026-10-24 10:00\n---\n## Touched records\n- api-0002\n",
        );
        w(
            root,
            "notes/rust/tools.md",
            "# Rust tools\nabout api-0001\n",
        );

        let g = graph(&vault).unwrap();
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "api",
                "api-0001",
                "api-0002",
                "api-D-0001",
                "api/sessions/2026-10-24-1000-aa",
                "notes/rust/tools.md"
            ]
        );
        assert_eq!(g.nodes[1].status.as_deref(), Some("doing"));
        assert_eq!(g.nodes[4].title, "Handoff 2026-10-24 10:00");
        assert_eq!(g.nodes[5].title, "Rust tools");
        assert_eq!(g.nodes[5].project, None);

        let links: Vec<(&str, &str, LinkKind)> = g
            .links
            .iter()
            .map(|l| (ids[l.source], ids[l.target], l.kind))
            .collect();
        assert_eq!(
            links,
            [
                ("api", "api-0001", LinkKind::Contains),
                ("api", "api-0002", LinkKind::Contains),
                ("api", "api-D-0001", LinkKind::Contains),
                ("api", "api/sessions/2026-10-24-1000-aa", LinkKind::Contains),
                // related once in either direction; unknown ids are dropped
                ("api-0001", "api-D-0001", LinkKind::Related),
                ("api-0001", "api-0002", LinkKind::Mentions),
                (
                    "api/sessions/2026-10-24-1000-aa",
                    "api-0002",
                    LinkKind::Mentions
                ),
                ("notes/rust/tools.md", "api-0001", LinkKind::Mentions),
            ]
        );
    }

    #[test]
    fn id_words() {
        assert_eq!(
            id_candidates("(api-D-0010), api-0002. web-app-0001 v-12 2026-10-24 api-0002"),
            ["api-D-0010", "api-0002", "web-app-0001"]
        );
    }
}
