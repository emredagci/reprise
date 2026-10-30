//! `sbrain sweep`: distills sessions that ended without a handoff card into records. Picks transcripts that
//! have settled and grown since they were last handled, skips the ones that need no distilling, has the
//! engine CLI answer with JSON, and writes what passes the checks.
//!
//! Transcripts come from every connected agent that keeps them (one sbrain cannot be connected to,
//! like Codex, when it is installed), and from the engine; the engine distills them all.
//! The agent you code with and the one that distills may differ.
//!
//! Every step is reported as it happens: one JSON object per line with `--json`, a short line otherwise.
//! `--due` runs only when the schedule says so; the job `sbrain sweep schedule` installs runs that.

use std::io::IsTerminal as _;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use sbrain_clients::{Client, ClientId, DistillRun, Sweeper, Transcript};
use sbrain_core::config::SweepConfig;
use sbrain_core::distill::{self, Distilled};
use sbrain_core::ledger::{self, Ledger};
use sbrain_core::project::Resolver;
use sbrain_core::record::Record;
use sbrain_core::store::{self, CardSource};
use sbrain_core::sweep::{self, DISTILL, ENDED, State};
use sbrain_core::{Access, Settings, Vault};
use serde::Serialize;

/// The client whose transcripts are swept when no engine is chosen.
const DEFAULT_CLIENT: &str = "claude";

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Runs only when the schedule says a run is due.
    pub due: bool,
    /// Says what would be distilled; writes nothing and calls no model.
    pub dry_run: bool,
    /// Only this session (its id).
    pub only: Option<String>,
    /// Overrides `[sweep] max_per_run`.
    pub max: Option<usize>,
    /// Overrides `[sweep] settle_minutes`.
    pub settle: Option<u64>,
    pub json: bool,
}

/// One step of a run.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
enum Event {
    /// Another run holds the lock.
    Busy,
    /// `--due`, and no run is due yet.
    NotDue {
        /// Local time of the next run, `YYYY-MM-DD HH:MM`; `null` when it runs only by hand.
        next: Option<String>,
    },
    /// First run: only sessions written from now on are distilled.
    BaselineSet {
        at: String,
    },
    /// First run, dry: a real run would start from now; nothing was written.
    WouldSetBaseline {
        at: String,
    },
    Start {
        client: String,
        model: Option<String>,
        /// The agents whose transcripts were read.
        sources: Vec<String>,
        due: usize,
    },
    Session(Outcome),
    Done {
        distilled: usize,
        /// Dry run: sessions that would have been distilled.
        would_distill: usize,
        skipped: usize,
        failed: usize,
        /// Due sessions left for a later run.
        remaining: usize,
        offline: bool,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
struct Outcome {
    session: String,
    /// ok, skipped, failed, offline or would-distill.
    status: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied: Option<distill::Applied>,
    /// The vault commit, when the vault is a git repository and the commit worked.
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
}

impl Outcome {
    fn new(session: &str, status: &'static str, project: &str, reason: impl Into<String>) -> Self {
        Self {
            session: session.to_owned(),
            status,
            project: project.to_owned(),
            reason: reason.into(),
            ..Self::default()
        }
    }
}

struct Reporter {
    json: bool,
}

impl Reporter {
    fn emit(&self, event: &Event) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string(event).expect("events serialize")
            );
            return;
        }
        match event {
            Event::Busy => println!("Another sweep is running; nothing done."),
            Event::NotDue { next } => {
                // The scheduled job asks every few minutes; its log keeps only runs.
                if std::io::stdout().is_terminal() {
                    match next {
                        Some(next) => println!("No sweep is due; the next one is at {next}."),
                        None => println!("The sweep runs only by hand; nothing is due."),
                    }
                }
            }
            Event::BaselineSet { at } => println!(
                "First run: sessions written after {at} will be distilled; earlier ones are left alone."
            ),
            Event::WouldSetBaseline { at } => println!(
                "First run (dry run): a real run would mark {at} as the start and distill only sessions written after it. Nothing was written."
            ),
            Event::Start {
                client,
                model,
                sources,
                due,
            } => println!(
                "{due} session(s) due from {} · distilled by {client}{}",
                sources.join(", "),
                model
                    .as_deref()
                    .map(|m| format!(" · {m}"))
                    .unwrap_or_default()
            ),
            Event::Session(o) => {
                let short = sbrain_core::text::take_chars(&o.session, 8);
                let project = if o.project.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", o.project)
                };
                let detail = match (&o.applied, o.reason.is_empty()) {
                    (Some(a), _) => format!(
                        " → {} (+{} items, {} updated, +{} decisions{})",
                        a.card.display(),
                        a.items_added.len(),
                        a.items_updated.len(),
                        a.decisions_added.len(),
                        if a.left_out.is_empty() {
                            String::new()
                        } else {
                            format!(", {} left out without a quote", a.left_out.len())
                        }
                    ),
                    (None, false) => format!(": {}", o.reason),
                    (None, true) => String::new(),
                };
                println!("{} {short}{project}{detail}", o.status);
            }
            Event::Done {
                distilled,
                would_distill,
                skipped,
                failed,
                remaining,
                offline,
            } => {
                let done = if *would_distill > 0 {
                    format!("{would_distill} would be distilled (dry run)")
                } else {
                    format!("{distilled} distilled")
                };
                println!(
                    "Done: {done}, {skipped} skipped, {failed} failed{}{}.",
                    if *remaining > 0 {
                        format!(", {remaining} left for the next run")
                    } else {
                        String::new()
                    },
                    if *offline { ", stopped: offline" } else { "" }
                );
            }
        }
    }
}

