//! Health check: broken records, secrets in the vault, weak verify commands, work left unverified or forgotten, stale
//! verified items, broken links, context files that get cut, the vault's git backup, failed or slow
//! hooks, the job ledger and the sweep schedule. Reads only; the result is also saved to `.sbrain/cache/doctor.json`, which the
//! session-start context summarizes.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{NaiveDate, NaiveDateTime};
use serde::Serialize;

use crate::record::Record;
use crate::schema::{DecisionStatus, ItemStatus, ItemType};
use crate::stale::{git, repo_for, stale_check};
use crate::{Access, Result, Vault};

/// Days after which a done but unverified item is reported.
const UNVERIFIED_WARN_DAYS: i64 = 7;
/// Days after which an item still `doing` is reported.
const DOING_WARN_DAYS: i64 = 14;
/// Characters the session-start context keeps of each file (see `context`).
const SIZE_LIMITS: [(&str, usize); 4] = [
    ("user.md", 1500),
    ("rules.md", 1500),
    ("state.md", 2000),
    ("gotchas.md", 800),
];
const REQUIRED_ITEM: [&str; 4] = ["id", "title", "type", "status"];
const REQUIRED_DECISION: [&str; 3] = ["id", "title", "status"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub level: Level,
    /// What was checked: format, id, status, type, related, superseded, weak-verify, unverified,
    /// forgotten, stale, size, secret, git, push, hook, hook-speed, ledger or sweep.
    pub check: &'static str,
    pub msg: String,
    /// How to fix it; empty when there is nothing to do.
    pub fix: String,
    /// The id of the record it is about; empty when it is not about one.
    pub record: String,
    /// The vault-relative file it is about; empty when it is not about one.
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// `YYYY-MM-DD HH:MM`, local time.
    pub at: String,
    pub counts: BTreeMap<Level, usize>,
    /// Errors first, then warnings, then notes.
    pub findings: Vec<Finding>,
}

impl Serialize for Level {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        })
    }
}

