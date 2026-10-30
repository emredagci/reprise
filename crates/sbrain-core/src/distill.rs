//! Distilling one session into records. The engine gets the instructions (`templates/distill/system.md`)
//! and an input built here from the vault and the session's transcript digest, and answers with JSON
//! matching `templates/distill/schema.json`. It never writes to the vault: what comes back is
//! checked here first, records whose quotes are not in the session are left out (`evidence`), and
//! only then is the rest written through `store`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::record::{Record, Value};
use crate::schema::{ItemStatus, ItemType};
use crate::store::{self, CardSource, Change, NewDecision, NewItem};
use crate::{Error, Result, Vault, evidence};

/// Instructions for the engine.
pub const SYSTEM_PROMPT: &str = include_str!("../templates/distill/system.md");
/// JSON Schema of the answer, handed to the engine as is.
pub const SCHEMA: &str = include_str!("../templates/distill/schema.json");

/// Most records one session may produce; more means the answer went wrong.
const MAX_NEW_ITEMS: usize = 12;
const MAX_UPDATES: usize = 20;
const MAX_DECISIONS: usize = 6;
const MAX_RULES: usize = 6;
/// Characters kept of a title, a rule and each `state.md` section.
const TITLE_LIMIT: usize = 200;
const RULE_LIMIT: usize = 300;
const STATE_SECTION_LIMIT: usize = 600;
/// Open items and decisions listed in the input.
const LISTED_ITEMS: usize = 60;
const LISTED_DECISIONS: usize = 15;
/// The section of an item or decision that keeps the quotes it was written from.
const EVIDENCE: &str = "Evidence";
/// The closing line of every card sbrain writes from a transcript.
const CARD_NOTE: &str = "Distilled from the transcript; not verified.";

/// The session being distilled.
#[derive(Debug, Clone)]
pub struct Session<'a> {
    /// The agent's session id; cards use its first 8 characters.
    pub id: &'a str,
    pub project: &'a str,
    pub cwd: &'a str,
    /// First and last activity, `YYYY-MM-DD HH:MM`, local time.
    pub first: &'a str,
    pub last: &'a str,
    /// The transcript digest the client built.
    pub digest: &'a str,
    /// Every message the user wrote, whole: rules must quote these.
    pub user_text: &'a str,
}

