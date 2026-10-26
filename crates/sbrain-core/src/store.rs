//! Writing records: items and their fields, decisions, handoff cards and dated rules, with id
//! allocation. Layout and fields: docs/vault-format.md.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::Serialize;

use crate::project::ProjectToml;
use crate::record::{Record, Value};
use crate::schema::{DecisionStatus, ItemStatus, ItemType};
use crate::{Error, Result, Vault};

/// Longest slug in a record's file name, in characters.
const SLUG_LIMIT: usize = 40;
/// List fields of an item; `+=` appends to them.
const LIST_FIELDS: [&str; 3] = ["aliases", "files", "related"];
/// Fields only sbrain writes.
const READ_ONLY: [&str; 4] = ["id", "created", "updated", "verified_at"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewItem {
    pub title: String,
    pub kind: ItemType,
    pub status: ItemStatus,
    /// Goes under `## Problem`.
    pub body: String,
    pub captured_during: String,
    pub aliases: Vec<String>,
    pub files: Vec<String>,
    pub related: Vec<String>,
}

impl NewItem {
    /// An inbox note with only a title, as a quick capture writes it.
    pub fn note(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            kind: ItemType::Feature,
            status: ItemStatus::Inbox,
            body: String::new(),
            captured_during: String::new(),
            aliases: Vec::new(),
            files: Vec::new(),
            related: Vec::new(),
        }
    }
}

/// What a write produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Written {
    pub id: String,
    pub project: String,
    /// Relative to the vault root.
    pub path: PathBuf,
    pub status: Option<ItemStatus>,
}

/// One field change: `key=value` replaces, `key+=a, b` appends to a list field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Set { key: String, value: String },
    Append { key: String, values: Vec<String> },
}

impl Change {
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = || {
            Error::InvalidChange(format!(
                "not understood: {text:?} (use key=value or key+=value)"
            ))
        };
        let (key, value) = text.split_once('=').ok_or_else(invalid)?;
        let (key, append) = match key.strip_suffix('+') {
            Some(k) => (k, true),
            None => (key, false),
        };
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Err(invalid());
        }
        let key = key.to_owned();
        Ok(if append {
            Self::Append {
                key,
                values: csv(value),
            }
        } else {
            Self::Set {
                key,
                value: value.to_owned(),
            }
        })
    }

    pub fn status(status: ItemStatus) -> Self {
        Self::Set {
            key: "status".into(),
            value: status.as_str().into(),
        }
    }
}