pub fn run(vault: &Vault, opts: &Options) -> Result<()> {
    let out = Reporter { json: opts.json };
    // Settings that cannot be read stop the sweep: falling back would mean every folder is allowed.
    let settings = Settings::load()?;
    let schedule = settings.sweep.normalized();
    let ledger = Ledger::open(vault)?;
    let before = State::load(vault);
    if !opts.dry_run {
        // Vault commits that failed before are made now, when git lets them.
        sbrain_core::git::retry_commits(vault.root(), Local::now().timestamp())?;
    }
    if opts.due {
        let next = before.next_run(
            schedule.every_hours,
            Local::now().timestamp(),
            sweep::waiting(&ledger)?,
        );
        if next.is_none_or(|next| next > Local::now().timestamp()) {
            out.emit(&Event::NotDue {
                next: next.map(local_time),
            });
            return Ok(());
        }
    }
    let Some(_lock) = sweep::lock(vault)? else {
        out.emit(&Event::Busy);
        return Ok(());
    };
    let now = Local::now();
    let mut state = State::load(vault);
    if state.baseline.is_none() {
        let at = now.format("%Y-%m-%d %H:%M").to_string();
        if opts.dry_run {
            out.emit(&Event::WouldSetBaseline { at });
            return Ok(());
        }
        state.baseline = Some(now.timestamp());
        state.save(vault)?;
        out.emit(&Event::BaselineSet { at });
        return Ok(());
    }

    let cfg = vault.config().sweep;
    let access = settings
        .access()
        .cloned()
        .context("accessible folders are not set up; run `sbrain setup set`")?;
    let engine = settings.engine.clone();
    let client_id = engine
        .as_ref()
        .map_or(DEFAULT_CLIENT, |h| h.client.as_str());
    let client =
        sbrain_clients::get(client_id).with_context(|| format!("unknown engine {client_id}"))?;
    let sweeper = client
        .sweeper()
        .with_context(|| format!("{} cannot distill sessions", client.display_name()))?;
    let model = engine.as_ref().and_then(|h| h.model.clone());
    let effort = engine.as_ref().and_then(|h| h.effort.clone());

    let distill_dir = sweep::distill_dir()?;
    std::fs::create_dir_all(&distill_dir)?;
    let settle = i64::try_from(opts.settle.unwrap_or(cfg.settle_minutes) * 60).unwrap_or(i64::MAX);
    // An agent sbrain cannot be connected to counts as connected when it is installed.
    let sources = sources(sbrain_clients::all(), client.id(), |c| {
        match c.connector() {
            Some(conn) => conn.is_connected().unwrap_or(false),
            None => c.detect().installed(),
        }
    });
    let mut transcripts: Vec<(Transcript, &dyn Sweeper)> = Vec::new();
    for source in &sources {
        let reader = source.sweeper().expect("sources can be swept");
        for t in reader.transcripts(&distill_dir)? {
            transcripts.push((t, reader));
        }
    }
    transcripts.sort_by_key(|(t, _)| t.modified);
    if !opts.dry_run {
        // A retry whose transcript is gone has nothing left to distill.
        for job in ledger.due(DISTILL, now.timestamp())? {
            if !transcripts.iter().any(|(t, _)| t.session == job.key) {
                ledger.remove(DISTILL, &job.key)?;
            }
        }
    }
    let mut due = Vec::new();
    for (t, reader) in transcripts {
        if opts.only.as_ref().is_some_and(|only| &t.session != only) {
            continue;
        }
        let force = opts.only.is_some();
        if sweep::is_due(
            &state,
            &ledger,
            &t.session,
            t.size,
            t.modified,
            now.timestamp(),
            settle,
            force,
        )? {
            due.push((t, reader));
        }
    }
    out.emit(&Event::Start {
        client: client.display_name().to_owned(),
        model: model.clone(),
        sources: sources
            .iter()
            .map(|c| c.display_name().to_owned())
            .collect(),
        due: due.len(),
    });

    let max = opts.max.unwrap_or(if opts.due {
        schedule.per_run
    } else {
        cfg.max_per_run
    });
    let (mut distilled, mut would_distill, mut skipped, mut failed, mut offline) =
        (0, 0, 0, 0, false);
    let mut online: Option<bool> = None;
    let mut wrote = false;
    let mut handled = 0;
    let mut record = Vec::new();
    for (t, reader) in &due {
        if distilled + would_distill + failed >= max {
            break;
        }
        let ctx = Run {
            vault,
            cfg: &cfg,
            access: &access,
            reader: *reader,
            sweeper,
            model: model.as_deref(),
            effort: effort.as_deref(),
            distill_dir: &distill_dir,
        };
        let mut outcome = ctx.one(&t.path, &t.session, opts.dry_run, &mut online);
        match outcome.status {
            "ok" => distilled += 1,
            "would-distill" => would_distill += 1,
            "failed" => failed += 1,
            "offline" => offline = true,
            _ => skipped += 1,
        }
        // Offline and dry runs leave no trace, so the session is due again next time. A failure is
        // tried again after a wait that grows each time, and given up after `ledger::MAX_ATTEMPTS`.
        if !opts.dry_run && outcome.status != "offline" {
            let at = Local::now();
            let failed_now = outcome.status == "failed";
            // The size is kept after a failure too: the session is tried again early only if it grew.
            let payload = sweep::distilled(
                Some(t.size),
                outcome.status,
                &outcome.reason,
                &outcome.project,
                &at.format("%Y-%m-%d %H:%M").to_string(),
            );
            if failed_now {
                let state = ledger.fail(
                    DISTILL,
                    &t.session,
                    &payload,
                    &outcome.reason,
                    at.timestamp(),
                )?;
                if state == ledger::State::GaveUp {
                    outcome.reason = format!(
                        "{}; given up after {} tries, `sbrain sweep retry {}` tries again",
                        outcome.reason,
                        ledger::MAX_ATTEMPTS,
                        t.session
                    );
                }
            } else {
                ledger.succeed(DISTILL, &t.session, &payload, at.timestamp())?;
            }
            ledger.remove(ENDED, &t.session)?;
        }
        wrote |= outcome.status == "ok";
        if outcome.status != "offline" {
            handled += 1;
            record.push(run_session(&outcome));
        }
        out.emit(&Event::Session(outcome));
        if offline {
            break;
        }
    }
    let remaining = due.len() - handled;
    // Ended sessions that are not due at all need no more haste; ones left for the next run keep their
    // mark. A mark that came in during this run is left for the next.
    if !opts.dry_run {
        let waiting: Vec<&str> = due
            .iter()
            .skip(handled)
            .map(|(t, _)| t.session.as_str())
            .collect();
        for job in ledger.list(ENDED, ledger::State::Pending)? {
            if job.next_at <= now.timestamp() && !waiting.contains(&job.key.as_str()) {
                ledger.remove(ENDED, &job.key)?;
            }
        }
    }
    if !opts.dry_run {
        state.last_run = Some(sweep::Run {
            started: now.timestamp(),
            finished: Local::now().timestamp(),
            client: client.display_name().to_owned(),
            model: model.clone(),
            sessions: record,
            remaining,
            offline,
        });
        state.save(vault)?;
    }
    // The session-start line reads the doctor's cache; refresh it after writing records.
    if wrote {
        let _ = sbrain_core::doctor::doctor(vault, &access, Local::now().naive_local(), false);
    }
    out.emit(&Event::Done {
        distilled,
        would_distill,
        skipped,
        failed,
        remaining,
        offline,
    });
    Ok(())
}

