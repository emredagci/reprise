//! The text sbrain adds to a coding agent's context: the session-start block, the handoff reminder
//! when the context fills up, and pointers to related records for a prompt.
//!
//! The text is English whatever language the records are in; the agent answers in the language the
//! user's profile asks for.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::NaiveDate;

use crate::project::{Resolver, Rule};
use crate::record::Record;
use crate::report::report;
use crate::search::search;
use crate::text::{clip, strip_comments, take_chars, tokens};
use crate::{Access, Result, Vault};

/// Upper bound of the session-start block, in characters.
pub const SESSION_START_BUDGET: usize = 6000;

/// Words left out of prompt terms, English and Turkish, folded to ASCII like tokens. Sorted.
const STOPWORDS: &[&str] = &[
    "ama",
    "and",
    "bana",
    "ben",
    "bir",
    "biz",
    "bize",
    "bu",
    "bunu",
    "cok",
    "da",
    "daha",
    "de",
    "diye",
    "eger",
    "ekle",
    "ekleyelim",
    "for",
    "from",
    "gerek",
    "gibi",
    "hem",
    "her",
    "hic",
    "icin",
    "ile",
    "into",
    "ise",
    "kadar",
    "lazim",
    "mi",
    "mu",
    "nasil",
    "ne",
    "neden",
    "niye",
    "olan",
    "oldu",
    "olsun",
    "once",
    "onu",
    "sana",
    "sen",
    "sey",
    "seyi",
    "seyler",
    "simdi",
    "sonra",
    "su",
    "sunu",
    "that",
    "the",
    "this",
    "tum",
    "var",
    "ve",
    "veya",
    "what",
    "when",
    "where",
    "which",
    "with",
    "ya",
    "yap",
    "yapalim",
    "yaptik",
    "yok",
];

/// Sessions the transcript distiller runs (in `sweep::distill_dir`) get no hook context.
pub fn is_distiller_session(cwd: &Path) -> bool {
    crate::sweep::distill_dir().is_ok_and(|d| cwd.starts_with(d.canonicalize().unwrap_or(d)))
}

/// Whether a file says anything beyond its template: a line that is not blank, not a heading and not
/// an unfilled `- Label:` bullet.
pub(crate) fn has_content(text: &str) -> bool {
    text.lines().map(str::trim).any(|line| {
        !line.is_empty()
            && !line.starts_with('#')
            && !(line.starts_with("- ") && line.ends_with(':'))
    })
}

/// A file's body without HTML comments, or `None` when it has no content of its own. Cut at `limit`
/// characters with the full path, so the agent knows to open it.
fn body(path: &Path, limit: usize) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let record = Record::read(path)?;
    let text = strip_comments(&record.body);
    let text = text.trim();
    if !has_content(text) {
        return Ok(None);
    }
    if text.chars().count() <= limit {
        return Ok(Some(text.to_owned()));
    }
    Ok(Some(format!(
        "{}\n[cut: read {} for the rest]",
        take_chars(text, limit).trim_end(),
        path.display()
    )))
}