/// Runs every check. `quick` skips the staleness check, which runs git per verified item.
pub fn doctor(vault: &Vault, access: &Access, now: NaiveDateTime, quick: bool) -> Result<Report> {
    let mut findings = Vec::new();
    // `about` is the record's id and the file, either empty when there is none.
    let mut add = |level, check, about: (&str, &str), msg: String, fix: String| {
        findings.push(Finding {
            level,
            check,
            msg,
            fix,
            record: about.0.to_owned(),
            file: about.1.to_owned(),
        });
    };
    let today = now.date();

    // Secrets in any file the vault's git history takes.
    for path in vault_files(vault.root()) {
        let Ok(text) = crate::safe::read_to_string(&path) else {
            continue;
        };
        let kinds = crate::secret::kinds(&text);
        if !kinds.is_empty() {
            let rel = relative(vault, &path);
            add(
                Level::Error,
                "secret",
                ("", &rel),
                format!("{rel}: holds what looks like a secret ({})", kinds.join(", ")),
                "Revoke or rotate it, then replace it with [redacted]; once committed it stays in the vault's git history.".into(),
            );
        }
    }

    // Verified items whose files changed, for session start (`stale::Found`).
    let mut stale_found = Vec::new();
    // Records: format, ids, values.
    let mut records = Vec::new();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for (project, dir) in project_dirs(vault) {
        for (folder, is_item) in [("items", true), ("decisions", false)] {
            for path in markdown_files(&dir.join(folder)) {
                let rel = relative(vault, &path);
                let Ok(text) = crate::safe::read_to_string(&path) else {
                    add(
                        Level::Error,
                        "format",
                        ("", &rel),
                        format!("{rel}: cannot be read"),
                        String::new(),
                    );
                    continue;
                };
                let Some(record) = Record::parse(&text) else {
                    add(
                        Level::Error,
                        "format",
                        ("", &rel),
                        format!("{rel}: no frontmatter, or it is not closed with ---"),
                        "Start the file with the record template's frontmatter.".into(),
                    );
                    continue;
                };
                let required: &[&str] = if is_item {
                    &REQUIRED_ITEM
                } else {
                    &REQUIRED_DECISION
                };
                let missing: Vec<_> = required
                    .iter()
                    .filter(|k| record.get(k).is_none_or(|v| !v.is_truthy()))
                    .copied()
                    .collect();
                if !missing.is_empty() {
                    add(
                        Level::Error,
                        "format",
                        ("", &rel),
                        format!("{rel}: missing {}", missing.join(", ")),
                        "Fill them in from the record template.".into(),
                    );
                }
                let id = Some(record.text("id"))
                    .filter(|i| !i.is_empty())
                    .map_or_else(|| stem(&path), str::to_owned);
                if let Some(first) = ids.get(&id) {
                    add(
                        Level::Error,
                        "id",
                        (&id, &rel),
                        format!("{id} is used twice: {first} and {rel}"),
                        "Renumber one of them.".into(),
                    );
                } else {
                    ids.insert(id.clone(), rel.clone());
                }
                let status = record.text("status");
                let valid_status = if is_item {
                    status.parse::<ItemStatus>().is_ok()
                } else {
                    status.parse::<DecisionStatus>().is_ok()
                };
                if !valid_status && !status.is_empty() {
                    let allowed: Vec<&str> = if is_item {
                        ItemStatus::ALL.iter().map(|s| s.as_str()).collect()
                    } else {
                        DecisionStatus::ALL.iter().map(|s| s.as_str()).collect()
                    };
                    add(
                        Level::Error,
                        "status",
                        (&id, &rel),
                        format!("{id}: status {status:?} is not valid"),
                        format!("Use one of: {}.", allowed.join(", ")),
                    );
                }
                let kind = record.text("type");
                if is_item && !kind.is_empty() && kind.parse::<ItemType>().is_err() {
                    let allowed: Vec<_> = ItemType::ALL.iter().map(|t| t.as_str()).collect();
                    add(
                        Level::Warning,
                        "type",
                        (&id, &rel),
                        format!("{id}: type {kind:?} is not valid"),
                        format!("Use one of: {}.", allowed.join(", ")),
                    );
                }
                records.push((is_item, project.clone(), id, rel, record));
            }
        }
    }

    for (is_item, project, id, rel, record) in &records {
        for other in record.get("related").map(|v| v.items()).unwrap_or_default() {
            if !ids.contains_key(&other) {
                add(
                    Level::Warning,
                    "related",
                    (id, rel),
                    format!("{id}: related {other} does not exist"),
                    format!("Correct it with `sbrain item set {id} related=…`."),
                );
            }
        }
        let status = record.text("status");
        if !is_item {
            if status == "superseded" {
                let by = record.text("superseded_by");
                if by.is_empty() || !ids.contains_key(by) {
                    add(
                        Level::Warning,
                        "superseded",
                        (id, rel),
                        format!(
                            "{id}: superseded, but the decision that replaced it is not named or does not exist"
                        ),
                        "Set superseded_by to the newer decision's id.".into(),
                    );
                }
            }
            continue;
        }
        let verify = record.text("verify");
        if matches!(status, "verified" | "done-unverified") && is_weak_verify(verify) {
            add(
                Level::Warning,
                "weak-verify",
                (id, rel),
                format!("{id}: verify {verify:?} proves nothing"),
                "Write a narrow command that really tests the item.".into(),
            );
        }
        let age = days_since(record.text("updated"), today);
        if status == "done-unverified" {
            if verify.is_empty() {
                add(
                    Level::Warning,
                    "unverified",
                    (id, rel),
                    format!("{id}: done, but it has no verify command"),
                    format!("Add one, then run `sbrain item verify {id}`."),
                );
            } else if let Some(days) = age.filter(|d| *d >= UNVERIFIED_WARN_DAYS) {
                add(
                    Level::Warning,
                    "unverified",
                    (id, rel),
                    format!("{id}: done {days} days ago and still not verified"),
                    format!("Run `sbrain item verify {id}`."),
                );
            }
        }
        if status == "doing"
            && let Some(days) = age.filter(|d| *d >= DOING_WARN_DAYS)
        {
            add(
                Level::Info,
                "forgotten",
                (id, rel),
                format!("{id}: doing for {days} days"),
                "Finish it, move it back to planned, or drop it.".into(),
            );
        }
        if status == "verified" && !quick {
            let repo = repo_for(vault, access, project);
            let st = stale_check(record, repo.as_deref());
            if let crate::stale::Staleness::Checked { changed_commits } = st
                && changed_commits > 0
            {
                stale_found.push(crate::stale::StaleItem {
                    project: project.clone(),
                    id: id.to_owned(),
                    changed_commits,
                });
                add(
                    Level::Warning,
                    "stale",
                    (id, rel),
                    format!(
                        "{id}: its files changed in {} since it was verified",
                        count(changed_commits, "commit")
                    ),
                    format!("Verify it again with `sbrain item verify {id}`."),
                );
            }
        }
    }

    // Files the session-start context cuts.
    let mut sized = vec![
        vault.profile_dir().join("user.md"),
        vault.profile_dir().join("rules.md"),
    ];
    for (_, dir) in project_dirs(vault) {
        sized.push(dir.join("state.md"));
        sized.push(dir.join("gotchas.md"));
    }
    for path in sized {
        let Some(limit) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| SIZE_LIMITS.iter().find(|(f, _)| *f == n))
            .map(|(_, l)| *l)
        else {
            continue;
        };
        let Ok(record) = Record::read(&path) else {
            continue;
        };
        let size = crate::text::strip_comments(&record.body)
            .trim()
            .chars()
            .count();
        if size > limit {
            add(
                Level::Info,
                "size",
                ("", &relative(vault, &path)),
                format!(
                    "{}: {} characters; the session start cuts it at {}",
                    relative(vault, &path),
                    crate::text::thousands(size as u64),
                    crate::text::thousands(limit as u64)
                ),
                "Shorten it; move the detail into items or decisions.".into(),
            );
        }
    }

    // The vault's history and backup.
    let root = vault.root();
    if git(root, &["rev-parse", "--git-dir"]).is_none() {
        add(
            Level::Info,
            "git",
            ("", ""),
            "The vault is not a git repository, so it has no history or backup".into(),
            "Run `git init` in the vault and add a remote to back it up.".into(),
        );
    } else {
        let dirty = git(root, &["status", "--porcelain"])
            .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0);
        if dirty > 0 {
            add(
                Level::Info,
                "git",
                ("", ""),
                format!("{} in the vault not committed", count(dirty, "change")),
                "Commit them, so they are kept in the history.".into(),
            );
        }
        if git(root, &["remote", "get-url", "origin"]).is_none() {
            add(
                Level::Info,
                "push",
                ("", ""),
                "The vault has no origin remote, so it is not backed up".into(),
                "Add a remote and push to it.".into(),
            );
        } else {
            match git(root, &["rev-list", "--count", "@{u}..HEAD"]) {
                None => add(
                    Level::Warning,
                    "push",
                    ("", ""),
                    "The vault's branch has no upstream, so commits are not pushed".into(),
                    "Run `git push -u origin <branch>` in the vault.".into(),
                ),
                Some(out) => {
                    let ahead: usize = out.trim().parse().unwrap_or(0);
                    if ahead > 0 {
                        add(
                            Level::Warning,
                            "push",
                            ("", ""),
                            format!("{} not pushed to origin", count(ahead, "commit")),
                            "Run `git push` in the vault and read the error.".into(),
                        );
                    }
                }
            }
        }
    }

    // Hook failures (hooks never fail a session, so they only show up here).
    let hook_log = vault.cache_dir().join("hook-errors.log");
    if let Ok(text) = crate::safe::read_to_string(&hook_log)
        && let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty())
    {
        add(
            Level::Warning,
            "hook",
            ("", &relative(vault, &hook_log)),
            format!("A hook failed: {}", crate::text::clip(last.trim(), 160)),
            format!("Fix the cause, then delete {}.", relative(vault, &hook_log)),
        );
    }

    // Slow hooks: they run before every prompt, so a slow one is felt in every session.
    let slow_log = vault.cache_dir().join("hook-slow.log");
    if let Ok(text) = crate::safe::read_to_string(&slow_log) {
        let times: Vec<(&str, u128)> = text
            .lines()
            .filter_map(|l| {
                let (_, rest) = l.split_once(' ')?;
                let (hook, ms) = rest.split_once(": ")?;
                Some((hook, ms.trim_end_matches(" ms").parse().ok()?))
            })
            .collect();
        if let Some((hook, ms)) = times.iter().max_by_key(|(_, ms)| *ms) {
            add(
                Level::Info,
                "hook-speed",
                ("", &relative(vault, &slow_log)),
                format!(
                    "Hooks took over {} ms {}; the slowest, {hook}, took {ms} ms",
                    crate::context::SLOW_HOOK_MS,
                    count(times.len(), "time")
                ),
                format!(
                    "Find what makes the hook slow (a large vault, a slow disk); delete {} once it is fixed.",
                    relative(vault, &slow_log)
                ),
            );
        }
    }

    // The job ledger: sessions the sweep gave up on, vault commits still waiting.
    for (level, msg, fix) in ledger_findings(vault) {
        add(level, "ledger", ("", ""), msg, fix);
    }

    // The transcript sweep: sessions it could not distill, and a schedule that stopped running.
    let swept = crate::sweep::State::load(vault);
    if let Some(run) = &swept.last_run {
        let failed: Vec<_> = run
            .sessions
            .iter()
            .filter(|s| s.status == "failed")
            .collect();
        if let Some(first) = failed.first() {
            add(
                Level::Warning,
                "sweep",
                ("", ""),
                format!(
                    "The last sweep could not distill {}: {}",
                    count(failed.len(), "session"),
                    crate::text::clip(&first.reason, 160)
                ),
                "Failed sessions are tried again on the next run; fix the cause first.".into(),
            );
        }
    }
    let every_hours = crate::Settings::load().map_or(1, |s| s.sweep.normalized().every_hours);
    let schedule = crate::sweep::Schedule::load().ok().flatten();
    let now_ts = now
        .and_local_timezone(chrono::Local)
        .earliest()
        .map_or_else(|| now.and_utc().timestamp(), |t| t.timestamp());
    for (level, msg, fix) in schedule_findings(&swept, every_hours, schedule.as_ref(), now_ts) {
        add(level, "sweep", ("", ""), msg, fix);
    }

    findings.sort_by_key(|f| f.level);
    let mut counts = BTreeMap::from([(Level::Error, 0), (Level::Warning, 0), (Level::Info, 0)]);
    for f in &findings {
        *counts.entry(f.level).or_insert(0) += 1;
    }
    let report = Report {
        at: now.format("%Y-%m-%d %H:%M").to_string(),
        counts,
        findings,
    };
    if !quick {
        crate::stale::Found {
            at: now.format("%Y-%m-%d %H:%M").to_string(),
            items: stale_found,
        }
        .save(vault);
    }
    // A cache for the session-start line; failing to write it does not fail the check.
    if let Ok(json) = serde_json::to_string_pretty(&report) {
        let _ = crate::scaffold::write(&vault.cache_dir().join("doctor.json"), &json);
    }
    Ok(report)
}