/// Writes a new item into `project` with the next free id. A new item cannot start closed.
pub fn add_item(vault: &Vault, project: &str, item: &NewItem, today: NaiveDate) -> Result<Written> {
    let dir = project_dir(vault, project)?;
    // Frontmatter values are one line each.
    let title = one_line(&item.title);
    let title = title.as_str();
    if title.is_empty() {
        return Err(Error::InvalidChange("an item needs a title".into()));
    }
    if item.status.is_closed() {
        return Err(Error::InvalidChange(
            "a new item starts as inbox, planned, doing or done-unverified".into(),
        ));
    }
    let prefix = ProjectToml::read(&dir.join("project.toml"))
        .unwrap_or_default()
        .prefix(project);
    let date = today.format("%Y-%m-%d").to_string();
    let (id, path) = create_numbered(&dir.join("items"), &prefix, title, |id| {
        let text = crate::scaffold::render(
            crate::scaffold::ITEM_MD,
            &[
                ("id", id),
                ("title", title),
                ("type", item.kind.as_str()),
                ("status", item.status.as_str()),
                ("date", &date),
                ("captured_during", &one_line(&item.captured_during)),
                ("body", item.body.trim()),
            ],
        );
        let mut record = Record::parse(&text).expect("the item template has frontmatter");
        record.set("aliases", Value::List(item.aliases.clone()));
        record.set("files", Value::List(item.files.clone()));
        record.set("related", Value::List(item.related.clone()));
        record.to_text()
    })?;
    Ok(Written {
        id,
        project: project.to_owned(),
        path: relative(vault, &path),
        status: Some(item.status),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewDecision {
    pub title: String,
    pub context: String,
    pub decision: String,
    pub why: String,
    pub alternatives: String,
    pub related: Vec<String>,
    pub files: Vec<String>,
    /// HEAD of the project's repository when the decision was taken; may be empty.
    pub commit: String,
}

/// Writes a new decision into `project` as `<prefix>-D-<n>`.
pub fn add_decision(
    vault: &Vault,
    project: &str,
    decision: &NewDecision,
    today: NaiveDate,
) -> Result<Written> {
    let dir = project_dir(vault, project)?;
    let title = one_line(&decision.title);
    let title = title.as_str();
    if title.is_empty() {
        return Err(Error::InvalidChange("a decision needs a title".into()));
    }
    let prefix = ProjectToml::read(&dir.join("project.toml"))
        .unwrap_or_default()
        .prefix(project);
    let date = today.format("%Y-%m-%d").to_string();
    let (id, path) = create_numbered(
        &dir.join("decisions"),
        &format!("{prefix}-D"),
        title,
        |id| {
            let text = crate::scaffold::render(
                crate::scaffold::DECISION_MD,
                &[
                    ("id", id),
                    ("title", title),
                    ("date", &date),
                    ("commit", &one_line(&decision.commit)),
                    ("body", decision.decision.trim()),
                ],
            );
            let mut record = Record::parse(&text).expect("the decision template has frontmatter");
            record.set("files", Value::List(decision.files.clone()));
            record.set("related", Value::List(decision.related.clone()));
            record.body = fill_section(&record.body, "Context", &decision.context);
            record.body = fill_section(&record.body, "Why", &decision.why);
            record.body = fill_section(&record.body, "Alternatives", &decision.alternatives);
            record.to_text()
        },
    )?;
    Ok(Written {
        id,
        project: project.to_owned(),
        path: relative(vault, &path),
        status: None,
    })
}

/// Fields of a decision that can be changed: the rest (`id`, `created`) only sbrain writes.
const DECISION_FIELDS: [&str; 6] = [
    "title",
    "status",
    "superseded_by",
    "files",
    "related",
    "commit",
];

/// Changes fields of an existing decision. A decision is replaced, not edited away: `status:
/// superseded` needs `superseded_by` naming another decision that exists. Decisions have no `updated`.
pub fn set_decision(vault: &Vault, id: &str, changes: &[Change]) -> Result<Written> {
    let (project, path) = find_decision(vault, id)?;
    let mut record = Record::read(&path)?;
    for change in changes {
        let key = match change {
            Change::Set { key, .. } | Change::Append { key, .. } => key.as_str(),
        };
        if !DECISION_FIELDS.contains(&key) {
            return Err(Error::InvalidChange(format!(
                "a decision's {key} is not changed by hand; these are: {}",
                DECISION_FIELDS.join(", ")
            )));
        }
        let multiline = match change {
            Change::Set { value, .. } => value.contains(['\n', '\r']),
            Change::Append { values, .. } => values.iter().any(|v| v.contains(['\n', '\r'])),
        };
        if multiline {
            return Err(Error::InvalidChange("a field value is one line".into()));
        }
        match change {
            Change::Set { key, value } if key == "status" => {
                let status: DecisionStatus = value.parse().map_err(|_| {
                    let all: Vec<_> = DecisionStatus::ALL.iter().map(|s| s.as_str()).collect();
                    Error::InvalidChange(format!("status must be one of: {}", all.join(", ")))
                })?;
                record.set(key, Value::Text(status.as_str().into()));
            }
            Change::Set { key, value } if key == "files" || key == "related" => {
                record.set(key, Value::List(csv(value)));
            }
            Change::Set { key, value } => record.set(key, Value::Text(value.trim().to_owned())),
            Change::Append { key, values } if key == "files" || key == "related" => {
                let mut list = record.get(key).map(Value::items).unwrap_or_default();
                for v in values {
                    if !list.contains(v) {
                        list.push(v.clone());
                    }
                }
                record.set(key, Value::List(list));
            }
            Change::Append { key, .. } => {
                return Err(Error::InvalidChange(format!(
                    "{key} is not a list; += works on files, related"
                )));
            }
        }
    }
    if record.text("title").trim().is_empty() {
        return Err(Error::InvalidChange("a decision needs a title".into()));
    }
    let by = record.text("superseded_by").trim().to_owned();
    if record.text("status") == DecisionStatus::Superseded.as_str() {
        if by.is_empty() {
            return Err(Error::InvalidChange(
                "a superseded decision needs superseded_by: the decision that replaces it".into(),
            ));
        }
        if by == id {
            return Err(Error::InvalidChange(
                "a decision cannot supersede itself".into(),
            ));
        }
        find_decision(vault, &by)
            .map_err(|_| Error::InvalidChange(format!("superseded_by names no decision: {by}")))?;
    } else if !by.is_empty() {
        return Err(Error::InvalidChange(
            "superseded_by is set only with status=superseded".into(),
        ));
    }
    crate::scaffold::write(&path, &record.to_text())?;
    Ok(Written {
        id: id.to_owned(),
        status: None,
        path: relative(vault, &path),
        project,
    })
}

/// A handoff card's sections, as `sbrain session write` takes them; each entry is one bullet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    pub done: Vec<String>,
    pub decisions: Vec<String>,
    pub open: Vec<String>,
    pub next_step: String,
    /// Ids of the records the session wrote or changed.
    pub touched: Vec<String>,
}

impl Card {
    /// The card's body: every heading of the template, in order, with its bullets.
    pub fn body(&self) -> String {
        let bullets = |lines: &[String]| {
            lines
                .iter()
                .map(|l| one_line(l))
                .filter(|l| !l.is_empty())
                .map(|l| format!("- {}", l.trim_start_matches("- ")))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let next = self.next_step.trim();
        [
            ("Done", bullets(&self.done)),
            ("Decisions", bullets(&self.decisions)),
            ("Open", bullets(&self.open)),
            ("Next step", next.to_owned()),
            ("Touched records", bullets(&self.touched)),
        ]
        .iter()
        .map(|(h, text)| {
            if text.is_empty() {
                format!("## {h}\n")
            } else {
                format!("## {h}\n\n{text}\n")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.done.is_empty()
            && self.decisions.is_empty()
            && self.open.is_empty()
            && self.next_step.trim().is_empty()
            && self.touched.is_empty()
    }
}

/// Who wrote a handoff card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CardSource {
    /// The agent, at the end of the session.
    Handoff,
    /// `sbrain sweep`, from the session's transcript.
    Transcript,
}

impl CardSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Transcript => "transcript",
        }
    }
}

/// The handoff card of a session in `project`, by the first 8 characters of its id.
pub fn find_card(vault: &Vault, project: &str, session: &str) -> Option<PathBuf> {
    let session8 = crate::text::take_chars(session, 8);
    let suffix = format!("-{session8}.md");
    let mut hits: Vec<PathBuf> = std::fs::read_dir(vault.project_dir(project).join("sessions"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// Writes a session's handoff card: `body` under the card's frontmatter. A card for the same
/// session is rewritten in place; otherwise it is `sessions/<YYYY-MM-DD-HHMM>-<session8>.md`, named
/// after `datetime` (`YYYY-MM-DD HH:MM`).
pub fn write_card(
    vault: &Vault,
    project: &str,
    session: &str,
    datetime: &str,
    source: CardSource,
    body: &str,
) -> Result<Written> {
    let dir = project_dir(vault, project)?;
    let session8 = crate::text::take_chars(session.trim(), 8);
    let valid_session = !session8.is_empty()
        && session8
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    let stamp: String = datetime
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    if !valid_session || stamp.len() != 12 {
        return Err(Error::InvalidChange(format!(
            "a card needs a session id and a YYYY-MM-DD HH:MM time, not {session:?} and {datetime:?}"
        )));
    }
    let path = find_card(vault, project, session8).unwrap_or_else(|| {
        dir.join("sessions").join(format!(
            "{}-{}-{}-{}-{session8}.md",
            &stamp[0..4],
            &stamp[4..6],
            &stamp[6..8],
            &stamp[8..12]
        ))
    });
    let text = crate::scaffold::render(
        crate::scaffold::SESSION_MD,
        &[
            ("session", session8),
            ("project", project),
            ("datetime", &one_line(datetime)),
            ("source", source.as_str()),
        ],
    );
    let mut record = Record::parse(&text).expect("the card template has frontmatter");
    record.body = format!("\n{}\n", body.trim());
    crate::scaffold::write(&path, &record.to_text())?;
    Ok(Written {
        id: session8.to_owned(),
        project: project.to_owned(),
        path: relative(vault, &path),
        status: None,
    })
}

/// Adds a dated one-line rule: to the project's `gotchas.md`, or with no project to
/// `profile/rules.md`. A rule already there with the same text is not added again; `None` then.
pub fn add_rule(
    vault: &Vault,
    project: Option<&str>,
    rule: &str,
    today: NaiveDate,
) -> Result<Option<PathBuf>> {
    let rule = one_line(rule);
    let rule = rule.trim_start_matches("- ").trim();
    if rule.is_empty() {
        return Err(Error::InvalidChange("a rule needs text".into()));
    }
    let path = match project {
        Some(p) => project_dir(vault, p)?.join("gotchas.md"),
        None => vault.profile_dir().join("rules.md"),
    };
    let current = crate::safe::read_to_string(&path).unwrap_or_default();
    let known = current.lines().any(|l| {
        l.split_once(": ")
            .is_some_and(|(_, text)| text.trim() == rule)
    });
    if known {
        return Ok(None);
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("- {}: {rule}\n", today.format("%Y-%m-%d")));
    crate::scaffold::write(&path, &text)?;
    Ok(Some(relative(vault, &path)))
}

/// Where rules found in transcripts wait for the user: not read into any agent's context.
pub const PROPOSED_RULES: &str = "proposed-rules.md";
const PROPOSALS_HEAD: &str = "# Proposed rules\n\nRules the sweep found in sessions. They are not given to agents until \
the user accepts one: `sbrain rule accept <id>`; `sbrain rule reject <id>` drops it.\n\n";

/// A rule the sweep proposed, waiting for the user's yes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proposal {
    /// Stable while the rule's text and scope stay the same: 8 hex digits.
    pub id: String,
    /// The project it is for; `None` for every project.
    pub project: Option<String>,
    pub rule: String,
    /// `YYYY-MM-DD` it was proposed.
    pub date: String,
    /// The session it came from (the first 8 characters of its id).
    pub session: String,
    /// The proposals file, relative to the vault.
    pub path: PathBuf,
}

fn proposals_file(vault: &Vault, project: Option<&str>) -> Result<PathBuf> {
    Ok(match project {
        Some(p) => project_dir(vault, p)?.join(PROPOSED_RULES),
        None => vault.profile_dir().join(PROPOSED_RULES),
    })
}

fn proposal_id(project: Option<&str>, rule: &str) -> String {
    // FNV-1a: the same id on every machine and version, unlike std's hasher.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{}\u{0}{rule}", project.unwrap_or("")).bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", h >> 32)
}

/// Proposes a rule found in a transcript instead of putting it in effect: it goes to
/// `proposed-rules.md` (the project's, or `profile/` with no project), which no agent reads, and waits
/// for `accept_rule`. A rule already in effect or already proposed is not added again; `None` then.
pub fn propose_rule(
    vault: &Vault,
    project: Option<&str>,
    rule: &str,
    session: &str,
    today: NaiveDate,
) -> Result<Option<PathBuf>> {
    let rule = one_line(rule);
    let rule = rule.trim_start_matches("- ").trim();
    if rule.is_empty() {
        return Err(Error::InvalidChange("a rule needs text".into()));
    }
    let in_effect = match project {
        Some(p) => project_dir(vault, p)?.join("gotchas.md"),
        None => vault.profile_dir().join("rules.md"),
    };
    let known = |text: &str| {
        text.lines().any(|l| {
            l.split_once(": ")
                .is_some_and(|(_, text)| text.trim() == rule)
        })
    };
    if known(&crate::safe::read_to_string(&in_effect).unwrap_or_default()) {
        return Ok(None);
    }
    let path = proposals_file(vault, project)?;
    let mut text = crate::safe::read_to_string(&path).unwrap_or_else(|_| PROPOSALS_HEAD.to_owned());
    if known(&text) {
        return Ok(None);
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let session: String = session
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    text.push_str(&format!(
        "- {} ({session}): {rule}\n",
        today.format("%Y-%m-%d")
    ));
    crate::scaffold::write(&path, &text)?;
    Ok(Some(relative(vault, &path)))
}

/// Every rule waiting for the user: the general ones first, then each project's.
pub fn proposals(vault: &Vault) -> Result<Vec<Proposal>> {
    let mut scopes: Vec<Option<String>> = vec![None];
    if let Ok(entries) = std::fs::read_dir(vault.projects_dir()) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        scopes.extend(names.into_iter().map(Some));
    }
    let mut out = Vec::new();
    for project in scopes {
        let path = proposals_file(vault, project.as_deref())?;
        let Ok(text) = crate::safe::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("- ") else {
                continue;
            };
            let Some((head, rule)) = rest.split_once(": ") else {
                continue;
            };
            let (date, session) = match head.split_once(" (") {
                Some((d, s)) => (d, s.trim_end_matches(')')),
                None => (head, ""),
            };
            out.push(Proposal {
                id: proposal_id(project.as_deref(), rule.trim()),
                project: project.clone(),
                rule: rule.trim().to_owned(),
                date: date.to_owned(),
                session: session.to_owned(),
                path: relative(vault, &path),
            });
        }
    }
    Ok(out)
}

fn take_proposal(vault: &Vault, id: &str) -> Result<(Proposal, PathBuf)> {
    let found = proposals(vault)?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| Error::RecordNotFound(format!("proposed rule {id}")))?;
    let path = vault.root().join(&found.path);
    let text = crate::safe::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let kept: String = text
        .lines()
        .filter(|l| {
            !(l.starts_with("- ")
                && l.split_once(": ")
                    .is_some_and(|(_, r)| r.trim() == found.rule))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    crate::scaffold::write(&path, &kept)?;
    Ok((found, path))
}

/// Puts a proposed rule in effect: into `gotchas.md` or `profile/rules.md`, dated today, and out of the
/// proposals. Returns the files changed, relative to the vault.
pub fn accept_rule(vault: &Vault, id: &str, today: NaiveDate) -> Result<Vec<PathBuf>> {
    let (p, proposals) = take_proposal(vault, id)?;
    let mut files = vec![relative(vault, &proposals)];
    files.extend(add_rule(vault, p.project.as_deref(), &p.rule, today)?);
    Ok(files)
}

/// Drops a proposed rule. Returns the file changed, relative to the vault.
pub fn reject_rule(vault: &Vault, id: &str) -> Result<PathBuf> {
    let (_, proposals) = take_proposal(vault, id)?;
    Ok(relative(vault, &proposals))
}

/// Adds a line to an item's `## Notes` and makes `updated` today.
pub fn note_item(vault: &Vault, id: &str, note: &str, today: NaiveDate) -> Result<Written> {
    let (project, path) = find_item(vault, id)?;
    let mut record = Record::read(&path)?;
    let line = format!("- {}: {}", today.format("%Y-%m-%d"), one_line(note));
    record.body = append_to_section(&record.body, "Notes", &line);
    record.set("updated", Value::Text(today.format("%Y-%m-%d").to_string()));
    crate::scaffold::write(&path, &record.to_text())?;
    Ok(Written {
        id: id.to_owned(),
        status: record.text("status").parse().ok(),
        path: relative(vault, &path),
        project,
    })
}

/// Writes a new `<prefix>-NNNN-<slug>.md` in `dir` with the next free number; `text` makes the file
/// for an id. A file another writer took first moves on to the one after it.
fn create_numbered(
    dir: &Path,
    prefix: &str,
    title: &str,
    text: impl Fn(&str) -> String,
) -> Result<(String, PathBuf)> {
    create_dir(dir)?;
    for _ in 0..20 {
        let id = format!("{prefix}-{:04}", next_number(dir, prefix));
        let path = dir.join(format!("{id}-{}.md", slugify(title)));
        // `create_new` never opens an existing name, a planted symlink included.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text(&id).as_bytes())
                    .map_err(|source| Error::Io {
                        path: path.clone(),
                        source,
                    })?;
                return Ok((id, path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(Error::Io { path, source }),
        }
    }
    Err(Error::InvalidChange(format!(
        "no free id found in {}",
        dir.display()
    )))
}

/// `text` put under the `## heading` of `body` (before what is already there). Unchanged when the
/// text is empty; the heading is added at the end when it is missing.
pub(crate) fn fill_section(body: &str, heading: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return body.to_owned();
    }
    let marker = format!("## {heading}\n");
    match body.find(&marker) {
        Some(at) => {
            let insert = at + marker.len();
            format!("{}\n{text}\n{}", &body[..insert], &body[insert..])
        }
        None => format!("{}\n## {heading}\n\n{text}\n", body.trim_end()),
    }
}

/// `line` added at the end of the `## heading` section of `body` (the heading is added at the end
/// when it is missing).
pub(crate) fn append_to_section(body: &str, heading: &str, line: &str) -> String {
    let marker = format!("## {heading}\n");
    let Some(at) = body.find(&marker) else {
        return format!("{}\n\n## {heading}\n\n{line}\n", body.trim_end());
    };
    let start = at + marker.len();
    let end = body[start..]
        .find("\n## ")
        .map_or(body.len(), |i| start + i + 1);
    let section = body[start..end].trim_end();
    format!(
        "{}{section}\n{line}\n{}{}",
        &body[..start],
        if end < body.len() { "\n" } else { "" },
        &body[end..]
    )
}

/// Changes fields of an existing item. `verified` is never set by hand, and sbrain's own fields
/// (`id`, `created`, `updated`, `verified_at`) are refused. `updated` becomes today.
pub fn set_item(vault: &Vault, id: &str, changes: &[Change], today: NaiveDate) -> Result<Written> {
    let (project, path) = find_item(vault, id)?;
    let mut record = Record::read(&path)?;
    for change in changes {
        let multiline = match change {
            Change::Set { value, .. } => value.contains(['\n', '\r']),
            Change::Append { values, .. } => values.iter().any(|v| v.contains(['\n', '\r'])),
        };
        if multiline {
            return Err(Error::InvalidChange("a field value is one line".into()));
        }
        let key = match change {
            Change::Set { key, .. } | Change::Append { key, .. } => key.as_str(),
        };
        if READ_ONLY.contains(&key) {
            return Err(Error::InvalidChange(format!(
                "{key} is not changed by hand"
            )));
        }
        match change {
            Change::Set { key, value } if key == "status" => {
                let status: ItemStatus = value.parse().map_err(|_| {
                    let all: Vec<_> = ItemStatus::ALL.iter().map(|s| s.as_str()).collect();
                    Error::InvalidChange(format!("status must be one of: {}", all.join(", ")))
                })?;
                if !status.settable_by_hand() {
                    return Err(Error::InvalidChange(
                        "verified is only set when the verify command passes".into(),
                    ));
                }
                record.set(key, Value::Text(status.as_str().into()));
            }
            Change::Set { key, value } if key == "type" => {
                let kind: ItemType = value.parse().map_err(|_| {
                    let all: Vec<_> = ItemType::ALL.iter().map(|t| t.as_str()).collect();
                    Error::InvalidChange(format!("type must be one of: {}", all.join(", ")))
                })?;
                record.set(key, Value::Text(kind.as_str().into()));
            }
            Change::Set { key, value } if LIST_FIELDS.contains(&key.as_str()) => {
                record.set(key, Value::List(csv(value)));
            }
            Change::Set { key, value } => record.set(key, Value::Text(value.trim().to_owned())),
            Change::Append { key, values } if LIST_FIELDS.contains(&key.as_str()) => {
                let mut list = record.get(key).map(Value::items).unwrap_or_default();
                for v in values {
                    if !list.contains(v) {
                        list.push(v.clone());
                    }
                }
                record.set(key, Value::List(list));
            }
            Change::Append { key, .. } => {
                return Err(Error::InvalidChange(format!(
                    "{key} is not a list; += works on {}",
                    LIST_FIELDS.join(", ")
                )));
            }
        }
    }
    record.set("updated", Value::Text(today.format("%Y-%m-%d").to_string()));
    crate::scaffold::write(&path, &record.to_text())?;
    Ok(Written {
        id: id.to_owned(),
        status: record.text("status").parse().ok(),
        path: relative(vault, &path),
        project,
    })
}

/// The project and file of the item with this id. Ids carry the project's prefix, which need not
/// be its name, so every project's `items/` is looked at.
pub fn find_item(vault: &Vault, id: &str) -> Result<(String, PathBuf)> {
    find_record(vault, id, "items")
}

/// The project and file of the decision with this id, as [`find_item`] finds items.
pub fn find_decision(vault: &Vault, id: &str) -> Result<(String, PathBuf)> {
    find_record(vault, id, "decisions")
}

fn find_record(vault: &Vault, id: &str, folder: &str) -> Result<(String, PathBuf)> {
    let wanted = format!("{id}-");
    let Ok(projects) = std::fs::read_dir(vault.projects_dir()) else {
        return Err(Error::RecordNotFound(id.to_owned()));
    };
    let mut names: Vec<_> = projects
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    for name in names {
        let Ok(files) = std::fs::read_dir(vault.project_dir(&name).join(folder)) else {
            continue;
        };
        for file in files.flatten() {
            let file_name = file.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if file_name.starts_with(&wanted)
                && file_name.ends_with(".md")
                && Record::read(&file.path()).is_ok_and(|r| r.text("id") == id)
            {
                return Ok((name, file.path()));
            }
        }
    }
    Err(Error::RecordNotFound(id.to_owned()))
}

/// The title folded to ASCII, lowercase, words joined by `-`, at most 40 characters; `item` when
/// nothing is left.
pub fn slugify(title: &str) -> String {
    let slug = crate::text::tokens(title).join("-");
    let cut = crate::text::take_chars(&slug, SLUG_LIMIT).trim_end_matches('-');
    if cut.is_empty() {
        "item".to_owned()
    } else {
        cut.to_owned()
    }
}

/// One more than the highest `<prefix>-NNNN` among the files of `dir`.
fn next_number(dir: &Path, prefix: &str) -> u32 {
    let start = format!("{prefix}-");
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .filter_map(|n| {
                    let rest = n.strip_prefix(&start)?;
                    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    (digits.len() == 4)
                        .then(|| digits.parse::<u32>().ok())
                        .flatten()
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
        + 1
}

fn project_dir(vault: &Vault, project: &str) -> Result<PathBuf> {
    let valid = !project.is_empty() && !project.starts_with('.') && !project.contains(['/', '\\']);
    let dir = vault.project_dir(project);
    if valid && dir.is_dir() {
        Ok(dir)
    } else {
        Err(Error::ProjectNotFound(project.to_owned()))
    }
}

fn create_dir(path: &Path) -> Result<()> {
    crate::safe::create_dir_all(path)
}

fn relative(vault: &Vault, path: &Path) -> PathBuf {
    path.strip_prefix(vault.root())
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

/// Line breaks turned into spaces, and the ends trimmed.
fn one_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn csv(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn vault_with(projects: &[(&str, Option<&str>)]) -> (tempfile::TempDir, Vault) {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        for (name, prefix) in projects {
            let dir = vault.project_dir(name);
            fs::create_dir_all(&dir).unwrap();
            if let Some(p) = prefix {
                fs::write(dir.join("project.toml"), format!("prefix = \"{p}\"\n")).unwrap();
            }
        }
        (tmp, vault)
    }

    #[test]
    fn slugs_are_ascii_and_short() {
        assert_eq!(
            slugify("Davet linki: e-postayla İŞ çözümü"),
            "davet-linki-e-postayla-is-cozumu"
        );
        assert_eq!(slugify("!!!"), "item");
        let long = slugify("one two three four five six seven eight nine ten");
        assert!(long.chars().count() <= 40 && !long.ends_with('-'), "{long}");
    }

    #[test]
    fn adds_items_with_the_next_id() {
        let (_tmp, vault) = vault_with(&[("web app", Some("web"))]);
        let items = vault.project_dir("web app").join("items");
        fs::create_dir_all(&items).unwrap();
        fs::write(items.join("web-0007-old.md"), "---\nid: web-0007\n---\n").unwrap();

        let mut item = NewItem::note("  Login fails after:\n timeout ");
        item.body = "It fails.".into();
        item.related = vec!["web-0007".into()];
        let w = add_item(&vault, "web app", &item, day("2026-10-25")).unwrap();
        assert_eq!(w.id, "web-0008");
        assert_eq!(
            w.path,
            PathBuf::from("projects/web app/items/web-0008-login-fails-after-timeout.md")
        );
        let text = fs::read_to_string(vault.root().join(&w.path)).unwrap();
        assert!(text.starts_with("---\nid: web-0008\ntitle: Login fails after: timeout\ntype: feature\nstatus: inbox\ncreated: 2026-10-25\nupdated: 2026-10-25\naliases: []\n"), "{text}");
        assert!(
            text.contains("related: [web-0007]\ncaptured_during:\n---\n"),
            "{text}"
        );
        assert!(text.contains("## Problem\n\nIt fails.\n"), "{text}");

        let next = add_item(
            &vault,
            "web app",
            &NewItem::note("Second"),
            day("2026-10-25"),
        )
        .unwrap();
        assert_eq!(next.id, "web-0009");
    }

    #[test]
    fn refuses_bad_new_items() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        let today = day("2026-10-25");
        assert!(matches!(
            add_item(&vault, "missing", &NewItem::note("x"), today),
            Err(Error::ProjectNotFound(_))
        ));
        assert!(matches!(
            add_item(&vault, "../app", &NewItem::note("x"), today),
            Err(Error::ProjectNotFound(_))
        ));
        assert!(matches!(
            add_item(&vault, "app", &NewItem::note("  "), today),
            Err(Error::InvalidChange(_))
        ));
        let mut done = NewItem::note("x");
        done.status = ItemStatus::Verified;
        assert!(matches!(
            add_item(&vault, "app", &done, today),
            Err(Error::InvalidChange(_))
        ));
        // Without a project.toml the prefix comes from the name.
        assert_eq!(
            add_item(&vault, "app", &NewItem::note("x"), today)
                .unwrap()
                .id,
            "app-0001"
        );
    }

    #[test]
    fn sets_fields_and_keeps_the_rest() {
        let (_tmp, vault) = vault_with(&[("app", Some("a"))]);
        let w = add_item(&vault, "app", &NewItem::note("Thing"), day("2026-10-20")).unwrap();
        let changes = [
            Change::status(ItemStatus::Planned),
            Change::parse("files=src/a.rs, src/b.rs").unwrap(),
            Change::parse("related+=a-0002").unwrap(),
            Change::parse("related+=a-0002, a-0003").unwrap(),
            Change::parse("verify=cargo test -p app").unwrap(),
        ];
        let out = set_item(&vault, "a-0001", &changes, day("2026-10-25")).unwrap();
        assert_eq!(out.status, Some(ItemStatus::Planned));
        assert_eq!(out.project, "app");
        let r = Record::read(&vault.root().join(&w.path)).unwrap();
        assert_eq!(r.text("status"), "planned");
        assert_eq!(r.text("created"), "2026-10-20");
        assert_eq!(r.text("updated"), "2026-10-25");
        assert_eq!(r.text("verify"), "cargo test -p app");
        assert_eq!(r.get("files").unwrap().items(), ["src/a.rs", "src/b.rs"]);
        assert_eq!(r.get("related").unwrap().items(), ["a-0002", "a-0003"]);
        assert!(r.body.contains("## Problem"));
    }

    #[test]
    fn refuses_bad_changes() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        let today = day("2026-10-25");
        add_item(&vault, "app", &NewItem::note("Thing"), today).unwrap();
        let set = |c: &str| set_item(&vault, "app-0001", &[Change::parse(c).unwrap()], today);
        assert!(matches!(
            set("status=verified"),
            Err(Error::InvalidChange(_))
        ));
        assert!(matches!(set("status=done"), Err(Error::InvalidChange(_))));
        assert!(matches!(set("type=chore"), Err(Error::InvalidChange(_))));
        assert!(matches!(set("id=x"), Err(Error::InvalidChange(_))));
        assert!(matches!(
            set("updated=2020-01-01"),
            Err(Error::InvalidChange(_))
        ));
        assert!(matches!(set("title+=x"), Err(Error::InvalidChange(_))));
        assert!(matches!(
            set_item(&vault, "app-0099", &[], today),
            Err(Error::RecordNotFound(_))
        ));
        assert!(matches!(
            set("verify=a\nstatus: verified"),
            Err(Error::InvalidChange(_))
        ));
        assert!(Change::parse("no equals").is_err());
        assert!(Change::parse("Bad=x").is_err());
        assert!(set("status=dropped").is_ok());
    }

    #[test]
    fn adds_decisions_with_their_sections() {
        let (_tmp, vault) = vault_with(&[("app", Some("a"))]);
        let d = NewDecision {
            title: "Keep the session in a cookie".into(),
            context: "Tokens leaked.".into(),
            decision: "Use an httpOnly cookie.".into(),
            why: "Scripts cannot read it.".into(),
            alternatives: String::new(),
            related: vec!["a-0001".into()],
            files: vec![],
            commit: "abc1234".into(),
        };
        let w = add_decision(&vault, "app", &d, day("2026-10-25")).unwrap();
        assert_eq!(w.id, "a-D-0001");
        let r = Record::read(&vault.root().join(&w.path)).unwrap();
        assert_eq!(r.text("status"), "accepted");
        assert_eq!(r.text("commit"), "abc1234");
        assert_eq!(r.get("related").unwrap().items(), ["a-0001"]);
        assert!(
            r.body.contains("## Context\n\nTokens leaked.\n"),
            "{}",
            r.body
        );
        assert!(
            r.body.contains("## Decision\n\nUse an httpOnly cookie.\n"),
            "{}",
            r.body
        );
        assert!(
            r.body.contains("## Why\n\nScripts cannot read it.\n"),
            "{}",
            r.body
        );
        assert!(r.body.contains("## Alternatives"), "{}", r.body);
        let next = add_decision(&vault, "app", &d, day("2026-10-25")).unwrap();
        assert_eq!(next.id, "a-D-0002");
    }

    #[test]
    fn cards_are_rewritten_in_place() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        let w = write_card(
            &vault,
            "app",
            "1a2b3c4d-5e6f",
            "2026-10-25 14:30",
            CardSource::Transcript,
            "## Done\n\n- x\n",
        )
        .unwrap();
        assert_eq!(
            w.path,
            PathBuf::from("projects/app/sessions/2026-10-25-1430-1a2b3c4d.md")
        );
        let again = write_card(
            &vault,
            "app",
            "1a2b3c4d",
            "2026-10-25 16:00",
            CardSource::Transcript,
            "## Done\n\n- y\n",
        )
        .unwrap();
        assert_eq!(again.path, w.path);
        let r = Record::read(&vault.root().join(&w.path)).unwrap();
        assert_eq!(r.text("source"), "transcript");
        assert_eq!(r.text("date"), "2026-10-25 16:00");
        assert!(r.body.contains("- y") && !r.body.contains("- x"));
        assert!(
            write_card(
                &vault,
                "app",
                "../x",
                "2026-10-25 14:30",
                CardSource::Handoff,
                ""
            )
            .is_err()
        );
        assert!(write_card(&vault, "app", "1a2b", "today", CardSource::Handoff, "").is_err());
    }

    /// A remote can sync a symlink in place of `gotchas.md`; a rule never reads or writes through it.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_gotchas_file_is_refused() {
        let (tmp, vault) = vault_with(&[("app", None)]);
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "private\n").unwrap();
        let gotchas = vault.project_dir("app").join("gotchas.md");
        let _ = fs::remove_file(&gotchas);
        std::os::unix::fs::symlink(&outside, &gotchas).unwrap();
        assert!(add_rule(&vault, Some("app"), "A rule.", day("2026-10-25")).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "private\n");
        assert!(gotchas.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn proposed_rules_wait_until_accepted_or_rejected() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        let today = day("2026-10-26");
        let p = propose_rule(
            &vault,
            Some("app"),
            "- Measure first.",
            "1a2b3c4d-xyz",
            today,
        )
        .unwrap()
        .unwrap();
        assert_eq!(p, PathBuf::from("projects/app/proposed-rules.md"));
        propose_rule(
            &vault,
            None,
            "Answer in short sentences.",
            "9f9f9f9f",
            today,
        )
        .unwrap();
        assert_eq!(
            propose_rule(&vault, Some("app"), "Measure first.", "other", today).unwrap(),
            None,
            "proposed once"
        );
        let waiting = proposals(&vault).unwrap();
        assert_eq!(waiting.len(), 2);
        assert_eq!(waiting[0].project, None, "general rules first");
        let app = waiting
            .iter()
            .find(|w| w.project.is_some())
            .unwrap()
            .clone();
        assert_eq!(
            (app.rule.as_str(), app.session.as_str()),
            ("Measure first.", "1a2b3c4d")
        );
        assert_eq!(app.id.len(), 8);
        assert_eq!(proposals(&vault).unwrap()[1].id, app.id, "ids are stable");

        let files = accept_rule(&vault, &app.id, day("2026-10-27")).unwrap();
        assert!(files.contains(&PathBuf::from("projects/app/gotchas.md")));
        let gotchas = fs::read_to_string(vault.project_dir("app").join("gotchas.md")).unwrap();
        assert!(
            gotchas.ends_with("- 2026-10-27: Measure first.\n"),
            "{gotchas}"
        );
        assert_eq!(proposals(&vault).unwrap().len(), 1);
        assert_eq!(
            propose_rule(&vault, Some("app"), "Measure first.", "x", today).unwrap(),
            None,
            "in effect: not proposed again"
        );

        let general = proposals(&vault).unwrap()[0].id.clone();
        reject_rule(&vault, &general).unwrap();
        assert!(proposals(&vault).unwrap().is_empty());
        let rules = fs::read_to_string(vault.profile_dir().join("rules.md")).unwrap_or_default();
        assert!(
            !rules.contains("short sentences"),
            "rejected: never in effect"
        );
        assert!(matches!(
            accept_rule(&vault, "deadbeef", today),
            Err(Error::RecordNotFound(_))
        ));
    }

    #[test]
    fn rules_are_dated_and_not_repeated() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        fs::write(
            vault.project_dir("app").join("gotchas.md"),
            "# app\n\n- 2026-10-01: Old rule.",
        )
        .unwrap();
        let today = day("2026-10-25");
        assert!(
            add_rule(&vault, Some("app"), "- New rule.", today)
                .unwrap()
                .is_some()
        );
        assert!(
            add_rule(&vault, Some("app"), "Old rule.", today)
                .unwrap()
                .is_none()
        );
        let text = fs::read_to_string(vault.project_dir("app").join("gotchas.md")).unwrap();
        assert_eq!(
            text,
            "# app\n\n- 2026-10-01: Old rule.\n- 2026-10-25: New rule.\n"
        );
        let general = add_rule(&vault, None, "Read before writing.", today)
            .unwrap()
            .unwrap();
        assert_eq!(general, PathBuf::from("profile/rules.md"));
        assert!(add_rule(&vault, Some("app"), " ", today).is_err());
    }

    #[test]
    fn notes_go_under_their_heading() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        let w = add_item(&vault, "app", &NewItem::note("Thing"), day("2026-10-20")).unwrap();
        note_item(&vault, "app-0001", "first", day("2026-10-24")).unwrap();
        note_item(&vault, "app-0001", "second", day("2026-10-25")).unwrap();
        let r = Record::read(&vault.root().join(&w.path)).unwrap();
        assert!(
            r.body
                .ends_with("## Notes\n\n- 2026-10-24: first\n- 2026-10-25: second\n"),
            "{}",
            r.body
        );
        assert_eq!(r.text("updated"), "2026-10-25");
        let body = append_to_section("## Notes\n\n## After\n", "Notes", "- n");
        assert_eq!(body, "## Notes\n\n- n\n\n## After\n");
    }

    #[test]
    fn decisions_change_and_are_superseded_by_another() {
        let (_tmp, vault) = vault_with(&[("app", None)]);
        let today = day("2026-10-26");
        let d = |title: &str| NewDecision {
            title: title.into(),
            decision: "Do it.".into(),
            ..NewDecision::default()
        };
        let old = add_decision(&vault, "app", &d("Session in a cookie"), today).unwrap();
        let new = add_decision(&vault, "app", &d("Session in a token"), today).unwrap();
        let item = add_item(&vault, "app", &NewItem::note("Log out"), today).unwrap();
        let set = |id: &str, changes: &[&str]| {
            let changes: Vec<Change> = changes.iter().map(|c| Change::parse(c).unwrap()).collect();
            set_decision(&vault, id, &changes)
        };
        let err = |r: Result<Written>| r.unwrap_err().to_string();

        assert!(err(set(&old.id, &["status=superseded"])).contains("needs superseded_by"));
        assert!(
            err(set(
                &old.id,
                &["status=superseded", "superseded_by=app-D-0099"]
            ))
            .contains("names no decision")
        );
        assert!(
            err(set(
                &old.id,
                &["status=superseded", &format!("superseded_by={}", old.id)]
            ))
            .contains("itself")
        );
        assert!(err(set(&old.id, &["status=gone"])).contains("status must be one of"));
        assert!(err(set(&old.id, &["created=2020-01-01"])).contains("not changed by hand"));
        assert!(err(set(&old.id, &["id=x"])).contains("not changed by hand"));
        assert!(err(set(&old.id, &["title=a\nb"])).contains("one line"));
        assert!(err(set(&old.id, &[&format!("superseded_by={}", new.id)])).contains("only with"));

        let w = set(
            &old.id,
            &[
                "status=superseded",
                &format!("superseded_by={}", new.id),
                &format!("related+={}, {}", item.id, new.id),
                "files=src/auth.rs",
            ],
        )
        .unwrap();
        let r = Record::read(&vault.root().join(&w.path)).unwrap();
        assert_eq!(r.text("status"), "superseded");
        assert_eq!(r.text("superseded_by"), new.id);
        assert_eq!(
            r.get("related").map(Value::items).unwrap(),
            [item.id.clone(), new.id.clone()]
        );
        assert_eq!(r.get("files").map(Value::items).unwrap(), ["src/auth.rs"]);
        assert!(r.body.contains("Do it."), "the body is kept");
        let problems = crate::doctor::record_problems(&vault, &vault.root().join(&w.path));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(set("app-D-0042", &["title=x"]).is_err());
    }

    #[test]
    fn a_card_has_every_heading_in_order() {
        let card = Card {
            done: vec![
                "Cached the login query".into(),
                "- Wrote tests\nfor it".into(),
            ],
            next_step: "Measure it.".into(),
            touched: vec!["app-0001".into()],
            ..Card::default()
        };
        let body = card.body();
        let headings: Vec<&str> = body.lines().filter(|l| l.starts_with("## ")).collect();
        assert_eq!(
            headings,
            [
                "## Done",
                "## Decisions",
                "## Open",
                "## Next step",
                "## Touched records"
            ]
        );
        assert!(
            body.contains("- Cached the login query\n- Wrote tests for it\n"),
            "{body}"
        );
        assert!(body.contains("## Next step\n\nMeasure it.\n"));
        assert!(!card.is_empty());
        assert!(Card::default().is_empty());

        let (_tmp, vault) = vault_with(&[("app", None)]);
        let w = write_card(
            &vault,
            "app",
            "1a2b3c4d-5678",
            "2026-10-26 14:30",
            CardSource::Handoff,
            &body,
        )
        .unwrap();
        assert!(crate::doctor::record_problems(&vault, &vault.root().join(&w.path)).is_empty());
    }
}