/// `session` is the agent's session id, for the handoff card; one that is not a plain id is left out.
pub fn session_start(
    vault: &Vault,
    access: &Access,
    cwd: &Path,
    session: Option<&str>,
    today: NaiveDate,
) -> Result<String> {
    let res = Resolver::new(vault, access).resolve(cwd);
    let project = res.project.as_deref();
    let head = match (project, res.rule) {
        (Some(p), _) => format!("project: {p}"),
        (None, Rule::Vault) => "inside the vault; no project".to_owned(),
        (None, _) => {
            "no project (not a repository sbrain may access); nothing is recorded".to_owned()
        }
    };
    let repo = res
        .repo
        .as_ref()
        .map(|r| format!(" · repo: {}", r.display()))
        .unwrap_or_default();
    let mut parts = vec![format!("# sbrain · {head}{repo}")];

    let profile = vault.profile_dir();
    let user = profile.join("user.md");
    match body(&user, 1500)? {
        Some(text) => parts.push(format!("## User\n{text}")),
        None => parts.push(format!(
            "## User\nNot filled in yet. Offer once to write what you learn about the user into {}.",
            user.display()
        )),
    }
    if let Some(rules) = body(&profile.join("rules.md"), 1500)? {
        parts.push(format!("## Rules\n{rules}"));
    }

    if let (Some(p), Some(repo)) = (project, &res.repo) {
        // Memory starts with the first session: the project folder is created from the templates.
        // A failure here must not cost the session its context.
        let _ = crate::scaffold::ensure_project(vault, p, res.remote.as_deref(), today);
        let _ = crate::cache::remember_repo(vault, p, repo);
        let root = vault.project_dir(p);
        if let Some(state) = body(&root.join("state.md"), 2000)? {
            parts.push(format!("## {p} state\n{state}"));
        }
        if let Some(gotchas) = body(&root.join("gotchas.md"), 800)? {
            parts.push(format!("## {p} gotchas\n{gotchas}"));
        }
        let reset = crate::reset::active(vault, p);
        if let Some(facts) = facts_block(vault, p, reset) {
            parts.push(facts);
        }
        if let Some(events) = events_block(vault, p, today, reset) {
            parts.push(events);
        }
        let lines = open_work(vault, p, today, reset)?;
        if !lines.is_empty() {
            parts.push(format!("## Open work\n{}", lines.join("\n")));
        }
    }

    if let Some(line) = doctor_line(vault) {
        parts.push(line);
    }
    if let Some(line) = proposals_line(vault, project) {
        parts.push(line);
    }
    let guide = vault.root().join("AGENTS.md");
    let records = match project {
        Some(p) => format!(
            " This project's records: {}.",
            vault.project_dir(p).display()
        ),
        None => String::new(),
    };
    if let (Some(p), Some(id)) = (project, session.filter(|s| is_session_id(s))) {
        parts.push(format!(
            "Handoff card for this session: `sbrain session write --project {p} --session {id} \
             --done … --open … --next …`."
        ));
    }
    parts.push(format!(
        "This block is sbrain's memory; do not read these files again.{records} \
         How to answer from and keep the memory: {}. \
         Answer questions about the past from the records and say what you could not find.",
        guide.display()
    ));
    Ok(clip(&parts.join("\n\n"), SESSION_START_BUDGET))
}

/// Characters of the facts and of the events in the session-start block.
const FACTS_LIMIT: usize = 600;
const EVENTS_LIMIT: usize = 500;
/// Events that happened shown at session start, the latest ones.
const EVENTS_SHOWN: usize = 4;

/// The facts that hold now, the project's and those about no single project.
fn facts_block(vault: &Vault, project: &str, reset: Option<NaiveDate>) -> Option<String> {
    let mut lines = Vec::new();
    for scope in [Some(project), None] {
        let facts = crate::facts::read(vault, scope).unwrap_or_default();
        // After a project's reset, a project fact from before it is no longer current.
        let before_reset =
            |f: &crate::facts::Fact| scope.is_some() && reset.is_some_and(|r| f.from.start < r);
        for f in crate::facts::current(&facts)
            .into_iter()
            .filter(|f| !before_reset(f))
        {
            lines.push(format!(
                "- {}: {} (since {})",
                f.key,
                f.value,
                f.from.display()
            ));
        }
    }
    (!lines.is_empty()).then(|| {
        format!(
            "## Facts now\n{}\nOlder values: `sbrain fact list --all`.",
            clip(&lines.join("\n"), FACTS_LIMIT)
        )
    })
}

/// The latest events and the next planned ones, with how long ago, computed for today.
fn events_block(
    vault: &Vault,
    project: &str,
    today: NaiveDate,
    reset: Option<NaiveDate>,
) -> Option<String> {
    let mut events = crate::events::read(vault, project).ok()?;
    events.retain(|e| reset.is_none_or(|r| e.when.end >= r));
    let done: Vec<&crate::events::Event> = events
        .iter()
        .filter(|e| e.status == "done" && e.when.start <= today)
        .collect();
    let planned = events
        .iter()
        .filter(|e| e.status == "planned" && e.when.end >= today)
        .take(2);
    let lines: Vec<String> = done[done.len().saturating_sub(EVENTS_SHOWN)..]
        .iter()
        .copied()
        .chain(planned)
        .map(|e| {
            let status = if e.status == "done" {
                String::new()
            } else {
                format!(" [{}]", e.status)
            };
            format!(
                "- {} · {} · {}{status} ({})",
                e.when.display(),
                e.kind,
                e.what,
                crate::events::ago(&e.when, today)
            )
        })
        .collect();
    (!lines.is_empty()).then(|| {
        format!(
            "## Recent events\n{}\nCounts, gaps and dates: `sbrain timeline`; quote it, do not work dates out.",
            clip(&lines.join("\n"), EVENTS_LIMIT)
        )
    })
}

