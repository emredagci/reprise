//! Events: things that happened once (a release, a deploy, an incident, a migration) or are planned,
//! each with the day it happened, kept apart from the day it was said. A day known only to the month
//! stays a span. The timeline is written by code: the order, how many of each kind, the gaps between
//! them and how long ago each was, so an agent quotes numbers instead of working them out. A project's
//! events are in `projects/<name>/events.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use serde::Serialize;

use crate::dates::When;
use crate::{Error, Result, Vault};

pub const FILE: &str = "events.md";
const COLUMNS: [&str; 6] = ["When", "What", "Kind", "Status", "Said", "Source"];
const HEAD: &str = "# Events\n\nThings that happened once, or are planned, in the order they happen. `When` is a day, \
a month (`2026-10`) or a span (`2026-10-19..2026-10-25`) when the day is not known. Written with `reprise event`.";
/// What an event can be.
pub const STATUSES: [&str; 3] = ["done", "planned", "cancelled"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub when: When,
    pub what: String,
    /// One word: release, deploy, incident, migration, meeting, other…
    pub kind: String,
    /// done, planned or cancelled.
    pub status: String,
    /// The day it was said, `YYYY-MM-DD`.
    pub said: String,
    pub source: String,
}

impl Event {
    fn row(&self) -> Vec<String> {
        vec![
            self.when.display(),
            self.what.clone(),
            self.kind.clone(),
            self.status.clone(),
            self.said.clone(),
            self.source.clone(),
        ]
    }
}

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

/// Every event of `project`, in the order they happen; rows that do not parse are left out.
pub fn read(vault: &Vault, project: &str) -> Result<Vec<Event>> {
    let text = crate::safe::read_to_string(path(vault, project)?).unwrap_or_default();
    Ok(crate::table::rows(&text)
        .into_iter()
        .filter_map(|r| {
            let when = When::parse_display(&r[0])?;
            (!r[1].is_empty()).then(|| Event {
                when,
                what: r[1].clone(),
                kind: r[2].clone(),
                status: r[3].clone(),
                said: r[4].clone(),
                source: r[5].clone(),
            })
        })
        .collect())
}

/// A kind as stored: one lowercase word, `other` when empty.
pub fn kind(text: &str) -> Result<String> {
    let k = crate::text::tokens(text).join("-");
    if k.is_empty() {
        return Ok("other".into());
    }
    if k.contains('-') && text.split_whitespace().count() > 1 {
        return Err(Error::InvalidChange(format!(
            "kind {text:?}: one word, such as release, deploy, incident"
        )));
    }
    Ok(k)
}

/// Adds an event in its place in time. One already written (the same day or span and the same
/// words) is not added again; `false` then. Returns the file relative to the vault.
pub fn add(vault: &Vault, project: &str, event: Event) -> Result<(bool, PathBuf)> {
    let what = event.what.split_whitespace().collect::<Vec<_>>().join(" ");
    if what.is_empty() {
        return Err(Error::InvalidChange(
            "an event needs to say what happened".into(),
        ));
    }
    if !STATUSES.contains(&event.status.as_str()) {
        return Err(Error::InvalidChange(format!(
            "status {:?}: one of {}",
            event.status,
            STATUSES.join(", ")
        )));
    }
    let event = Event {
        what,
        kind: kind(&event.kind)?,
        ..event
    };
    let path = path(vault, project)?;
    let rel = path
        .strip_prefix(vault.root())
        .unwrap_or(&path)
        .to_path_buf();
    let mut events = read(vault, project)?;
    if events
        .iter()
        .any(|e| e.when == event.when && e.what.eq_ignore_ascii_case(&event.what))
    {
        return Ok((false, rel));
    }
    let at = events.partition_point(|e| e.when.start <= event.when.start);
    events.insert(at, event);
    let rows: Vec<Vec<String>> = events.iter().map(Event::row).collect();
    crate::scaffold::write(&path, &crate::table::render(HEAD, &COLUMNS, &rows))?;
    Ok((true, rel))
}

