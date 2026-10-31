//! `reprise status`: everything reprise does on this computer at one glance. The vault and the folders it may
//! read, the agents it is connected to and which reprise their hooks run, the engine, the scheduled sweep
//! with its last and next run, and the job ledger's waiting and given-up work. Reads only; `--json` gives
//! the same as one object. What needs the user's hand is listed last, with the command that fixes it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Local;
use reprise::ledger::{self, Ledger};
use reprise::sweep::{self, DISTILL, ENDED, Schedule, State};
use reprise::{Settings, Vault};
use serde::Serialize;

/// Ledger kinds, in the order they are shown.
const KINDS: [&str; 3] = [DISTILL, ENDED, reprise::git::COMMIT];

#[derive(Debug, Serialize)]
pub struct Status {
    pub version: &'static str,
    /// The reprise answering.
    pub reprise: PathBuf,
    pub settings: PathBuf,
    pub vault: Option<VaultInfo>,
    pub access: Option<reprise::Access>,
    pub engine: Engine,
    pub agents: Vec<Agent>,
    pub schedule: ScheduleInfo,
    pub sweep: Option<SweepInfo>,
    pub ledger: Vec<Kind>,
    /// Errors and warnings of the last doctor run, from its cache.
    pub doctor: Option<DoctorInfo>,
    /// What needs the user: a line each, with the command that fixes it.
    pub attention: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct VaultInfo {
    pub path: PathBuf,
    /// root, inside (another repository), outside or no-git.
    pub git: &'static str,
    pub projects: usize,
}

#[derive(Debug, Serialize)]
pub struct Engine {
    pub client: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Agent {
    pub client: &'static str,
    pub name: &'static str,
    pub installed: bool,
    pub version: Option<String>,
    /// `None` when reprise cannot be connected to it.
    pub connected: Option<bool>,
    /// The reprise its hooks run.
    pub hooks_run: Option<PathBuf>,
    /// That reprise is this one.
    pub hooks_run_this: Option<bool>,
    /// Its transcripts are swept.
    pub swept: bool,
}

#[derive(Debug, Serialize)]
pub struct ScheduleInfo {
    pub installed: bool,
    pub service: Option<String>,
    pub reprise: Option<PathBuf>,
    /// The program the job runs is there.
    pub reprise_exists: Option<bool>,
    pub every_hours: u32,
    pub per_run: usize,
    pub log: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct SweepInfo {
    /// Local time sessions are swept from; `None` before the first run.
    pub since: Option<String>,
    pub last: Option<LastRun>,
    /// Local time of the next scheduled run; `None` when it runs only by hand.
    pub next: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LastRun {
    pub finished: String,
    pub client: String,
    pub distilled: usize,
    pub skipped: usize,
    pub failed: usize,
    pub remaining: usize,
    pub offline: bool,
}

#[derive(Debug, Serialize)]
pub struct Kind {
    pub kind: &'static str,
    pub pending: usize,
    pub gave_up: Vec<GaveUp>,
}

#[derive(Debug, Serialize)]
pub struct GaveUp {
    pub key: String,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DoctorInfo {
    pub at: String,
    pub errors: u64,
    pub warnings: u64,
}

pub fn gather(vault_arg: Option<PathBuf>) -> Result<Status> {
    let reprise = std::env::current_exe()?;
    let settings_path = Settings::default_path()?;
    let settings = Settings::load()?;
    let vault = match vault_arg {
        Some(p) => Vault::open(&p).ok(),
        None => Vault::discover().ok(),
    };
    let engine = settings.engine.clone().unwrap_or(reprise::Engine {
        client: "claude".into(),
        model: None,
        effort: None,
    });
    let now = Local::now().timestamp();
    let mut attention = Vec::new();

    let agents = agents(&reprise, &engine.client)?;
    for a in &agents {
        match (&a.hooks_run, a.hooks_run_this) {
            (Some(p), _) if !p.exists() => attention.push(format!(
                "{}'s hooks run {}, which is gone: run `reprise connect {} --yes`",
                a.name,
                p.display(),
                a.client
            )),
            (Some(p), Some(false)) => attention.push(format!(
                "{}'s hooks run another reprise ({}): `reprise connect {} --yes` points them at this one",
                a.name,
                p.display(),
                a.client
            )),
            _ => {}
        }
    }
    if !agents.iter().any(|a| a.connected == Some(true)) {
        attention.push(
            "no agent is connected, so sessions get no memory: run `reprise connect claude`".into(),
        );
    }

    let installed = Schedule::load().ok().flatten();
    let sweep_settings = settings.sweep.normalized();
    let schedule = ScheduleInfo {
        installed: installed.is_some(),
        service: installed.as_ref().map(|s| s.service.clone()),
        reprise: installed.as_ref().map(|s| s.reprise.clone()),
        reprise_exists: installed.as_ref().map(|s| s.reprise.exists()),
        every_hours: sweep_settings.every_hours,
        per_run: sweep_settings.per_run,
        log: installed.as_ref().and_then(|s| s.log.clone()),
    };
    if schedule.reprise_exists == Some(false) {
        attention.push(
            "the scheduled sweep runs a reprise that is gone: run `reprise sweep schedule`".into(),
        );
    }

    let (mut sweep_info, mut kinds, mut doctor) = (None, Vec::new(), None);
    let mut vault_info = None;
    match &vault {
        Some(v) => {
            vault_info = Some(VaultInfo {
                path: v.root().to_path_buf(),
                git: match reprise::git::place(v.root()) {
                    reprise::git::Place::Root => "root",
                    reprise::git::Place::Inside(_) => "inside",
                    reprise::git::Place::Outside => "outside",
                    reprise::git::Place::NoGit => "no-git",
                },
                projects: count_projects(v),
            });
            let state = State::load(v);
            let led = Ledger::open(v).ok();
            let waiting = led.as_ref().and_then(|l| sweep::waiting(l).ok()).flatten();
            let last = state.last_run.as_ref().map(|r| LastRun {
                finished: local(r.finished),
                client: r.client.clone(),
                distilled: count(r, "ok"),
                skipped: count(r, "skipped"),
                failed: count(r, "failed"),
                remaining: r.remaining,
                offline: r.offline,
            });
            sweep_info = Some(SweepInfo {
                since: state.baseline.map(local),
                last,
                next: state
                    .next_run(sweep_settings.every_hours, now, waiting)
                    .map(local),
            });
            if schedule.installed && sweep_settings.every_hours > 0 {
                let late = i64::from(sweep_settings.every_hours) * 3600 * 2;
                let last_at = state
                    .last_run
                    .as_ref()
                    .map(|r| r.finished)
                    .or(state.baseline);
                if last_at.is_some_and(|t| now - t > late) {
                    attention.push(
                        "the scheduled sweep has not run for twice its interval: see `reprise doctor`"
                            .into(),
                    );
                }
            }
            if let Some(led) = &led {
                for kind in KINDS {
                    let pending = led.list(kind, ledger::State::Pending)?.len();
                    let gave_up: Vec<GaveUp> = led
                        .list(kind, ledger::State::GaveUp)?
                        .into_iter()
                        .map(|j| GaveUp {
                            key: j.key,
                            attempts: j.attempts,
                            last_error: j.last_error,
                        })
                        .collect();
                    if !gave_up.is_empty() {
                        attention.push(match kind {
                            DISTILL => format!(
                                "{} session(s) the sweep gave up on: `reprise sweep retry`",
                                gave_up.len()
                            ),
                            _ => format!(
                                "{} {kind} job(s) given up: see `reprise doctor`",
                                gave_up.len()
                            ),
                        });
                    }
                    kinds.push(Kind {
                        kind,
                        pending,
                        gave_up,
                    });
                }
            }
            doctor = doctor_cache(v);
            if let Some(d) = &doctor
                && d.errors > 0
            {
                attention.push(format!(
                    "the doctor found {} error(s) ({}): run `reprise doctor`",
                    d.errors, d.at
                ));
            }
            let proposed = reprise::store::proposals(v).map_or(0, |p| p.len());
            if proposed > 0 {
                attention.push(format!(
                    "{proposed} rule(s) the sweep found wait for you: `reprise rule list`"
                ));
            }
        }
        None => attention.push("no vault is chosen: run `reprise setup set`".into()),
    }

    Ok(Status {
        version: env!("CARGO_PKG_VERSION"),
        reprise,
        settings: settings_path,
        vault: vault_info,
        access: settings.access.clone(),
        engine: Engine {
            client: engine.client,
            model: engine.model,
            effort: engine.effort,
        },
        agents,
        schedule,
        sweep: sweep_info,
        ledger: kinds,
        doctor,
        attention,
    })
}

fn agents(this: &Path, engine: &str) -> Result<Vec<Agent>> {
    let this = this.canonicalize().unwrap_or_else(|_| this.to_path_buf());
    let mut out = Vec::new();
    for c in reprise::clients::all() {
        let d = c.detect();
        let (connected, hooks_run) = match c.connector() {
            Some(k) => (Some(k.is_connected()?), k.hooks_run()?),
            None => (None, None),
        };
        let hooks_run_this = hooks_run
            .as_ref()
            .map(|p| p.canonicalize().is_ok_and(|p| p == this));
        let swept = c.sweeper().is_some()
            && (c.id().as_str() == engine
                || connected == Some(true)
                || (connected.is_none() && d.installed()));
        out.push(Agent {
            client: c.id().as_str(),
            name: c.display_name(),
            installed: d.installed(),
            version: d.version,
            connected,
            hooks_run,
            hooks_run_this,
            swept,
        });
    }
    Ok(out)
}

fn count(run: &sweep::Run, status: &str) -> usize {
    run.sessions.iter().filter(|s| s.status == status).count()
}

fn count_projects(vault: &Vault) -> usize {
    std::fs::read_dir(vault.projects_dir()).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .count()
    })
}

fn doctor_cache(vault: &Vault) -> Option<DoctorInfo> {
    let text = reprise::safe::read_to_string(vault.cache_dir().join("doctor.json")).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let n = |k: &str| doc["counts"][k].as_u64().unwrap_or(0);
    Some(DoctorInfo {
        at: doc["at"].as_str().unwrap_or("?").to_owned(),
        errors: n("error"),
        warnings: n("warning"),
    })
}

/// `YYYY-MM-DD HH:MM`, local time.
fn local(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub fn print(s: &Status) {
    let yes = |b: bool| if b { "yes" } else { "no" };
    println!("reprise {} · {}", s.version, s.reprise.display());
    println!("settings  {}", s.settings.display());
    match &s.vault {
        Some(v) => println!(
            "vault     {} · {} project(s) · git: {}",
            v.path.display(),
            v.projects,
            v.git
        ),
        None => println!("vault     not chosen"),
    }
    match &s.access {
        Some(a) if a.mode == reprise::AccessMode::Auto => println!("access    every folder"),
        Some(a) => println!(
            "access    {}",
            a.folders
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => println!("access    not set up"),
    }
    let mut engine = s.engine.client.clone();
    for part in [&s.engine.model, &s.engine.effort].into_iter().flatten() {
        engine.push_str(&format!(" · {part}"));
    }
    println!("engine    {engine}");

    println!("\nAgents");
    for a in &s.agents {
        let state = match (a.installed, a.connected) {
            (false, _) => "not installed".to_owned(),
            (true, None) => "cannot be connected".to_owned(),
            (true, Some(c)) => format!("connected: {}", yes(c)),
        };
        // `2.1.30 (Claude Code)`: the number is enough beside the name.
        let version = a
            .version
            .as_deref()
            .and_then(|v| v.split_whitespace().next())
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let swept = if a.swept { " · swept" } else { "" };
        println!("  {}{version}: {state}{swept}", a.name);
        if let Some(p) = &a.hooks_run {
            let which = match a.hooks_run_this {
                Some(true) => "this reprise",
                _ if !p.exists() => "gone",
                _ => "another reprise",
            };
            println!("    hooks run {} ({which})", p.display());
        }
    }

    println!("\nSweep");
    match (s.schedule.installed, s.schedule.every_hours) {
        (true, 0) => println!(
            "  scheduled ({}), but set to run only by hand",
            s.schedule.service.as_deref().unwrap_or("?")
        ),
        (true, h) => println!(
            "  scheduled ({}) every {h} hour(s), {} session(s) a run",
            s.schedule.service.as_deref().unwrap_or("?"),
            s.schedule.per_run
        ),
        (false, _) => println!("  not scheduled; runs by hand (`reprise sweep schedule`)"),
    }
    if let Some(sw) = &s.sweep {
        match &sw.since {
            Some(since) => println!("  sweeping sessions since {since}"),
            None => println!("  never ran: the first run marks where to start"),
        }
        if let Some(l) = &sw.last {
            println!(
                "  last run {} · {} · {} distilled, {} skipped, {} failed{}{}",
                l.finished,
                l.client,
                l.distilled,
                l.skipped,
                l.failed,
                if l.remaining > 0 {
                    format!(", {} left", l.remaining)
                } else {
                    String::new()
                },
                if l.offline { ", stopped offline" } else { "" }
            );
        }
        if s.schedule.installed
            && let Some(next) = &sw.next
        {
            println!("  next run {next}");
        }
    }
    let busy: Vec<String> = s
        .ledger
        .iter()
        .filter(|k| k.pending > 0 || !k.gave_up.is_empty())
        .map(|k| {
            let name = match k.kind {
                DISTILL => "distilling",
                ENDED => "ended sessions",
                _ => "commits",
            };
            format!(
                "{name}: {} waiting, {} given up",
                k.pending,
                k.gave_up.len()
            )
        })
        .collect();
    if !s.ledger.is_empty() {
        if busy.is_empty() {
            println!("  ledger: nothing waiting");
        } else {
            println!("  ledger: {}", busy.join(" · "));
        }
    }
    if let Some(d) = &s.doctor {
        println!(
            "\nDoctor ({}): {} error(s), {} warning(s)",
            d.at, d.errors, d.warnings
        );
    }
    if s.attention.is_empty() {
        println!("\nNothing needs you.");
    } else {
        println!("\nNeeds you");
        for line in &s.attention {
            println!("  - {line}");
        }
    }
}