/// What is wrong with one item or decision file, in the words Doctor uses, with how to fix it; empty when
/// nothing is. Used right after an agent writes the file. A path outside `projects/<p>/items` and
/// `projects/<p>/decisions` is not a record and has no problems.
pub fn record_problems(vault: &Vault, path: &Path) -> Vec<String> {
    let kind = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    let in_projects = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .is_some_and(|p| p == vault.projects_dir());
    let is_item = match kind {
        Some("items") => true,
        Some("decisions") => false,
        _ => return Vec::new(),
    };
    if !in_projects || path.extension().and_then(|e| e.to_str()) != Some("md") {
        return Vec::new();
    }
    let Ok(text) = crate::safe::read_to_string(path) else {
        return Vec::new();
    };
    let Some(record) = Record::parse(&text) else {
        return vec!["no frontmatter, or it is not closed with ---; start the file with the record template's frontmatter".into()];
    };
    let mut problems = Vec::new();
    let required: &[&str] = if is_item {
        &REQUIRED_ITEM
    } else {
        &REQUIRED_DECISION
    };
    let missing: Vec<_> = required
        .iter()
        .filter(|k| record.get(k).is_none_or(|v| !v.is_truthy()))
        .copied()
        .collect();
    if !missing.is_empty() {
        problems.push(format!(
            "missing {}; fill them in from the record template",
            missing.join(", ")
        ));
    }
    let status = record.text("status");
    if !status.is_empty() {
        let allowed: Vec<&str> = if is_item {
            ItemStatus::ALL.iter().map(|s| s.as_str()).collect()
        } else {
            DecisionStatus::ALL.iter().map(|s| s.as_str()).collect()
        };
        if !allowed.contains(&status) {
            problems.push(format!(
                "status {status:?} is not valid; use one of: {}",
                allowed.join(", ")
            ));
        }
    }
    let kind_value = record.text("type");
    if is_item && !kind_value.is_empty() && kind_value.parse::<ItemType>().is_err() {
        let allowed: Vec<_> = ItemType::ALL.iter().map(|t| t.as_str()).collect();
        problems.push(format!(
            "type {kind_value:?} is not valid; use one of: {}",
            allowed.join(", ")
        ));
    }

    // Every other record's id and file, for duplicates and links.
    let id = record.text("id");
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for (_, dir) in project_dirs(vault) {
        for folder in ["items", "decisions"] {
            for other in markdown_files(&dir.join(folder)) {
                if other == path {
                    continue;
                }
                if let Ok(r) = Record::read(&other) {
                    ids.entry(r.text("id").to_owned())
                        .or_insert_with(|| relative(vault, &other));
                }
            }
        }
    }
    if let Some(first) = ids.get(id).filter(|_| !id.is_empty()) {
        problems.push(format!(
            "id {id} is already used by {first}; use the next free id"
        ));
    }
    for other in record.get("related").map(|v| v.items()).unwrap_or_default() {
        if !ids.contains_key(&other) && other != id {
            problems.push(format!(
                "related {other} does not exist; correct or remove it"
            ));
        }
    }
    if !is_item && status == "superseded" {
        let by = record.text("superseded_by");
        if by.is_empty() || !ids.contains_key(by) {
            problems.push(
                "superseded, but superseded_by does not name an existing decision; set it to the newer decision's id"
                    .into(),
            );
        }
    }
    problems
}