/// How far `when` is from `today`, in words: "today", "3 days ago", "in 2 days", "20–26 days ago".
pub fn ago(when: &When, today: NaiveDate) -> String {
    let one = |days: i64| match days {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        -1 => "tomorrow".to_owned(),
        d if d > 0 => format!("{d} days ago"),
        d => format!("in {} days", -d),
    };
    if when.is_day() {
        return one((today - when.start).num_days());
    }
    let (near, far) = (
        (today - when.end).num_days(),
        (today - when.start).num_days(),
    );
    if near >= 0 {
        format!("{near}–{far} days ago")
    } else if far <= 0 {
        format!("in {}–{} days", -far, -near)
    } else {
        "now".to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub when: String,
    pub what: String,
    pub kind: String,
    pub status: String,
    /// "3 days ago", computed for the day the timeline is made.
    pub ago: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindSummary {
    pub kind: String,
    /// Events of this kind that happened (`done`).
    pub count: usize,
    /// The latest one that happened, and how long ago.
    pub last: Option<String>,
    pub last_ago: Option<String>,
    /// Average whole days between them, from the days they are known to; `None` with fewer than two.
    pub average_gap_days: Option<i64>,
    /// The next planned one, if any.
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Timeline {
    pub today: String,
    pub entries: Vec<Entry>,
    pub kinds: Vec<KindSummary>,
}

/// The timeline of `events`, of one kind when `only` names it, from `since` when given.
pub fn timeline(
    events: &[Event],
    only: Option<&str>,
    since: Option<NaiveDate>,
    today: NaiveDate,
) -> Timeline {
    let only = only.map(|k| crate::text::tokens(k).join("-"));
    let chosen: Vec<&Event> = events
        .iter()
        .filter(|e| only.as_deref().is_none_or(|k| e.kind == k))
        .filter(|e| since.is_none_or(|s| e.when.end >= s))
        .collect();
    let entries = chosen
        .iter()
        .map(|e| Entry {
            when: e.when.display(),
            what: e.what.clone(),
            kind: e.kind.clone(),
            status: e.status.clone(),
            ago: ago(&e.when, today),
        })
        .collect();
    let mut by_kind: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
    for e in &chosen {
        by_kind.entry(e.kind.as_str()).or_default().push(e);
    }
    let kinds = by_kind
        .into_iter()
        .map(|(kind, list)| {
            let done: Vec<&&Event> = list
                .iter()
                .filter(|e| e.status == "done" && e.when.start <= today)
                .collect();
            let last = done.last();
            let gaps: Vec<i64> = done
                .windows(2)
                .map(|w| (w[1].when.start - w[0].when.start).num_days())
                .collect();
            let next = list
                .iter()
                .find(|e| e.status == "planned" && e.when.end >= today)
                .map(|e| format!("{} ({}, {})", e.what, e.when.display(), ago(&e.when, today)));
            KindSummary {
                kind: kind.to_owned(),
                count: done.len(),
                last: last.map(|e| format!("{} ({})", e.what, e.when.display())),
                last_ago: last.map(|e| ago(&e.when, today)),
                average_gap_days: (!gaps.is_empty())
                    .then(|| gaps.iter().sum::<i64>() / i64::try_from(gaps.len()).unwrap_or(1)),
                next,
            }
        })
        .collect();
    Timeline {
        today: today.format("%Y-%m-%d").to_string(),
        entries,
        kinds,
    }
}

impl Timeline {
    /// The timeline as text, for an agent to quote.
    pub fn render(&self) -> String {
        if self.entries.is_empty() {
            return format!("No events (today is {}).", self.today);
        }
        let mut out = format!("Timeline, computed on {}:\n", self.today);
        for e in &self.entries {
            let status = if e.status == "done" {
                String::new()
            } else {
                format!(" [{}]", e.status)
            };
            out.push_str(&format!(
                "- {} · {} · {}{status} ({})\n",
                e.when, e.kind, e.what, e.ago
            ));
        }
        out.push('\n');
        for k in &self.kinds {
            let mut line = format!("- {}: {} happened", k.kind, k.count);
            if let (Some(last), Some(ago)) = (&k.last, &k.last_ago) {
                line.push_str(&format!("; last {last}, {ago}"));
            }
            if let Some(gap) = k.average_gap_days {
                line.push_str(&format!("; about every {gap} days"));
            }
            if let Some(next) = &k.next {
                line.push_str(&format!("; next {next}"));
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn ev(when: &str, what: &str, kind: &str, status: &str) -> Event {
        Event {
            when: When::parse_display(when).unwrap(),
            what: what.into(),
            kind: kind.into(),
            status: status.into(),
            said: "2026-10-30".into(),
            source: String::new(),
        }
    }

    #[test]
    fn events_are_kept_in_order_once() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        crate::scaffold::ensure_project(&vault, "app", None, d("2026-10-01")).unwrap();
        let (added, path) = add(
            &vault,
            "app",
            ev("2026-10-27", "Released 0.1.0", "Release", "done"),
        )
        .unwrap();
        assert!(added);
        assert_eq!(path, PathBuf::from("projects/app/events.md"));
        add(
            &vault,
            "app",
            ev("2026-10-10", "Staging deploy", "deploy", "done"),
        )
        .unwrap();
        add(
            &vault,
            "app",
            ev("2026-11", "Release 0.2.0", "release", "planned"),
        )
        .unwrap();
        let (again, _) = add(
            &vault,
            "app",
            ev("2026-10-27", "released 0.1.0", "release", "done"),
        )
        .unwrap();
        assert!(!again, "already written");
        let events = read(&vault, "app").unwrap();
        let order: Vec<&str> = events.iter().map(|e| e.what.as_str()).collect();
        assert_eq!(order, ["Staging deploy", "Released 0.1.0", "Release 0.2.0"]);
        assert_eq!(events[1].kind, "release");
        assert!(add(&vault, "app", ev("2026-10-01", "x", "x", "maybe")).is_err());
        assert!(add(&vault, "app", ev("2026-10-01", "x", "two words", "done")).is_err());
    }

    #[test]
    fn the_timeline_counts_and_measures() {
        let events = vec![
            ev("2026-09-01", "Released 0.0.8", "release", "done"),
            ev("2026-09-10", "Outage on staging", "incident", "done"),
            ev("2026-09-15", "Released 0.0.9", "release", "done"),
            ev(
                "2026-10-19..2026-10-25",
                "Released 0.1.0",
                "release",
                "done",
            ),
            ev("2026-11-10", "Release 0.2.0", "release", "planned"),
        ];
        let t = timeline(&events, None, None, d("2026-10-30"));
        assert_eq!(t.entries.len(), 5);
        assert_eq!(t.entries[0].ago, "59 days ago");
        assert_eq!(t.entries[3].ago, "5–11 days ago");
        assert_eq!(t.entries[4].ago, "in 11 days");
        let release = t.kinds.iter().find(|k| k.kind == "release").unwrap();
        assert_eq!(release.count, 3);
        assert_eq!(release.average_gap_days, Some(24), "14 and 34 days");
        assert_eq!(release.last_ago.as_deref(), Some("5–11 days ago"));
        assert!(
            release
                .next
                .as_deref()
                .unwrap()
                .starts_with("Release 0.2.0 (2026-11-10, in 11 days)")
        );
        let text = t.render();
        assert!(text.contains("- release: 3 happened; last Released 0.1.0 (2026-10-19..2026-10-25), 5–11 days ago; about every 24 days; next Release 0.2.0"), "{text}");
        assert!(
            text.contains("2026-11-10 · release · Release 0.2.0 [planned] (in 11 days)"),
            "{text}"
        );

        let only = timeline(&events, Some("Incident"), None, d("2026-10-30"));
        assert_eq!(only.entries.len(), 1);
        let since = timeline(&events, None, Some(d("2026-10-01")), d("2026-10-30"));
        assert_eq!(since.entries.len(), 2);
        assert_eq!(
            ago(&When::day(d("2026-10-29")), d("2026-10-30")),
            "yesterday"
        );
    }
}