/// The clients whose transcripts are read: each connected one that can be swept, and the engine
/// (whose sessions are worth distilling even when nothing is connected), in `all()` order.
fn sources(
    clients: Vec<Box<dyn Client>>,
    engine: ClientId,
    connected: impl Fn(&dyn Client) -> bool,
) -> Vec<Box<dyn Client>> {
    clients
        .into_iter()
        .filter(|c| c.sweeper().is_some() && (c.id() == engine || connected(c.as_ref())))
        .collect()
}

/// `YYYY-MM-DD HH:MM`, local time.
fn local_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// What the run record keeps of an outcome.
fn run_session(o: &Outcome) -> sweep::RunSession {
    let applied = o.applied.as_ref();
    sweep::RunSession {
        session: o.session.clone(),
        status: o.status.to_owned(),
        project: o.project.clone(),
        reason: o.reason.clone(),
        card: applied.map(|a| a.card.to_string_lossy().into_owned()),
        items_added: applied.map_or(0, |a| a.items_added.len()),
        items_updated: applied.map_or(0, |a| a.items_updated.len()),
        decisions_added: applied.map_or(0, |a| a.decisions_added.len()),
        rules_proposed: applied.map_or(0, |a| a.rules_proposed),
        state_filled: applied.is_some_and(|a| a.state_filled),
        left_out: applied.map_or(0, |a| a.left_out.len()),
    }
}