/// Commands that pass without testing anything.
pub fn is_weak_verify(command: &str) -> bool {
    let c = command.trim().to_lowercase();
    if c.is_empty() {
        return false;
    }
    if c.trim_end().ends_with("|| true") {
        return true;
    }
    let mut words = c.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest: Vec<_> = words.collect();
    match first {
        "true" | ":" | "pwd" => rest.is_empty(),
        "exit" => rest == ["0"],
        "echo" | "ls" | "cat" => true,
        "test" => rest.len() == 2 && matches!(rest[0], "-d" | "-e" | "-f"),
        _ => false,
    }
}

/// What the job ledger holds that needs a person: sessions the sweep gave up on, and vault commits
/// that keep failing. Nothing when there is no ledger yet.
fn ledger_findings(vault: &Vault) -> Vec<(Level, String, String)> {
    use crate::ledger::{Ledger, MAX_ATTEMPTS, State};
    let mut out = Vec::new();
    if !vault.cache_dir().join("ledger.sqlite").is_file() {
        return out;
    }
    let Ok(ledger) = Ledger::open(vault) else {
        return out;
    };
    for job in ledger
        .list(crate::sweep::DISTILL, State::GaveUp)
        .unwrap_or_default()
    {
        out.push((
            Level::Warning,
            format!(
                "The sweep gave up on session {} after {MAX_ATTEMPTS} tries: {}",
                job.key,
                crate::text::clip(job.last_error.as_deref().unwrap_or("no reason"), 160)
            ),
            format!(
                "Fix the cause, then run `sbrain sweep retry {}`; it is tried again on the next sweep.",
                job.key
            ),
        ));
    }
    let commits: Vec<_> = [State::Pending, State::GaveUp]
        .into_iter()
        .flat_map(|s| ledger.list(crate::git::COMMIT, s).unwrap_or_default())
        .collect();
    if let Some(first) = commits.first() {
        let gave_up = commits.iter().any(|j| j.state == State::GaveUp);
        out.push((
            if gave_up {
                Level::Error
            } else {
                Level::Warning
            },
            format!(
                "{} not made yet: {}",
                count(commits.len(), "vault commit"),
                crate::text::clip(first.last_error.as_deref().unwrap_or("no reason"), 160)
            ),
            "Fix what git says in the vault; every sbrain command tries the commits again.".into(),
        ));
    }
    out
}