/// The engine's answer (`templates/distill/schema.json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distilled {
    pub card: CardOut,
    pub new_items: Vec<NewItemOut>,
    pub item_updates: Vec<ItemUpdate>,
    pub new_decisions: Vec<NewDecisionOut>,
    pub rules: Vec<RuleOut>,
    pub state: Option<StateOut>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardOut {
    pub done: Vec<String>,
    pub decisions: Vec<String>,
    pub open: Vec<String>,
    pub next_step: String,
    pub uncertain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewItemOut {
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub problem: String,
    pub solution: String,
    pub files: Vec<String>,
    pub aliases: Vec<String>,
    pub commit: String,
    pub verify: String,
    /// Quotes from the digest that show the work.
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemUpdate {
    pub id: String,
    /// Empty when the status does not change.
    pub status: String,
    pub commit: String,
    pub verify: String,
    pub files: Vec<String>,
    pub note: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDecisionOut {
    pub title: String,
    pub context: String,
    pub decision: String,
    pub why: String,
    pub alternatives: String,
    pub related: Vec<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleOut {
    /// `project` or `general`.
    pub scope: String,
    pub rule: String,
    /// Quotes from the user's own messages.
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateOut {
    pub what: String,
    pub architecture: String,
    pub working: String,
    pub not_working: String,
    pub run_and_test: String,
}

/// What was written, with paths relative to the vault.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub card: PathBuf,
    pub items_added: Vec<String>,
    pub items_updated: Vec<String>,
    pub decisions_added: Vec<String>,
    /// Rules proposed (`store::propose_rule`), waiting for the user.
    pub rules_proposed: usize,
    pub state_filled: bool,
    /// What the answer had but no quote in the session backed, so was not written.
    pub left_out: Vec<String>,
    /// Every file written or changed, for the vault commit.
    pub files: Vec<PathBuf>,
}

impl Distilled {
    /// Reads the engine's answer; anything off the schema is refused.
    pub fn from_json(value: serde_json::Value) -> Result<Self> {
        serde_json::from_value(value)
            .map_err(|e| Error::InvalidChange(format!("distill output: {e}")))
    }
}

/// The input for the engine: the session, the project's `state.md`, open items, recent decisions,
/// the existing card and the digest.
pub fn input(vault: &Vault, s: &Session) -> String {
    let dir = vault.project_dir(s.project);
    let card = store::find_card(vault, s.project, s.id)
        .and_then(|p| crate::safe::read_to_string(p).ok())
        .map_or_else(
            || "none".to_owned(),
            |t| format!("\n\n```markdown\n{}\n```", t.trim()),
        );
    let state = crate::safe::read_to_string(dir.join("state.md")).unwrap_or_default();
    let state = if state_is_blank(&state) {
        "(still the template: fill `state` from this session)".to_owned()
    } else {
        format!("```markdown\n{}\n```", state.trim())
    };
    let items: Vec<String> = crate::browse::items(vault)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.project == s.project)
        .filter(|i| {
            i.status
                .parse::<ItemStatus>()
                .is_ok_and(|st| !st.is_closed())
        })
        .take(LISTED_ITEMS)
        .map(|i| format!("- {} [{}] {}", i.id, i.status, i.title))
        .collect();
    let decisions: Vec<String> = crate::browse::decisions(vault)
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.project == s.project && d.status != "superseded")
        .take(LISTED_DECISIONS)
        .map(|d| format!("- {} {}", d.id, d.title))
        .collect();
    let list = |lines: Vec<String>| {
        if lines.is_empty() {
            "(none)".to_owned()
        } else {
            lines.join("\n")
        }
    };
    format!(
        "# Session to distill\n\nproject: {}\nsession: {}\nworking directory: {}\ntime: {} → {}\nexisting handoff card: {card}\n\n\
         ## state.md\n\n{state}\n\n## Open items of this project\n\n{}\n\n## Recent decisions of this project\n\n{}\n\n\
         ## Transcript digest\n\n{}\n",
        s.project,
        s.id,
        s.cwd,
        s.first,
        s.last,
        list(items),
        list(decisions),
        s.digest.trim()
    )
}

/// Problems that keep the answer from being written; empty when it can be.
pub fn check(vault: &Vault, project: &str, d: &Distilled) -> Vec<String> {
    let mut problems = Vec::new();
    for (what, n, max) in [
        ("new items", d.new_items.len(), MAX_NEW_ITEMS),
        ("item updates", d.item_updates.len(), MAX_UPDATES),
        ("new decisions", d.new_decisions.len(), MAX_DECISIONS),
        ("rules", d.rules.len(), MAX_RULES),
    ] {
        if n > max {
            problems.push(format!("{n} {what}; at most {max}"));
        }
    }
    for item in &d.new_items {
        if item.title.trim().is_empty() {
            problems.push("a new item has no title".into());
        }
        if item.kind.parse::<ItemType>().is_err() {
            problems.push(format!("new item {:?}: type {:?}", item.title, item.kind));
        }
        match item.status.parse::<ItemStatus>() {
            Ok(st) if !st.is_closed() => {}
            _ => problems.push(format!(
                "new item {:?}: status {:?}",
                item.title, item.status
            )),
        }
    }
    for update in &d.item_updates {
        match store::find_item(vault, &update.id) {
            Ok((p, _)) if p == project => {}
            Ok((p, _)) => problems.push(format!("{} belongs to {p}, not {project}", update.id)),
            Err(_) => problems.push(format!("{} does not exist", update.id)),
        }
        if !update.status.is_empty() {
            match update.status.parse::<ItemStatus>() {
                Ok(st) if st.settable_by_hand() => {}
                _ => problems.push(format!("{}: status {:?}", update.id, update.status)),
            }
        }
    }
    for decision in &d.new_decisions {
        if decision.title.trim().is_empty() {
            problems.push("a new decision has no title".into());
        }
    }
    for rule in &d.rules {
        if !matches!(rule.scope.as_str(), "project" | "general") {
            problems.push(format!("rule scope {:?}", rule.scope));
        }
        if rule.rule.trim().is_empty() {
            problems.push("a rule has no text".into());
        }
    }
    problems
}

/// Writes the answer: new items, item changes, decisions, rules, `state.md` when it is still the
/// template, and the session's card. Run `check` first; this refuses what it refuses.
pub fn apply(vault: &Vault, s: &Session, d: &Distilled, today: NaiveDate) -> Result<Applied> {
    let problems = check(vault, s.project, d);
    if !problems.is_empty() {
        return Err(Error::InvalidChange(format!(
            "distill output: {}",
            problems.join("; ")
        )));
    }
    let session8 = crate::text::take_chars(s.id, 8);
    let mut out = Applied::default();
    let mut touched: Vec<String> = Vec::new();
    let everything = evidence::normalize(&format!("{}\n{}", s.digest, s.user_text));
    let users = evidence::normalize(s.user_text);

    for item in &d.new_items {
        let quotes = evidence::supported(&item.evidence, &everything);
        if quotes.is_empty() {
            out.left_out
                .push(format!("item \"{}\"", one_line(&item.title)));
            continue;
        }
        let new = NewItem {
            title: crate::text::clip(item.title.trim(), TITLE_LIMIT),
            kind: item.kind.parse().expect("checked"),
            status: item.status.parse().expect("checked"),
            body: item.problem.trim().to_owned(),
            captured_during: format!("transcript {session8}"),
            aliases: clean_list(&item.aliases),
            files: clean_list(&item.files),
            related: Vec::new(),
        };
        let w = store::add_item(vault, s.project, &new, today)?;
        let mut changes = Vec::new();
        for (key, value) in [("commit", &item.commit), ("verify", &item.verify)] {
            if !value.trim().is_empty() {
                changes.push(Change::Set {
                    key: key.into(),
                    value: one_line(value),
                });
            }
        }
        if !changes.is_empty() {
            store::set_item(vault, &w.id, &changes, today)?;
        }
        let path = vault.root().join(&w.path);
        let mut record = Record::read(&path)?;
        record.body = store::fill_section(&record.body, "Solution", &item.solution);
        record.body = store::fill_section(&record.body, EVIDENCE, &quote_lines(&quotes));
        crate::scaffold::write(&path, &record.to_text())?;
        touched.push(w.id.clone());
        out.items_added.push(w.id);
        out.files.push(w.path);
    }

    for update in &d.item_updates {
        if evidence::supported(&update.evidence, &everything).is_empty() {
            out.left_out.push(format!("change to {}", update.id));
            continue;
        }
        let mut changes = Vec::new();
        if !update.status.is_empty() {
            changes.push(Change::status(update.status.parse().expect("checked")));
        }
        for (key, value) in [("commit", &update.commit), ("verify", &update.verify)] {
            if !value.trim().is_empty() {
                changes.push(Change::Set {
                    key: key.into(),
                    value: one_line(value),
                });
            }
        }
        let files = clean_list(&update.files);
        if !files.is_empty() {
            changes.push(Change::Append {
                key: "files".into(),
                values: files,
            });
        }
        let mut written = None;
        if !changes.is_empty() {
            written = Some(store::set_item(vault, &update.id, &changes, today)?);
        }
        if !update.note.trim().is_empty() {
            let note = format!("{} (transcript {session8})", one_line(&update.note));
            written = Some(store::note_item(vault, &update.id, &note, today)?);
        }
        if let Some(w) = written {
            touched.push(w.id.clone());
            out.items_updated.push(w.id);
            out.files.push(w.path);
        }
    }

    let known = known_ids(vault);
    let mut decision_lines = Vec::new();
    for decision in &d.new_decisions {
        let quotes = evidence::supported(&decision.evidence, &everything);
        if quotes.is_empty() {
            out.left_out
                .push(format!("decision \"{}\"", one_line(&decision.title)));
            continue;
        }
        let new = NewDecision {
            title: crate::text::clip(decision.title.trim(), TITLE_LIMIT),
            context: decision.context.clone(),
            decision: decision.decision.clone(),
            why: decision.why.clone(),
            alternatives: decision.alternatives.clone(),
            related: decision
                .related
                .iter()
                .map(|r| r.trim().to_owned())
                .filter(|r| known.contains(r) || touched.contains(r))
                .collect(),
            files: Vec::new(),
            commit: String::new(),
        };
        let w = store::add_decision(vault, s.project, &new, today)?;
        let path = vault.root().join(&w.path);
        let mut record = Record::read(&path)?;
        record.body = store::fill_section(&record.body, EVIDENCE, &quote_lines(&quotes));
        crate::scaffold::write(&path, &record.to_text())?;
        decision_lines.push(format!("{}: {}", w.id, one_line(&decision.title)));
        touched.push(w.id.clone());
        out.decisions_added.push(w.id);
        out.files.push(w.path);
    }

    // Rules found in a transcript are only proposed: text in a session (a tool's output, a file it
    // read) can pass for a correction from the user, and a rule reaches every later session.
    for rule in &d.rules {
        let text = crate::text::clip(&one_line(&rule.rule), RULE_LIMIT);
        if evidence::supported(&rule.evidence, &users).is_empty() {
            out.left_out.push(format!("rule \"{text}\""));
            continue;
        }
        let project = (rule.scope == "project").then_some(s.project);
        if let Some(path) = store::propose_rule(vault, project, &text, s.id, today)? {
            out.rules_proposed += 1;
            if !out.files.contains(&path) {
                out.files.push(path);
            }
        }
    }

    if let Some(state) = &d.state
        && let Some(path) = fill_state(vault, s.project, state, today)?
    {
        out.state_filled = true;
        out.files.push(path);
    }

    let body = card_body(&d.card, &decision_lines, &touched, &out.left_out);
    let card = store::write_card(
        vault,
        s.project,
        s.id,
        s.last,
        CardSource::Transcript,
        &body,
    )?;
    out.files.push(card.path.clone());
    out.card = card.path;
    Ok(out)
}

/// The card's sections, in the vault format's order, with the closing note.
fn card_body(
    card: &CardOut,
    new_decisions: &[String],
    touched: &[String],
    left_out: &[String],
) -> String {
    let bullets = |lines: &[String]| {
        let lines: Vec<String> = lines
            .iter()
            .map(|l| one_line(l))
            .filter(|l| !l.is_empty())
            .map(|l| format!("- {}", l.trim_start_matches("- ")))
            .collect();
        lines.join("\n")
    };
    let mut decisions = card.decisions.clone();
    decisions.extend(new_decisions.iter().cloned());
    let mut uncertain = card.uncertain.clone();
    uncertain.extend(
        left_out
            .iter()
            .map(|l| format!("Not written, no quote from the session backs it: {l}")),
    );
    uncertain.push(CARD_NOTE.to_owned());
    let sections = [
        ("Done", bullets(&card.done)),
        ("Decisions", bullets(&decisions)),
        ("Open", bullets(&card.open)),
        ("Next step", card.next_step.trim().to_owned()),
        ("Touched records", bullets(touched)),
        ("Uncertain", bullets(&uncertain)),
    ];
    sections
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

/// Fills `state.md` from the answer when it still holds only its template. Returns its path when it
/// was written.
fn fill_state(
    vault: &Vault,
    project: &str,
    state: &StateOut,
    today: NaiveDate,
) -> Result<Option<PathBuf>> {
    let path = vault.project_dir(project).join("state.md");
    let Ok(text) = crate::safe::read_to_string(&path) else {
        return Ok(None);
    };
    if !state_is_blank(&text) {
        return Ok(None);
    }
    let Some(mut record) = Record::parse(&text) else {
        return Ok(None);
    };
    // The template's guidance comments go; so do the blank lines they leave.
    let mut body = crate::text::strip_comments(&record.body);
    while body.contains("\n\n\n") {
        body = body.replace("\n\n\n", "\n\n");
    }
    for (heading, value) in [
        ("What", &state.what),
        ("Architecture", &state.architecture),
        ("Working", &state.working),
        ("Not working", &state.not_working),
        ("Run and test", &state.run_and_test),
    ] {
        body = store::fill_section(
            &body,
            heading,
            &crate::text::clip(value.trim(), STATE_SECTION_LIMIT),
        );
    }
    record.body = body;
    record.set("updated", Value::Text(today.format("%Y-%m-%d").to_string()));
    crate::scaffold::write(&path, &record.to_text())?;
    Ok(Some(
        path.strip_prefix(vault.root())
            .unwrap_or(&path)
            .to_path_buf(),
    ))
}

/// Whether `state.md` says nothing beyond its template.
fn state_is_blank(text: &str) -> bool {
    let body = Record::parse(text).map_or_else(|| text.to_owned(), |r| r.body);
    !crate::context::has_content(&crate::text::strip_comments(&body))
}

/// Every item and decision id in the vault.
fn known_ids(vault: &Vault) -> BTreeSet<String> {
    let items = crate::browse::items(vault)
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.id);
    let decisions = crate::browse::decisions(vault)
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.id);
    items.chain(decisions).collect()
}

/// Quotes as a record keeps them: one `>` line each, a blank line between.
fn quote_lines(quotes: &[String]) -> String {
    quotes
        .iter()
        .map(|q| format!("> {q}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn clean_list(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let v = one_line(v).replace(',', " ");
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn one_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn vault() -> (tempfile::TempDir, Vault) {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        crate::scaffold::ensure_project(&vault, "app", None, day("2026-10-20")).unwrap();
        store::add_item(
            &vault,
            "app",
            &NewItem::note("Login is slow"),
            day("2026-10-20"),
        )
        .unwrap();
        (tmp, vault)
    }

    /// What the user said in the session `answer()` distills.
    const USER: &str = "Login is too slow, it runs the same query twice. Use a per-request cache, \
        not a global one. Measure before caching anything.";

    /// The digest `answer()` quotes from.
    fn digest() -> String {
        format!(
            "## User [2026-10-25 09:00]\n{USER}\nAssistant: I cached the login query per request.\n    \
             Half of the slowness is gone.\n$ cargo test login: ok"
        )
    }

    fn session(digest: &str) -> Session<'_> {
        Session {
            id: "1a2b3c4d-0000",
            project: "app",
            cwd: "/work/app",
            first: "2026-10-25 09:00",
            last: "2026-10-25 10:15",
            digest,
            user_text: USER,
        }
    }

    fn answer() -> serde_json::Value {
        serde_json::json!({
            "card": {
                "done": ["Cached the login query"],
                "decisions": [],
                "open": ["Measure it on staging"],
                "next_step": "Measure login time on staging.",
                "uncertain": ["Whether the cache is invalidated on logout"]
            },
            "new_items": [{
                "title": "Cache the login query",
                "type": "feature",
                "status": "done-unverified",
                "problem": "Login ran the same query twice.",
                "solution": "Cached it per request.",
                "files": ["src/login.rs"],
                "aliases": ["login cache"],
                "commit": "abc1234",
                "verify": "cargo test login",
                "evidence": ["I cached the login query per request."]
            }],
            "item_updates": [{
                "id": "app-0001",
                "status": "doing",
                "commit": "",
                "verify": "",
                "files": ["src/login.rs"],
                "note": "Half of it is fixed by the cache.",
                "evidence": ["cached the login query … Half of the slowness is gone"]
            }],
            "new_decisions": [{
                "title": "Cache per request, not globally",
                "context": "Login was slow.",
                "decision": "A per-request cache.",
                "why": "No invalidation needed.",
                "alternatives": "A global cache with TTL.",
                "related": ["app-0001", "nope-9999"],
                "evidence": ["Use a per-request cache, not a global one."]
            }],
            "rules": [{
                "scope": "project",
                "rule": "Measure before caching.",
                "evidence": ["Measure before caching anything"]
            }],
            "state": {
                "what": "A web app.",
                "architecture": "Rust backend.",
                "working": "Login.",
                "not_working": "",
                "run_and_test": "cargo test"
            }
        })
    }

    #[test]
    fn schema_and_types_agree() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let top: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            top,
            [
                "card",
                "new_items",
                "item_updates",
                "new_decisions",
                "rules",
                "state"
            ]
        );
        Distilled::from_json(answer()).unwrap();
        let mut extra = answer();
        extra["card"]["mood"] = "fine".into();
        assert!(Distilled::from_json(extra).is_err());
        let mut no_state = answer();
        no_state["state"] = serde_json::Value::Null;
        assert!(Distilled::from_json(no_state).unwrap().state.is_none());
    }

    #[test]
    fn input_lists_the_project_and_marks_a_blank_state() {
        let (_tmp, vault) = vault();
        let text = input(
            &vault,
            &session("## User [2026-10-25 09:00]\nmake login faster"),
        );
        assert!(
            text.contains("project: app\nsession: 1a2b3c4d-0000\n"),
            "{text}"
        );
        assert!(text.contains("existing handoff card: none"), "{text}");
        assert!(
            text.contains("(still the template: fill `state` from this session)"),
            "{text}"
        );
        assert!(text.contains("- app-0001 [inbox] Login is slow"), "{text}");
        assert!(text.ends_with("make login faster\n"), "{text}");
    }

    #[test]
    fn applies_an_answer() {
        let (_tmp, vault) = vault();
        let d = Distilled::from_json(answer()).unwrap();
        let out = apply(&vault, &session(&digest()), &d, day("2026-10-25")).unwrap();
        assert_eq!(out.items_added, ["app-0002"]);
        assert_eq!(out.items_updated, ["app-0001"]);
        assert_eq!(out.decisions_added, ["app-D-0001"]);
        assert_eq!(out.rules_proposed, 1);
        assert!(out.state_filled);
        assert_eq!(
            out.card,
            PathBuf::from("projects/app/sessions/2026-10-25-1015-1a2b3c4d.md")
        );

        let read = |rel: &PathBuf| Record::read(&vault.root().join(rel)).unwrap();
        let new = read(&out.files[0]);
        assert_eq!(new.text("status"), "done-unverified");
        assert_eq!(new.text("commit"), "abc1234");
        assert_eq!(new.text("verify"), "cargo test login");
        assert_eq!(new.text("captured_during"), "transcript 1a2b3c4d");
        assert!(
            new.body
                .contains("## Problem\n\nLogin ran the same query twice.\n"),
            "{}",
            new.body
        );
        assert!(
            new.body.contains("## Solution\n\nCached it per request.\n"),
            "{}",
            new.body
        );
        assert!(
            new.body
                .contains("## Evidence\n\n> I cached the login query per request.\n"),
            "{}",
            new.body
        );

        let old = read(&out.files[1]);
        assert_eq!(old.text("status"), "doing");
        assert_eq!(old.get("files").unwrap().items(), ["src/login.rs"]);
        assert!(
            old.body
                .contains("- 2026-10-25: Half of it is fixed by the cache. (transcript 1a2b3c4d)")
        );

        let decision = read(&out.files[2]);
        assert!(
            decision
                .body
                .contains("## Evidence\n\n> Use a per-request cache, not a global one.\n"),
            "{}",
            decision.body
        );
        assert!(out.left_out.is_empty(), "{:?}", out.left_out);
        assert_eq!(
            decision.get("related").unwrap().items(),
            ["app-0001"],
            "unknown ids are dropped"
        );

        // The rule waits for the user; it is not in the gotchas agents read.
        let gotchas = fs::read_to_string(vault.project_dir("app").join("gotchas.md")).unwrap();
        assert!(!gotchas.contains("Measure before caching"), "{gotchas}");
        let proposed =
            fs::read_to_string(vault.project_dir("app").join(store::PROPOSED_RULES)).unwrap();
        assert!(
            proposed.ends_with("- 2026-10-25 (1a2b3c4d): Measure before caching.\n"),
            "{proposed}"
        );
        let waiting = store::proposals(&vault).unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].project.as_deref(), Some("app"));
        assert_eq!(waiting[0].session, "1a2b3c4d");
        let state = fs::read_to_string(vault.project_dir("app").join("state.md")).unwrap();
        assert!(
            state.contains("## What\n\nA web app.\n\n## Architecture") && !state.contains("<!--"),
            "{state}"
        );
        assert!(!state.contains("\n\n\n"), "{state}");

        let card = read(&out.card);
        assert_eq!(card.text("source"), "transcript");
        assert_eq!(card.text("date"), "2026-10-25 10:15");
        for part in [
            "## Done\n\n- Cached the login query\n",
            "## Decisions\n\n- app-D-0001: Cache per request, not globally\n",
            "## Next step\n\nMeasure login time on staging.\n",
            "## Touched records\n\n- app-0002\n- app-0001\n- app-D-0001\n",
            "- Distilled from the transcript; not verified.\n",
        ] {
            assert!(card.body.contains(part), "{part:?} in {}", card.body);
        }

        // A filled state.md is left alone the next time.
        let again = apply(
            &vault,
            &session(&digest()),
            &Distilled::from_json(answer()).unwrap(),
            day("2026-10-26"),
        )
        .unwrap();
        assert!(!again.state_filled);
        assert_eq!(again.card, out.card, "the same session's card is rewritten");
    }

    #[test]
    fn leaves_out_what_the_session_does_not_back() {
        let (_tmp, vault) = vault();
        let mut a = answer();
        a["new_items"][0]["evidence"] = serde_json::json!(["We rewrote the login in Go."]);
        a["item_updates"][0]["evidence"] = serde_json::json!([]);
        a["new_decisions"][0]["evidence"] = serde_json::json!(["ok"]);
        // The assistant said it, the user did not: a rule needs the user's own words.
        a["rules"][0]["evidence"] = serde_json::json!(["Half of the slowness is gone."]);
        let d = Distilled::from_json(a).unwrap();
        let out = apply(&vault, &session(&digest()), &d, day("2026-10-25")).unwrap();
        assert!(out.items_added.is_empty());
        assert!(out.items_updated.is_empty());
        assert!(out.decisions_added.is_empty());
        assert_eq!(out.rules_proposed, 0);
        assert_eq!(
            out.left_out,
            [
                "item \"Cache the login query\"",
                "change to app-0001",
                "decision \"Cache per request, not globally\"",
                "rule \"Measure before caching.\""
            ]
        );
        let card = Record::read(&vault.root().join(&out.card)).unwrap();
        assert!(
            card.body.contains(
                "- Not written, no quote from the session backs it: change to app-0001\n"
            ),
            "{}",
            card.body
        );
        assert_eq!(
            store::find_item(&vault, "app-0001")
                .map(|(_, p)| Record::read(&p).unwrap().text("status").to_owned())
                .unwrap(),
            "inbox",
            "the unbacked change is not made"
        );
    }

    #[test]
    fn secrets_never_reach_a_record() {
        let (_tmp, vault) = vault();
        let key = format!("ghp_{}", "Zz9".repeat(12));
        let mut a = answer();
        a["new_items"][0]["problem"] = format!("Deploy failed with token {key}").into();
        let d = Distilled::from_json(a).unwrap();
        let out = apply(&vault, &session(&digest()), &d, day("2026-10-25")).unwrap();
        let text = fs::read_to_string(vault.root().join(&out.files[0])).unwrap();
        assert!(!text.contains(&key), "{text}");
        assert!(
            text.contains("Deploy failed with token [redacted]"),
            "{text}"
        );
    }

    #[test]
    fn refuses_answers_it_cannot_write() {
        let (_tmp, vault) = vault();
        let mut bad = answer();
        bad["item_updates"][0]["id"] = "app-0099".into();
        bad["new_items"][0]["status"] = "verified".into();
        bad["rules"][0]["scope"] = "team".into();
        let d = Distilled::from_json(bad).unwrap();
        let problems = check(&vault, "app", &d);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(apply(&vault, &session(&digest()), &d, day("2026-10-25")).is_err());
        assert!(
            !vault.project_dir("app").join("sessions").exists(),
            "nothing is written"
        );

        let mut verified = answer();
        verified["item_updates"][0]["status"] = "verified".into();
        assert!(
            Distilled::from_json(verified)
                .map(|d| check(&vault, "app", &d))
                .unwrap()
                .iter()
                .any(|p| p.contains("app-0001"))
        );
    }
}