struct Run<'a> {
    vault: &'a Vault,
    cfg: &'a SweepConfig,
    access: &'a Access,
    /// The agent that kept the transcript; it reads it.
    reader: &'a dyn Sweeper,
    /// The engine; it distills.
    sweeper: &'a dyn Sweeper,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    distill_dir: &'a Path,
}

impl Run<'_> {
    fn one(&self, path: &Path, session: &str, dry_run: bool, online: &mut Option<bool>) -> Outcome {
        let digest = match self.reader.digest(path) {
            Ok(d) => d,
            Err(e) => return Outcome::new(session, "failed", "", e.to_string()),
        };
        if digest.user_turns < self.cfg.min_user_turns {
            return Outcome::new(
                session,
                "skipped",
                "",
                format!("short ({} messages)", digest.user_turns),
            );
        }
        let Some(cwd) = digest.cwd.clone() else {
            return Outcome::new(session, "skipped", "", "no working directory");
        };
        let with_slash = format!("{}/", cwd.trim_end_matches('/'));
        if self
            .cfg
            .skip_cwd_prefixes
            .iter()
            .any(|p| with_slash.starts_with(p.as_str()))
        {
            return Outcome::new(session, "skipped", "", "temporary folder");
        }
        let resolution = Resolver::new(self.vault, self.access).resolve(Path::new(&cwd));
        let Some(project) = resolution.project.clone() else {
            return Outcome::new(session, "skipped", "", format!("not a project ({cwd})"));
        };
        if let Some(card) = store::find_card(self.vault, &project, session)
            && let Ok(r) = Record::read(&card)
            && r.text("source") == CardSource::Handoff.as_str()
            && !digest.last_user.is_empty()
            && r.text("date") >= digest.last_user.as_str()
        {
            return Outcome::new(session, "skipped", &project, "covered by a handoff card");
        }
        if !*online.get_or_insert_with(|| is_online(self.cfg)) {
            return Outcome::new(session, "offline", &project, "no connection");
        }
        if dry_run {
            return Outcome::new(
                session,
                "would-distill",
                &project,
                format!(
                    "{} messages, {} characters",
                    digest.user_turns,
                    digest.text.chars().count()
                ),
            );
        }

        let today = Local::now().date_naive();
        // A project seen for the first time gets its folder; its files go into the same commit.
        let created = match sbrain_core::scaffold::ensure_project(
            self.vault,
            &project,
            resolution.remote.as_deref(),
            today,
        ) {
            Ok(created) => created,
            Err(e) => return Outcome::new(session, "failed", &project, e.to_string()),
        };
        let user_text = digest
            .user_messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let s = distill::Session {
            id: session,
            project: &project,
            cwd: &cwd,
            first: &digest.first,
            last: if digest.last.is_empty() {
                &digest.first
            } else {
                &digest.last
            },
            digest: &digest.text,
            user_text: &user_text,
        };
        let input = distill::input(self.vault, &s);
        let run = DistillRun {
            system: distill::SYSTEM_PROMPT,
            input: &input,
            schema: distill::SCHEMA,
            model: self.model,
            effort: self.effort,
            cwd: self.distill_dir,
            timeout: Duration::from_secs(self.cfg.timeout_seconds),
        };
        let applied = self
            .sweeper
            .distill(&run)
            .map_err(|e| e.to_string())
            .and_then(|answer| Distilled::from_json(answer).map_err(|e| e.to_string()))
            .and_then(|d| distill::apply(self.vault, &s, &d, today).map_err(|e| e.to_string()));
        match applied {
            Ok(applied) => {
                let short = sbrain_core::text::take_chars(session, 8);
                let mut files = applied.files.clone();
                if created {
                    for file in ["project.toml", "state.md", "gotchas.md"] {
                        let path = PathBuf::from("projects").join(&project).join(file);
                        if !files.contains(&path) {
                            files.push(path);
                        }
                    }
                }
                let commit = sbrain_core::git::commit(
                    self.vault.root(),
                    &files,
                    &format!("sweep({project}): {short} distilled from the transcript"),
                );
                let mut o = Outcome::new(session, "ok", &project, "");
                o.applied = Some(applied);
                o.commit = commit;
                o
            }
            Err(reason) => Outcome::new(session, "failed", &project, reason),
        }
    }
}