/// The sweep on this computer: a scheduled job whose sbrain is gone, a vault that is swept but not on a
/// schedule here, and runs that stopped. Nothing for a vault never swept or swept only by hand.
fn schedule_findings(
    swept: &crate::sweep::State,
    every_hours: u32,
    schedule: Option<&crate::sweep::Schedule>,
    now_ts: i64,
) -> Vec<(Level, String, String)> {
    let mut out = Vec::new();
    if let Some(s) = schedule
        && !s.sbrain.exists()
    {
        out.push((
            Level::Warning,
            format!(
                "The scheduled sweep runs {}, which is gone",
                s.sbrain.display()
            ),
            "Run `sbrain sweep schedule` to point it at the sbrain you use now.".into(),
        ));
    }
    let Some(since) = swept
        .last_run
        .as_ref()
        .map(|r| r.finished)
        .or(swept.baseline)
        .filter(|_| every_hours > 0)
    else {
        return out;
    };
    if schedule.is_none() {
        out.push((
            Level::Info,
            "The sweep is not scheduled on this computer".into(),
            "Run `sbrain sweep schedule` so it runs by itself, or `sbrain sweep` now and then."
                .into(),
        ));
    }
    let limit = i64::from(every_hours.max(12)) * 2 * 3600;
    if now_ts - since > limit {
        let when = chrono::DateTime::from_timestamp(since, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let fix = match schedule {
            Some(s) => match &s.log {
                Some(log) => format!(
                    "Read the job's log ({}), or run `sbrain sweep`.",
                    log.display()
                ),
                None => format!(
                    "Read the job's log (`journalctl --user -u {}`), or run `sbrain sweep`.",
                    s.name.replace(".timer", ".service")
                ),
            },
            None => "Run `sbrain sweep schedule`, or `sbrain sweep` by hand.".into(),
        };
        out.push((Level::Info, format!("Sweep has not run since {when}"), fix));
    }
    out
}

/// `1 commit`, `3 commits`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn days_since(value: &str, today: NaiveDate) -> Option<i64> {
    let date = NaiveDate::parse_from_str(crate::text::take_chars(value, 10), "%Y-%m-%d").ok()?;
    Some((today - date).num_days())
}

fn project_dirs(vault: &Vault) -> Vec<(String, std::path::PathBuf)> {
    let Ok(entries) = std::fs::read_dir(vault.projects_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| Some((e.file_name().to_str()?.to_owned(), e.path())))
        .filter(|(n, _)| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

fn markdown_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .collect();
    out.sort();
    out
}

/// Every text file in the vault, at any depth, except hidden files and folders (`.git`, `.sbrain`).
fn vault_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry
                .file_name()
                .to_str()
                .is_none_or(|n| n.starts_with('.'))
            {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e, "md" | "toml" | "json" | "txt"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Masks the secrets in a vault file the agent wrote; returns what kinds were masked, empty when
/// there were none or the file is not the vault's (`.sbrain`, `.git`, outside it, a link).
pub fn mask_secrets(vault: &Vault, path: &Path) -> Result<Vec<&'static str>> {
    let Ok(rel) = path.strip_prefix(vault.root()) else {
        return Ok(Vec::new());
    };
    if rel.starts_with(".sbrain") || rel.starts_with(".git") {
        return Ok(Vec::new());
    }
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return Ok(Vec::new());
    }
    let text = crate::safe::read_text(path)?;
    let kinds = crate::secret::kinds(&text);
    if !kinds.is_empty() {
        crate::safe::write(path, &crate::secret::redact(&text))?;
    }
    Ok(kinds)
}