/// Rules the sweep proposed for this project or for every project, waiting for the user: only their
/// number reaches the agent, never their text, so a rule is in effect only once the user accepts it.
fn proposals_line(vault: &Vault, project: Option<&str>) -> Option<String> {
    let n = crate::store::proposals(vault)
        .ok()?
        .iter()
        .filter(|p| p.project.is_none() || p.project.as_deref() == project)
        .count();
    (n > 0).then(|| {
        format!(
            "{n} proposed rule{} found in earlier sessions wait for the user (`sbrain rule list`). They \
             are not in effect: tell the user in one line; accept one only when the user says so.",
            if n == 1 { "" } else { "s" }
        )
    })
}

fn open_work(
    vault: &Vault,
    project: &str,
    today: NaiveDate,
    reset: Option<NaiveDate>,
) -> Result<Vec<String>> {
    let mut r = report(vault, project, today)?;
    let mut lines = Vec::new();
    if let Some(day) = reset {
        // Work not touched since the reset is left out; it stays in the vault and in search.
        let touched = |it: &crate::report::Row| it.updated.is_none_or(|u| u >= day);
        for rows in [
            &mut r.doing,
            &mut r.done_unverified,
            &mut r.planned,
            &mut r.inbox,
            &mut r.stale_verified,
        ] {
            rows.retain(|it| touched(it));
        }
        if r.last_session
            .as_ref()
            .is_some_and(|ls| ls.date.as_str() < day.format("%Y-%m-%d").to_string().as_str())
        {
            r.last_session = None;
        }
        lines.push(format!(
            "Reset on {day}: what came before is left out here (`sbrain items`, `sbrain search` still have it)."
        ));
    }
    if !r.counts.is_empty() {
        lines.push(
            r.counts
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    for (label, rows, n) in [
        ("Doing", &r.doing, 5),
        ("Unverified", &r.done_unverified, 5),
        ("Planned", &r.planned, 3),
        ("Inbox", &r.inbox, 3),
    ] {
        for it in rows.iter().take(n) {
            let age = it
                .age_days
                .map(|d| format!(" ({d} days)"))
                .unwrap_or_default();
            lines.push(format!("- {label}: {} · {}{age}", it.id, it.title));
        }
        if rows.len() > n {
            lines.push(format!("- {label}: +{} more", rows.len() - n));
        }
    }
    for it in r.stale_verified.iter().take(3) {
        lines.push(format!("- Possibly stale: {} · {}", it.id, it.title));
    }
    if let Some(ls) = &r.last_session {
        let rel = ls.path.strip_prefix(vault.root()).unwrap_or(&ls.path);
        lines.push(format!("- Last handoff {}: {}", ls.date, rel.display()));
        if !ls.next.is_empty() {
            lines.push(format!(
                "  Next step: {}",
                clip(&ls.next.replace('\n', " "), 300)
            ));
        }
    }
    Ok(lines)
}

/// The doctor's one-line summary from `.sbrain/cache/doctor.json`, when it found errors or warnings.
fn doctor_line(vault: &Vault) -> Option<String> {
    let text = crate::safe::read_to_string(vault.cache_dir().join("doctor.json")).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let count = |k: &str| {
        doc.get("counts")
            .and_then(|c| c.get(k))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let (errors, warnings) = (count("error"), count("warning"));
    if errors == 0 && warnings == 0 {
        return None;
    }
    let at = doc.get("at").and_then(|v| v.as_str()).unwrap_or("?");
    let mut line = format!("Doctor ({at}): {errors} errors, {warnings} warnings");
    let first = doc
        .get("findings")
        .and_then(|f| f.as_array())
        .and_then(|f| {
            f.iter().find(|x| {
                matches!(
                    x.get("level").and_then(|l| l.as_str()),
                    Some("error" | "warning")
                )
            })
        })
        .and_then(|f| f.get("msg"))
        .and_then(|m| m.as_str());
    if let Some(msg) = first {
        line.push_str(&format!(". First: {}", clip(msg, 160)));
    }
    Some(format!("{line}. Tell the user in one line."))
}

/// A hook that takes longer than this is noted in `.sbrain/cache/hook-slow.log`, which the doctor reports.
pub const SLOW_HOOK_MS: u128 = 250;
/// Lines the slow-hook log keeps; older ones are dropped.
const SLOW_LOG_LINES: usize = 50;

/// Notes a hook that took `ms` milliseconds, when that is over `SLOW_HOOK_MS`.
pub fn note_hook_time(vault: &Vault, hook: &str, ms: u128) {
    if ms <= SLOW_HOOK_MS {
        return;
    }
    let path = vault.cache_dir().join("hook-slow.log");
    let line = format!(
        "{} {hook}: {ms} ms",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
    );
    let _ = crate::safe::append_line(&path, &line);
    if let Ok(text) = crate::safe::read_to_string(&path) {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > SLOW_LOG_LINES {
            let kept = lines[lines.len() - SLOW_LOG_LINES..].join("\n");
            let _ = crate::safe::write(&path, &format!("{kept}\n"));
        }
    }
}

/// A session id as agents write them (a UUID, letters, digits, `-` and `_`), safe as a file name.
pub fn is_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The handoff reminder: once per session for each threshold the context usage crosses.
/// The last threshold reached is remembered in `.sbrain/cache/hooks/<session>.json`.
pub fn usage_notice(
    vault: &Vault,
    session_id: &str,
    used: u64,
    model: &str,
) -> Result<Option<String>> {
    // The id becomes a file name: only the characters agents use in ids, so it cannot leave the folder.
    if !is_session_id(session_id) {
        return Ok(None);
    }
    let cfg = vault.config().hooks;
    let pct = 100 * used / cfg.context_window(model, used).max(1);
    let dir = vault.cache_dir().join("hooks");
    crate::safe::create_dir_all(&dir)?;
    let state_file = dir.join(format!("{session_id}.json"));
    let warned = crate::safe::read_to_string(&state_file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("warned").and_then(|w| w.as_u64()))
        .unwrap_or(0);
    let level = cfg
        .context_thresholds
        .iter()
        .copied()
        .filter(|t| pct >= *t)
        .max()
        .unwrap_or(0);
    if level <= warned {
        return Ok(None);
    }
    crate::safe::write(&state_file, &format!("{{\"warned\": {level}}}"))?;
    Ok(Some(format!(
        "Context is at ~{pct}% ({} tokens). At the end of this answer, briefly suggest handing off: \
         a handoff card and updated records (see the vault's AGENTS.md), then a new session.",
        crate::text::thousands(used)
    )))
}

/// Up to three items or decisions of the current project that share at least two terms with the prompt.
pub fn related_records(
    vault: &Vault,
    access: &Access,
    cwd: &Path,
    prompt: &str,
) -> Result<Option<String>> {
    let prompt = prompt.trim();
    if prompt.chars().count() < 15 || prompt.starts_with('/') {
        return Ok(None);
    }
    let terms: Vec<String> = tokens(prompt)
        .into_iter()
        .filter(|t| t.chars().count() >= 4 && STOPWORDS.binary_search(&t.as_str()).is_err())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(12)
        .collect();
    // Outside any project there is nothing to point at.
    let Some(project) = Resolver::new(vault, access).resolve(cwd).project else {
        return Ok(None);
    };
    if terms.is_empty() {
        return Ok(None);
    }
    let found = search(vault, &[project], &terms, 50)?;
    let mut picks: Vec<_> = found
        .hits
        .into_iter()
        .filter(|h| matches!(h.kind, "item" | "decision") && h.matched.len() >= 2)
        .collect();
    picks.sort_by_key(|h| std::cmp::Reverse((h.matched.len(), h.score)));
    if picks.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = picks
        .iter()
        .take(3)
        .map(|h| format!("- {} · {} · {}", h.id, h.title, h.status))
        .collect();
    Ok(Some(format!(
        "Possibly related sbrain records (titles only; open them when the history matters):\n{}",
        lines.join("\n")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    struct Env {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        vault: Vault,
    }

    impl Env {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().canonicalize().unwrap();
            let vault = crate::vault::tests::new_vault(&base.join("vault"));
            Self {
                _tmp: tmp,
                base,
                vault,
            }
        }

        fn repo(&self, rel: &str, remote: Option<&str>) -> PathBuf {
            let dir = self.base.join(rel);
            fs::create_dir_all(&dir).unwrap();
            let git = |args: &[&str]| {
                assert!(
                    Command::new("git")
                        .arg("-C")
                        .arg(&dir)
                        .args(args)
                        .output()
                        .unwrap()
                        .status
                        .success()
                )
            };
            git(&["init", "-q"]);
            if let Some(remote) = remote {
                git(&["remote", "add", "origin", remote]);
            }
            dir
        }

        fn write(&self, rel: &str, text: &str) {
            let p = self.vault.root().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }

        fn start(&self, cwd: &Path) -> String {
            session_start(&self.vault, &Access::auto(), cwd, None, day()).unwrap()
        }
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()
    }

    #[test]
    fn stopwords_are_sorted_ascii() {
        assert!(STOPWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(STOPWORDS.iter().all(|w| tokens(w) == [*w]));
    }

    #[test]
    fn first_session_creates_the_project_and_points_at_the_guide() {
        let env = Env::new();
        let repo = env.repo("code/api", Some("git@github.com:acme/api.git"));
        let text = env.start(&repo);

        assert!(text.starts_with(&format!(
            "# sbrain · project: api · repo: {}\n\n## User\nNot filled in yet.",
            repo.display()
        )));
        assert!(
            !text.contains("## Rules"),
            "an empty rules file is left out"
        );
        assert!(
            !text.contains("## api state"),
            "an unfilled state.md is left out"
        );
        assert!(!text.contains("## Open work"));
        assert!(text.contains(&env.vault.root().join("AGENTS.md").display().to_string()));
        assert!(text.contains(&env.vault.project_dir("api").display().to_string()));

        let dir = env.vault.project_dir("api");
        let cfg = crate::project::ProjectToml::read(&dir.join("project.toml")).unwrap();
        assert_eq!(cfg.repos, ["github.com/acme/api"]);
        assert!(dir.join("state.md").is_file() && dir.join("gotchas.md").is_file());
        assert_eq!(crate::cache::repo_path(&env.vault, "api"), Some(repo));
    }

    #[test]
    fn session_start_gives_the_handoff_command_with_the_session_id() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        let start = |session| session_start(&env.vault, &Access::auto(), &repo, session, day());
        let text = start(Some("1a2b3c4d-0000")).unwrap();
        assert!(
            text.contains("`sbrain session write --project api --session 1a2b3c4d-0000 "),
            "{text}"
        );
        assert!(!start(None).unwrap().contains("session write"));
        assert!(
            !start(Some("x; rm -rf ~"))
                .unwrap()
                .contains("session write"),
            "an id that is not plain is left out"
        );
    }

    #[test]
    fn facts_and_events_come_with_their_dates_worked_out() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.write(
            "projects/api/facts.md",
            "| Key | Value | From | To | Source | Noted |\n| --- | --- | --- | --- | --- | --- |\n\
             | port | 8080 | 2026-01 | 2026-10-01 |  | 2026-10-01 |\n\
             | port | 9090 | 2026-10-01 |  |  | 2026-10-01 |\n",
        );
        env.write(
            "projects/api/events.md",
            "| When | What | Kind | Status | Said | Source |\n| --- | --- | --- | --- | --- | --- |\n\
             | 2026-10-20 | Shipped login | release | done | 2026-10-20 |  |\n\
             | 2026-11-02 | Ship billing | release | planned | 2026-10-20 |  |\n",
        );
        let text = env.start(&repo);
        assert!(
            text.contains("## Facts now\n- port: 9090 (since 2026-10-01)\n"),
            "{text}"
        );
        assert!(
            !text.contains("8080"),
            "a closed value is not current: {text}"
        );
        assert!(
            text.contains("- 2026-10-20 · release · Shipped login (5 days ago)"),
            "{text}"
        );
        assert!(
            text.contains("- 2026-11-02 · release · Ship billing [planned] (in 8 days)"),
            "{text}"
        );
    }

    #[test]
    fn a_reset_leaves_the_past_out_of_session_start() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.write(
            "projects/api/facts.md",
            "| Key | Value | From | To | Source | Noted |\n| --- | --- | --- | --- | --- | --- |\n\
             | team | old | 2026-01 |  |  | 2026-01-01 |\n\
             | port | 9090 | 2026-10-21 |  |  | 2026-10-21 |\n",
        );
        env.write(
            "projects/api/events.md",
            "| When | What | Kind | Status | Said | Source |\n| --- | --- | --- | --- | --- | --- |\n\
             | 2026-10-01 | Old launch | release | done | 2026-10-01 |  |\n\
             | 2026-10-22 | New launch | release | done | 2026-10-22 |  |\n",
        );
        env.write(
            "projects/api/items/api-0001-a.md",
            "---\nid: api-0001\ntitle: Old work\nstatus: doing\nupdated: 2026-10-01\n---\n",
        );
        env.write(
            "projects/api/items/api-0002-b.md",
            "---\nid: api-0002\ntitle: New work\nstatus: doing\nupdated: 2026-10-23\n---\n",
        );
        let before = env.start(&repo);
        assert!(
            before.contains("team: old") && before.contains("Old work"),
            "{before}"
        );

        crate::reset::reset(&env.vault, "api", day() - chrono::Duration::days(5), "").unwrap();
        let after = env.start(&repo);
        assert!(after.contains("Reset on 2026-10-20"), "{after}");
        assert!(!after.contains("team: old"), "{after}");
        assert!(after.contains("port: 9090"), "{after}");
        assert!(
            !after.contains("Old launch") && after.contains("New launch"),
            "{after}"
        );
        assert!(
            !after.contains("Old work") && after.contains("New work"),
            "{after}"
        );

        crate::reset::undo(&env.vault, "api", day()).unwrap();
        assert!(env.start(&repo).contains("Old work"));
    }

    #[test]
    fn filled_records_appear_in_order() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.write(
            "profile/user.md",
            "# User\n\n- Language: English\n- Tone:\n",
        );
        env.write(
            "profile/rules.md",
            "# Rules\n<!-- how -->\n- 2026-10-24: Read first.\n",
        );
        env.write(
            "projects/api/state.md",
            "---\nproject: api\n---\n# api\n\n## What\nAn API.\n",
        );
        env.write(
            "projects/api/items/api-0001-a.md",
            "---\nid: api-0001\ntitle: Login\nstatus: done-unverified\nupdated: 2026-10-20\n---\n",
        );
        env.write(
            "projects/api/sessions/2026-10-24-1000-aaaa.md",
            "---\ndate: 2026-10-24 10:00\n---\n## Next step\nship it\n",
        );
        let text = env.start(&repo);
        let order: Vec<usize> = [
            "## User\n# User\n\n- Language: English",
            "## Rules\n# Rules\n\n- 2026-10-24: Read first.",
            "## api state\n# api\n\n## What\nAn API.",
            "## Open work\ndone-unverified: 1\n- Unverified: api-0001 · Login (5 days)",
            "- Last handoff 2026-10-24 10:00: projects/api/sessions/2026-10-24-1000-aaaa.md\n  Next step: ship it",
            "This block is sbrain's memory",
        ]
        .iter()
        .map(|part| text.find(part).unwrap_or_else(|| panic!("{part}\n---\n{text}")))
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(
            !text.contains("gotchas"),
            "an unfilled gotchas.md is left out"
        );
    }

    #[test]
    fn long_files_are_cut_with_their_path() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.write("profile/user.md", &"word ".repeat(400));
        let text = env.start(&repo);
        let user = env.vault.profile_dir().join("user.md");
        assert!(text.contains(&format!("[cut: read {} for the rest]", user.display())));
        assert!(text.chars().count() <= SESSION_START_BUDGET + 20);
    }

    #[test]
    fn outside_projects_nothing_is_created() {
        let env = Env::new();
        let plain = env.base.join("Downloads");
        fs::create_dir_all(&plain).unwrap();
        assert!(env.start(&plain).starts_with("# sbrain · no project"));
        assert!(
            env.start(env.vault.root())
                .starts_with("# sbrain · inside the vault")
        );
        assert_eq!(fs::read_dir(env.vault.projects_dir()).unwrap().count(), 0);
    }

    #[test]
    fn doctor_summary_only_with_findings() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.write(
            ".sbrain/cache/doctor.json",
            r#"{"at":"2026-10-25T10:00","counts":{"error":0,"warning":0},"findings":[]}"#,
        );
        assert!(!env.start(&repo).contains("Doctor"));
        env.write(
            ".sbrain/cache/doctor.json",
            r#"{"at":"2026-10-25T10:00","counts":{"error":1,"warning":2},"findings":[{"level":"info","msg":"i"},{"level":"warning","msg":"w1"}]}"#,
        );
        assert!(env.start(&repo).contains(
            "Doctor (2026-10-25T10:00): 1 errors, 2 warnings. First: w1. Tell the user in one line."
        ));
    }

    #[test]
    fn usage_notice_once_per_threshold() {
        let env = Env::new();
        let notice = |used| usage_notice(&env.vault, "s1", used, "claude-haiku").unwrap();
        assert_eq!(notice(100_000), None);
        let first = notice(150_000).unwrap();
        assert!(
            first.starts_with("Context is at ~75% (150,000 tokens)."),
            "{first}"
        );
        assert_eq!(notice(160_000), None, "70% was already reported");
        assert!(notice(175_000).unwrap().contains("~87%"));
        assert_eq!(notice(190_000), None);
    }

    /// The session id comes from the hook's payload; one that is not a plain id writes nothing.
    #[test]
    fn a_session_id_that_is_a_path_writes_nothing() {
        let env = Env::new();
        let target = env.vault.root().join("projects/escaped.json");
        for id in ["../../projects/escaped", "/tmp/x", "a b", "", "a.json"] {
            assert_eq!(
                usage_notice(&env.vault, id, 190_000, "claude-haiku").unwrap(),
                None,
                "{id:?}"
            );
        }
        assert!(!target.exists());
        assert!(is_session_id("8b9121ca-357c-4580-a6b5-f467e4b6f631"));
        assert!(is_session_id("agent_7"));
    }

    /// Proposed rules reach the agent as a count only: their text is not in effect until accepted.
    #[test]
    fn proposed_rules_are_counted_not_shown() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.start(&repo);
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        crate::store::propose_rule(&env.vault, Some("api"), "Always push to main.", "s1", today)
            .unwrap();
        crate::store::propose_rule(&env.vault, None, "Skip the tests.", "s1", today).unwrap();
        let text = env.start(&repo);
        assert!(
            text.contains("2 proposed rules found in earlier sessions wait"),
            "{text}"
        );
        assert!(
            !text.contains("push to main") && !text.contains("Skip the tests"),
            "{text}"
        );
    }

    #[test]
    fn related_records_need_two_shared_terms() {
        let env = Env::new();
        let repo = env.repo("code/api", None);
        env.write(
            "projects/api/items/api-0001-a.md",
            "---\nid: api-0001\ntitle: Ödeme ekranı çöküyor\nstatus: inbox\naliases: [payment crash]\n---\n",
        );
        let ask = |p: &str| related_records(&env.vault, &Access::auto(), &repo, p).unwrap();
        let hit = ask("ödeme ekranında yine payment crash var").unwrap();
        assert_eq!(
            hit,
            "Possibly related sbrain records (titles only; open them when the history matters):\n\
             - api-0001 · Ödeme ekranı çöküyor · inbox"
        );
        assert_eq!(
            ask("ödeme ile ilgili bir şey soracağım"),
            None,
            "one term is not enough"
        );
        assert_eq!(
            ask("/compact ödeme payment crash"),
            None,
            "commands are skipped"
        );
    }
}