/// Whether `online_check` (`host:port`) answers; an empty setting skips the check.
fn is_online(cfg: &SweepConfig) -> bool {
    if cfg.online_check.trim().is_empty() {
        return true;
    }
    let timeout = Duration::from_secs(cfg.online_timeout_seconds.max(1));
    cfg.online_check
        .to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .any(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sbrain_clients::{Detection, Digest};

    /// A client that can be swept when `sweeps`.
    struct Fake {
        id: ClientId,
        sweeps: bool,
    }

    impl Client for Fake {
        fn id(&self) -> ClientId {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.id.as_str()
        }
        fn detect(&self) -> Detection {
            Detection {
                client: self.id,
                binary: None,
                version: None,
            }
        }
        fn sweeper(&self) -> Option<&dyn Sweeper> {
            self.sweeps.then_some(self as &dyn Sweeper)
        }
    }

    impl Sweeper for Fake {
        fn transcripts(&self, _: &Path) -> sbrain_clients::Result<Vec<Transcript>> {
            Ok(Vec::new())
        }
        fn digest(&self, _: &Path) -> sbrain_clients::Result<Digest> {
            Ok(Digest::default())
        }
        fn distill(&self, _: &DistillRun) -> sbrain_clients::Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }
    }

    fn ids(sweeps: [bool; 2], engine: ClientId, connected: &[ClientId]) -> Vec<&'static str> {
        let clients: Vec<Box<dyn Client>> = vec![
            Box::new(Fake {
                id: ClientId::Claude,
                sweeps: sweeps[0],
            }),
            Box::new(Fake {
                id: ClientId::Codex,
                sweeps: sweeps[1],
            }),
        ];
        sources(clients, engine, |c| connected.contains(&c.id()))
            .iter()
            .map(|c| c.id().as_str())
            .collect()
    }

    #[test]
    fn every_connected_agent_and_the_engine_are_read() {
        use ClientId::{Claude, Codex};
        assert_eq!(ids([true, true], Claude, &[]), ["claude"]);
        assert_eq!(ids([true, true], Claude, &[Codex]), ["claude", "codex"]);
        assert_eq!(
            ids([true, true], Codex, &[Claude]),
            ["claude", "codex"],
            "coding in Claude Code with Codex as the engine still sweeps Claude Code"
        );
        assert_eq!(
            ids([true, false], Claude, &[Codex]),
            ["claude"],
            "an agent whose transcripts cannot be read is left out"
        );
    }
}