fn relative(vault: &Vault, path: &Path) -> String {
    path.strip_prefix(vault.root())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned()
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

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn checks(report: &Report) -> Vec<(Level, &str, &str)> {
        report
            .findings
            .iter()
            .map(|f| (f.level, f.check, f.msg.as_str()))
            .collect()
    }

    #[test]
    fn weak_verify_commands() {
        for weak in [
            "true",
            ":",
            "exit 0",
            "echo ok",
            "ls src",
            "cat x",
            "pwd",
            "test -f a.rs",
            "cargo test || true",
            "  ECHO done ",
        ] {
            assert!(is_weak_verify(weak), "{weak}");
        }
        for strong in [
            "",
            "cargo test -p app",
            "test -f a.rs && cargo test",
            "npm test",
            "truecheck",
            "exit 1",
        ] {
            assert!(!is_weak_verify(strong), "{strong}");
        }
    }

    #[test]
    fn finds_broken_records() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault_without_git(root);
        let item = |id: &str, extra: &str| {
            format!(
                "---\nid: {id}\ntitle: T\ntype: bug\nstatus: inbox\nupdated: 2026-10-25\n{extra}---\n"
            )
        };
        write(
            root,
            "projects/app/items/a-0001-x.md",
            &item("a-0001", "related: [a-0002, a-D-0001]\n"),
        );
        write(root, "projects/app/items/a-0002-x.md", &item("a-0002", ""));
        write(
            root,
            "projects/app/items/a-0002-copy.md",
            &item("a-0002", ""),
        );
        write(root, "projects/app/items/a-0003-x.md", "no frontmatter");
        write(
            root,
            "projects/app/items/a-0004-x.md",
            "---\nid: a-0004\ntitle: T\nstatus: finished\ntype: chore\n---\n",
        );
        write(
            root,
            "projects/app/items/a-0005-x.md",
            "---\nid: a-0005\ntitle:\ntype: bug\nstatus: inbox\nrelated: [gone-1]\n---\n",
        );
        write(
            root,
            "projects/app/items/a-0006-x.md",
            &item("a-0006", "").replace("inbox", "done-unverified"),
        );
        write(
            root,
            "projects/app/items/a-0007-x.md",
            &item("a-0007", "verify: echo ok\n")
                .replace("inbox", "done-unverified")
                .replace("2026-10-25", "2026-10-01"),
        );
        write(
            root,
            "projects/app/items/a-0008-x.md",
            &item("a-0008", "")
                .replace("inbox", "doing")
                .replace("2026-10-25", "2026-10-01"),
        );
        write(
            root,
            "projects/app/decisions/a-D-0001-x.md",
            "---\nid: a-D-0001\ntitle: D\nstatus: superseded\nsuperseded_by: a-D-0009\n---\n",
        );
        write(
            root,
            "projects/app/decisions/a-D-0002-x.md",
            "---\nid: a-D-0002\ntitle: D\nstatus: superseded\nsuperseded_by: a-D-0001\n---\n",
        );
        write(
            root,
            "projects/app/state.md",
            &format!(
                "---\nproject: app\n---\n\n{}<!-- {} -->",
                "x".repeat(2001),
                "y".repeat(5000)
            ),
        );
        write(
            root,
            "projects/app/gotchas.md",
            &format!("{}\n", "z".repeat(800)),
        );
        write(
            root,
            ".sbrain/cache/hook-errors.log",
            "old\nsession-start: boom\n\n",
        );
        write(
            root,
            ".sbrain/cache/hook-slow.log",
            "2026-10-25T09:00:00 prompt: 300 ms\n2026-10-25T09:01:00 session-start: 1000 ms\n",
        );

        let report = doctor(&vault, &Access::auto(), at("2026-10-25 09:44"), true).unwrap();
        let found = checks(&report);
        let has = |level, check: &str, text: &str| {
            assert!(
                found
                    .iter()
                    .any(|(l, c, m)| *l == level && *c == check && m.contains(text)),
                "missing {check} {text:?} in {found:#?}"
            );
        };
        has(Level::Error, "id", "a-0002 is used twice");
        has(Level::Error, "format", "a-0003-x.md: no frontmatter");
        has(Level::Error, "format", "a-0005-x.md: missing title");
        has(Level::Error, "status", "a-0004: status \"finished\"");
        has(Level::Warning, "type", "a-0004: type \"chore\"");
        has(Level::Warning, "related", "a-0005: related gone-1");
        has(Level::Warning, "superseded", "a-D-0001: superseded");
        has(
            Level::Warning,
            "unverified",
            "a-0006: done, but it has no verify",
        );
        has(Level::Warning, "unverified", "a-0007: done 24 days ago");
        has(Level::Warning, "weak-verify", "a-0007: verify \"echo ok\"");
        has(Level::Info, "forgotten", "a-0008: doing for 24 days");
        has(
            Level::Info,
            "size",
            "projects/app/state.md: 2,001 characters",
        );
        has(Level::Warning, "hook", "session-start: boom");
        has(
            Level::Info,
            "hook-speed",
            "over 250 ms 2 times; the slowest, session-start, took 1000 ms",
        );
        has(Level::Info, "git", "not a git repository");
        // Each finding names the record and the file it is about, when there is one.
        let about = |check: &str, text: &str| {
            let f = report
                .findings
                .iter()
                .find(|f| f.check == check && f.msg.contains(text))
                .unwrap();
            (f.record.as_str(), f.file.as_str())
        };
        assert_eq!(
            about("weak-verify", "a-0007"),
            ("a-0007", "projects/app/items/a-0007-x.md")
        );
        assert_eq!(
            about("format", "a-0003-x.md"),
            ("", "projects/app/items/a-0003-x.md")
        );
        assert_eq!(about("size", "state.md"), ("", "projects/app/state.md"));
        assert_eq!(about("git", "not a git repository"), ("", ""));
        // Links that resolve, and files within their limit, are not reported.
        assert!(!found.iter().any(|(_, _, m)| m.contains("a-0001: related")));
        assert!(!found.iter().any(|(_, _, m)| m.contains("a-D-0002")));
        assert!(!found.iter().any(|(_, _, m)| m.contains("gotchas.md")));
        // Errors first, and the counts match.
        assert_eq!(report.findings[0].level, Level::Error);
        assert_eq!(report.counts[&Level::Error], 4);
        assert_eq!(report.at, "2026-10-25 09:44");

        // The cache is in the shape the session-start line reads.
        let cached: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(vault.cache_dir().join("doctor.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cached["counts"]["error"], 4);
        assert_eq!(cached["findings"][0]["level"], "error");
        assert!(cached["findings"][0]["msg"].is_string());
    }

    #[test]
    fn checks_one_record_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault_without_git(root);
        write(
            root,
            "projects/app/items/a-0001-x.md",
            "---\nid: a-0001\ntitle: T\ntype: bug\nstatus: inbox\n---\n",
        );
        let good = vault.root().join("projects/app/items/a-0001-x.md");
        assert!(record_problems(&vault, &good).is_empty());

        write(
            root,
            "projects/app/items/a-0002-x.md",
            "---\nid: a-0001\ntitle:\ntype: chore\nstatus: done\nrelated: [a-0001, gone-1]\n---\n",
        );
        let bad = vault.root().join("projects/app/items/a-0002-x.md");
        let problems = record_problems(&vault, &bad);
        for part in [
            "missing title",
            "status \"done\"",
            "type \"chore\"",
            "id a-0001 is already used",
            "related gone-1",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} in {problems:#?}"
            );
        }
        assert!(
            !problems.iter().any(|p| p.contains("related a-0001")),
            "a link to an existing id is fine"
        );

        write(root, "projects/app/state.md", "no frontmatter");
        assert!(record_problems(&vault, &vault.root().join("projects/app/state.md")).is_empty());
        write(root, "notes/x.md", "no frontmatter");
        assert!(record_problems(&vault, &vault.root().join("notes/x.md")).is_empty());
        write(root, "projects/app/decisions/a-D-0001-x.md", "id: x");
        assert_eq!(
            record_problems(
                &vault,
                &vault.root().join("projects/app/decisions/a-D-0001-x.md")
            )
            .len(),
            1
        );
    }

    #[test]
    fn secrets_are_reported_and_masked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let vault = crate::vault::tests::new_vault_without_git(root);
        let key = format!("ghp_{}", "a1B2".repeat(9));
        write(root, "notes/deploy.md", &format!("token {key}\n"));
        write(
            root,
            ".sbrain/cache/x.json",
            &format!("{{\"k\": \"{key}\"}}"),
        );
        let report = doctor(&vault, &Access::auto(), at("2026-10-30 10:00"), true).unwrap();
        let secrets: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.check == "secret")
            .collect();
        assert_eq!(secrets.len(), 1, "{secrets:#?}");
        assert_eq!(secrets[0].file, "notes/deploy.md");
        assert!(secrets[0].msg.contains("GitHub token"));

        let path = vault.root().join("notes/deploy.md");
        assert_eq!(mask_secrets(&vault, &path).unwrap(), ["GitHub token"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), "token [redacted]\n");
        assert!(mask_secrets(&vault, &path).unwrap().is_empty());
        let cache = vault.root().join(".sbrain/cache/x.json");
        assert!(
            mask_secrets(&vault, &cache).unwrap().is_empty(),
            "the machine's folder is not the vault's"
        );
        assert!(fs::read_to_string(cache).unwrap().contains(&key));
    }

    #[test]
    fn reports_sessions_the_sweep_could_not_distill() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        let state = crate::sweep::State {
            baseline: Some(1),
            last_run: Some(crate::sweep::Run {
                started: 1,
                finished: 2,
                sessions: vec![crate::sweep::RunSession {
                    session: "abc".into(),
                    status: "failed".into(),
                    reason: "claude failed: Not logged in".into(),
                    ..crate::sweep::RunSession::default()
                }],
                ..crate::sweep::Run::default()
            }),
        };
        state.save(&vault).unwrap();
        let report = doctor(&vault, &Access::auto(), at("2026-10-25 09:44"), true).unwrap();
        let found = checks(&report);
        assert!(
            found.iter().any(|(l, c, m)| *l == Level::Warning
                && *c == "sweep"
                && m.contains("could not distill 1 session: claude failed: Not logged in")),
            "{found:#?}"
        );
    }

    /// A full run keeps what it found stale for session start; a quick one does not check, so it keeps
    /// the last result.
    #[test]
    fn a_full_run_keeps_what_is_stale_for_session_start() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let vault = crate::vault::tests::new_vault(&root.join("vault"));
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        fs::write(repo.join("a.rs"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        crate::cache::remember_repo(&vault, "app", &repo).unwrap();
        write(
            vault.root(),
            "projects/app/items/app-0001-a.md",
            "---\nid: app-0001\ntitle: A\ntype: bug\nstatus: verified\nfiles: [a.rs]\nverify: cargo test\nverified_at: 2000-01-01 00:00\n---\n",
        );
        doctor(&vault, &Access::auto(), at("2026-10-26 10:00"), false).unwrap();
        let found = crate::stale::Found::load(&vault).unwrap();
        assert_eq!(found.at, "2026-10-26 10:00");
        assert_eq!(found.items.len(), 1, "{found:?}");
        assert_eq!(found.items[0].id, "app-0001");

        doctor(&vault, &Access::auto(), at("2026-10-27 10:00"), true).unwrap();
        assert_eq!(
            crate::stale::Found::load(&vault).unwrap().at,
            "2026-10-26 10:00",
            "a quick run keeps the last check"
        );
    }

    #[test]
    fn a_new_vault_in_git_is_healthy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // With git installed, a new vault starts its history with the first save.
        let vault = crate::vault::tests::new_vault(root);
        let report = doctor(&vault, &Access::auto(), at("2026-10-25 09:44"), false).unwrap();
        let found = checks(&report);
        // Only the missing remote remains (the cache it writes is ignored by the vault's .gitignore).
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].1, "push");
    }

    #[test]
    fn sweep_schedule_findings() {
        use crate::sweep::{Run, Schedule, State};
        let hour = 3600;
        let now = 1_000 * hour;
        let sbrain = std::env::current_exe().unwrap();
        let job = |sbrain: std::path::PathBuf| Schedule {
            service: "launchd".into(),
            name: "com.emredagci.sbrain.sweep".into(),
            sbrain,
            env: Vec::new(),
            files: Vec::new(),
            log: Some("/x/sweep.log".into()),
        };
        let messages = |state: &State, every: u32, s: Option<&Schedule>| {
            schedule_findings(state, every, s, now)
                .into_iter()
                .map(|(l, m, f)| (l, format!("{m} | {f}")))
                .collect::<Vec<_>>()
        };

        let never = State::default();
        assert!(messages(&never, 1, None).is_empty(), "never swept: no hint");

        let mut recent = State {
            baseline: Some(now - 2 * hour),
            ..State::default()
        };
        assert!(messages(&recent, 1, Some(&job(sbrain.clone()))).is_empty());
        assert!(messages(&recent, 0, None).is_empty(), "by hand: no hint");
        let found = messages(&recent, 1, None);
        assert_eq!(found.len(), 1);
        assert!(found[0].1.starts_with("The sweep is not scheduled"));

        recent.last_run = Some(Run {
            finished: now - 30 * hour,
            ..Run::default()
        });
        let found = messages(&recent, 1, Some(&job(sbrain.clone())));
        assert_eq!(found.len(), 1);
        assert!(found[0].1.contains("Sweep has not run since"));
        assert!(found[0].1.contains("/x/sweep.log"), "{found:?}");
        assert!(
            messages(&recent, 24, Some(&job(sbrain))).is_empty(),
            "daily: 30 hours is not late"
        );

        let found = messages(&never, 1, Some(&job("/gone/sbrain".into())));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Level::Warning);
        assert!(found[0].1.contains("/gone/sbrain, which is gone"));
    }
}
