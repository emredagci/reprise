mod schedule;
mod status;
mod sweep;
mod uninstall;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use sbrain_clients::Refresh;
use sbrain_core::project::{Resolver, Rule};
use sbrain_core::settings::SweepSettings;
use sbrain_core::sweep::Schedule;
use sbrain_core::{Access, AccessMode, Settings, Vault};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "sbrain",
    version,
    about = "A second brain for your coding agent"
)]
struct Cli {
    /// Vault root. Defaults to SBRAIN_VAULT, then the saved choice (`sbrain setup set`).
    #[arg(long, global = true)]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The project a working directory belongs to.
    Project {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Everything sbrain does on this computer at one glance: vault, agents and the sbrain their hooks
    /// run, engine, scheduled sweep, waiting and given-up work, and what needs you. Changes nothing.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Removes what sbrain set up on this computer, except the vault: connections, the scheduled sweep,
    /// its settings and caches. Lists what it would do; does it only with `--yes`. Then says how to remove
    /// the program itself.
    Uninstall {
        #[arg(long)]
        yes: bool,
    },
    /// Coding agents on this machine and whether sbrain is connected to them.
    Clients,
    /// Connects sbrain to a coding agent (hooks, permission, `sbrain` on PATH).
    /// Shows the changes; applies them only with `--yes`.
    Connect {
        client: String,
        #[arg(long)]
        yes: bool,
    },
    /// Removes exactly what `connect` added.
    Disconnect { client: String },
    /// Claude Code hook: reads the payload on stdin, prints `additionalContext`. Never fails the
    /// session: errors go to `.sbrain/cache/hook-errors.log` and nothing is printed.
    Hook {
        #[arg(value_enum)]
        event: HookEvent,
    },
    /// Per-project summary of the vault: counts, statuses, open items, latest decisions.
    Overview,
    /// Every record and how they link, as JSON.
    Graph,
    /// Writes or changes items.
    Item {
        #[command(subcommand)]
        action: ItemAction,
    },
    /// Every item with its sections, newest first, as JSON.
    Items,
    /// Checks the vault: broken records, unverified or forgotten work, stale items, broken links,
    /// context files that get cut, git backup, hook failures. Changes nothing.
    Doctor {
        /// Skip the staleness check, which runs git for each verified item.
        #[arg(long)]
        quick: bool,
        #[arg(long)]
        json: bool,
    },
    /// Distills sessions that ended without a handoff card into records: items, decisions, rules and
    /// the session's card, through the engine CLI. The first run only marks where to start.
    #[command(args_conflicts_with_subcommands = true)]
    Sweep {
        #[command(subcommand)]
        action: Option<SweepAction>,
        /// Run only when the schedule says a run is due (`sweep.every_hours` in the settings); the
        /// scheduled job runs this. Distills `sweep.per_run` sessions unless --max says otherwise.
        #[arg(long)]
        due: bool,
        /// Say what would be distilled; write nothing and call no model.
        #[arg(long)]
        dry_run: bool,
        /// Only this session id.
        #[arg(long)]
        only: Option<String>,
        /// Most sessions to distill (default: `[sweep] max_per_run`).
        #[arg(long)]
        max: Option<usize>,
        /// Minutes a transcript must be left unwritten (default: `[sweep] settle_minutes`).
        #[arg(long)]
        settle: Option<u64>,
        /// One JSON object per step.
        #[arg(long)]
        json: bool,
    },
    /// Writes or changes decisions.
    Decision {
        #[command(subcommand)]
        action: DecisionAction,
    },
    /// Every decision with its sections, newest first, as JSON.
    Decisions,
    /// Writes a session's handoff card.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Every handoff card with its sections, newest first, as JSON.
    Sessions,
    /// Imports a format 1 vault into this one. Shows what would be written; writes only
    /// with `--yes`. The source is never changed and existing files are never overwritten.
    Import {
        source: PathBuf,
        #[arg(long)]
        yes: bool,
        /// Also list every file.
        #[arg(long)]
        list: bool,
    },
    /// Shows the vault and accessible folders; `set` chooses both at once.
    Setup {
        #[command(subcommand)]
        action: Option<SetupAction>,
    },
    /// Shows or changes the accessible folders.
    Access {
        #[command(subcommand)]
        action: Option<AccessAction>,
    },
    /// Shows the coding CLI, model and effort that run sbrain's own work (the sweep), and the choices;
    /// `set` changes them.
    Engine {
        #[command(subcommand)]
        action: Option<EngineAction>,
    },
    /// Rules the sweep found in sessions wait for the user here; they are in effect only once accepted.
    Rule {
        #[command(subcommand)]
        action: RuleAction,
    },
}

#[derive(Subcommand)]
enum RuleAction {
    /// Every rule waiting for the user, as JSON.
    List,
    /// Puts a proposed rule in effect: into the project's gotchas.md or profile/rules.md.
    Accept { id: String },
    /// Drops a proposed rule.
    Reject { id: String },
}

#[derive(Subcommand)]
enum EngineAction {
    /// Chooses the coding CLI, and optionally its model and effort; an empty value means the CLI's own
    /// default.
    Set {
        /// A client from `sbrain clients` that can distill sessions, e.g. `claude`.
        client: String,
        /// A model alias the CLI documents, or a full model name.
        #[arg(long)]
        model: Option<String>,
        /// One of the effort levels the installed CLI accepts.
        #[arg(long)]
        effort: Option<String>,
    },
}

#[derive(Subcommand)]
enum SweepAction {
    /// Has this computer run the sweep by itself (launchd on macOS, a systemd user timer on Linux):
    /// it checks every 15 minutes and runs when due. Running it again replaces the job.
    Schedule {
        /// Hours between runs: 1, 3, 6, 12 or 24 (default: the current setting).
        #[arg(long)]
        every: Option<u32>,
        /// Sessions a run distills: 2, 4 or 8 (default: the current setting); more waiting ones follow
        /// a few minutes later.
        #[arg(long)]
        per_run: Option<usize>,
    },
    /// Removes the job `schedule` installed.
    Unschedule,
    /// Tries again a session the sweep gave up on (each is given up after 8 failures), or all of them.
    Retry {
        /// The session id; without it, every session given up on.
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum ItemAction {
    /// Writes a new item with the next free id of the project.
    Add {
        /// Defaults to the project of the working directory.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        title: String,
        #[arg(long = "type", default_value = "feature")]
        kind: String,
        #[arg(long, default_value = "inbox")]
        status: String,
        /// Goes under `## Problem`.
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, default_value = "")]
        captured_during: String,
        /// Comma separated.
        #[arg(long, default_value = "")]
        aliases: String,
        /// Comma separated.
        #[arg(long, default_value = "")]
        files: String,
        /// Comma separated.
        #[arg(long, default_value = "")]
        related: String,
    },
    /// Changes fields: `key=value`, or `key+=a, b` for aliases, files and related. `verified` is
    /// only set by a passing verify.
    Set {
        id: String,
        #[arg(required = true)]
        changes: Vec<String>,
    },
    /// Runs the item's verify command in its project's repository: a pass sets `verified` and
    /// `verified_at`, a failure takes `verified` back to `done-unverified`. Prints the result as JSON;
    /// exits with 1 when the command did not pass.
    Verify {
        id: String,
        /// The folder to run in (default: the project's repository on this machine).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Seconds the command may run.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum DecisionAction {
    /// Writes a new decision with the next free id of the project.
    Add {
        /// Defaults to the project of the working directory.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        title: String,
        /// What was decided; goes under `## Decision`.
        #[arg(long)]
        decision: String,
        #[arg(long, default_value = "")]
        context: String,
        #[arg(long, default_value = "")]
        why: String,
        #[arg(long, default_value = "")]
        alternatives: String,
        /// Comma separated.
        #[arg(long, default_value = "")]
        files: String,
        /// Comma separated.
        #[arg(long, default_value = "")]
        related: String,
        /// The repository's HEAD when it was decided (default: HEAD of the working directory's
        /// repository, when it belongs to the project).
        #[arg(long)]
        commit: Option<String>,
        /// A decision this one replaces: it becomes `superseded`, with `superseded_by` this one.
        #[arg(long)]
        supersedes: Option<String>,
    },
    /// Changes fields: `key=value`, or `key+=a, b` for files and related. A decision is replaced rather
    /// than rewritten: `status=superseded superseded_by=<id>`.
    Set {
        id: String,
        #[arg(required = true)]
        changes: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Writes the handoff card of a session; a card for the same session is rewritten. Each value of a
    /// repeated flag is one bullet.
    Write {
        /// The agent's session id (its first 8 characters name the card).
        #[arg(long)]
        session: String,
        /// Defaults to the project of the working directory.
        #[arg(long)]
        project: Option<String>,
        /// `YYYY-MM-DD HH:MM` (default: now).
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        done: Vec<String>,
        #[arg(long = "decision")]
        decisions: Vec<String>,
        #[arg(long)]
        open: Vec<String>,
        #[arg(long = "next", default_value = "")]
        next_step: String,
        /// Ids of the records this session wrote or changed.
        #[arg(long)]
        touched: Vec<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum HookEvent {
    SessionStart,
    Prompt,
    SessionEnd,
    ToolUse,
    PreCompact,
}

#[derive(Subcommand)]
enum SetupAction {
    /// Validates and saves the vault and access choice together. An empty vault folder becomes a new vault.
    Set {
        #[arg(long = "vault")]
        vault: PathBuf,
        /// Allow every folder.
        #[arg(long, conflicts_with = "folders")]
        auto: bool,
        /// An accessible folder (repeatable). Without --auto, at least one is required.
        #[arg(long = "folder", value_name = "PATH")]
        folders: Vec<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AccessAction {
    /// Allow every folder. Clears the folder list.
    Auto,
    /// Allow only these folders (at least one).
    Manual {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Adds accessible folders (manual mode).
    Add { paths: Vec<PathBuf> },
    /// Removes accessible folders (manual mode).
    Remove { paths: Vec<PathBuf> },
}

#[derive(Serialize)]
struct SetupInfo<'a> {
    vault: Option<&'a std::path::Path>,
    access: Option<&'a Access>,
    settings_file: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Hooks stay quick and quiet; connect and disconnect handle the connection themselves; status shows
    // things as they are and uninstall removes the connection, so neither repairs it.
    if !matches!(
        cli.command,
        Command::Hook { .. }
            | Command::Connect { .. }
            | Command::Disconnect { .. }
            | Command::Status { .. }
            | Command::Uninstall { .. }
    ) {
        refresh_connections();
        retry_vault_commits(cli.vault.as_deref());
    }

    match cli.command {
        Command::Hook { event } => {
            hook(event, cli.vault);
            Ok(())
        }
        Command::Project { cwd } => {
            let vault = open_vault(cli.vault)?;
            let settings = Settings::load()?;
            let access = settings
                .access()
                .context("accessible folders are not set up; run `sbrain setup set`")?;
            let cwd = match cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir()?,
            };
            let resolution = Resolver::new(&vault, access).resolve(&cwd);
            if resolution.rule == Rule::NotAccessible {
                eprintln!(
                    "{} is not accessible: `sbrain access add <path>` or `sbrain access auto`",
                    cwd.display()
                );
            }
            print_json(&resolution)
        }
        Command::Overview => print_json(&sbrain_core::overview::overview(&open_vault(cli.vault)?)?),
        Command::Graph => print_json(&sbrain_core::graph::graph(&open_vault(cli.vault)?)?),
        Command::Item { action } => item(action, cli.vault),
        Command::Items => print_json(&sbrain_core::browse::items(&open_vault(cli.vault)?)?),
        Command::Doctor { quick, json } => {
            let vault = open_vault(cli.vault)?;
            let access = Settings::load()?
                .access()
                .cloned()
                .unwrap_or_else(|_| Access::auto());
            let report = sbrain_core::doctor::doctor(
                &vault,
                &access,
                chrono::Local::now().naive_local(),
                quick,
            )?;
            if json {
                return print_json(&report);
            }
            print_doctor(&report);
            Ok(())
        }
        Command::Sweep {
            action: Some(SweepAction::Schedule { every, per_run }),
            ..
        } => schedule(every, per_run, cli.vault),
        Command::Rule { action } => rule(action, cli.vault),
        Command::Engine { action: None } => print_engine(&Settings::load()?),
        Command::Engine {
            action:
                Some(EngineAction::Set {
                    client,
                    model,
                    effort,
                }),
        } => set_engine(&client, model, effort),
        Command::Sweep {
            action: Some(SweepAction::Unschedule),
            ..
        } => unschedule(),
        Command::Sweep {
            action: Some(SweepAction::Retry { session }),
            ..
        } => {
            let vault = open_vault(cli.vault)?;
            let ledger = sbrain_core::ledger::Ledger::open(&vault)?;
            let n = ledger.requeue(
                sbrain_core::sweep::DISTILL,
                session.as_deref(),
                chrono::Local::now().timestamp(),
            )?;
            match (n, session) {
                (0, Some(s)) => println!("{s} is not a session the sweep gave up on."),
                (0, None) => println!("The sweep has given up on no session."),
                (n, _) => println!(
                    "{n} session{} will be tried again on the next sweep.",
                    if n == 1 { "" } else { "s" }
                ),
            }
            Ok(())
        }
        Command::Sweep {
            action: None,
            due,
            dry_run,
            only,
            max,
            settle,
            json,
        } => sweep::run(
            &open_vault(cli.vault)?,
            &sweep::Options {
                due,
                dry_run,
                only,
                max,
                settle,
                json,
            },
        ),
        Command::Decision { action } => decision(action, cli.vault),
        Command::Session { action } => session(action, cli.vault),
        Command::Decisions => print_json(&sbrain_core::browse::decisions(&open_vault(cli.vault)?)?),
        Command::Sessions => print_json(&sbrain_core::browse::sessions(&open_vault(cli.vault)?)?),
        Command::Import { source, yes, list } => {
            let vault = open_vault(cli.vault)?;
            let mut plan = if yes {
                sbrain_core::import::apply(&source, &vault)?
            } else {
                sbrain_core::import::plan(&source, &vault)?
            };
            if !list {
                plan.steps.clear();
            }
            print_json(&plan)?;
            if !yes {
                eprintln!("Nothing was written. Run again with --yes to import.");
            }
            Ok(())
        }
        Command::Status { json } => {
            let status = status::gather(cli.vault)?;
            if json {
                print_json(&status)
            } else {
                status::print(&status);
                Ok(())
            }
        }
        Command::Uninstall { yes } => uninstall::run(yes),
        Command::Clients => {
            let found: Vec<_> = sbrain_clients::all()
                .iter()
                .map(|c| {
                    Ok(ClientInfo {
                        detection: c.detect(),
                        connected: c.connector().map(|k| k.is_connected()).transpose()?,
                    })
                })
                .collect::<Result<_>>()?;
            print_json(&found)
        }
        Command::Connect { client, yes } => {
            let client = connectable(&client)?;
            let connector = client.connector().context("not connectable")?;
            let sbrain = std::env::current_exe()?;
            // Hooks without a vault and access choice would print nothing; set those up first.
            let settings = Settings::load()?;
            Vault::from_settings(&settings).context("choose a vault first: `sbrain setup set`")?;
            settings
                .access()
                .context("set up accessible folders first: `sbrain setup set`")?;
            if !yes {
                println!(
                    "Connecting sbrain to {} will make these changes:\n",
                    client.display_name()
                );
                for change in connector.plan(&sbrain)? {
                    println!("{}", change.title);
                    for line in change.detail.lines() {
                        println!("  {line}");
                    }
                }
                println!(
                    "\nRun again with --yes to apply. `sbrain disconnect` undoes exactly these."
                );
                return Ok(());
            }
            connector.connect(&sbrain)?;
            println!("Connected sbrain to {}.", client.display_name());
            if Schedule::load().ok().flatten().is_none() {
                println!(
                    "`sbrain sweep schedule` has this computer distill finished sessions by itself."
                );
            }
            Ok(())
        }
        Command::Disconnect { client } => {
            let client = connectable(&client)?;
            client
                .connector()
                .context("not connectable")?
                .disconnect()?;
            println!("Disconnected sbrain from {}.", client.display_name());
            Ok(())
        }
        Command::Setup { action: None } | Command::Access { action: None } => {
            print_setup(&Settings::load()?)
        }
        Command::Setup {
            action:
                Some(SetupAction::Set {
                    vault,
                    auto,
                    folders,
                }),
        } => {
            let access = if auto {
                Access::auto()
            } else if folders.is_empty() {
                bail!("choose --auto or at least one --folder <path>");
            } else {
                Access::manual(folders)
            };
            let mut settings = Settings::load()?;
            settings.configure(&vault, &access)?;
            settings.save()?;
            print_setup(&settings)
        }
        Command::Access {
            action: Some(action),
        } => {
            let mut settings = Settings::load()?;
            let mut access = settings
                .access
                .clone()
                .unwrap_or_else(|| Access::manual([]));
            if matches!(
                action,
                AccessAction::Add { .. } | AccessAction::Remove { .. }
            ) && access.mode == AccessMode::Auto
            {
                bail!("auto mode has no folder list; use `sbrain access manual <path>...`");
            }
            match action {
                AccessAction::Auto => access = Access::auto(),
                AccessAction::Manual { paths } => access = Access::manual(paths),
                AccessAction::Add { paths } => access.folders.extend(paths),
                AccessAction::Remove { paths } => {
                    let remove: Vec<PathBuf> = paths
                        .iter()
                        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                        .collect();
                    access.folders.retain(|f| !remove.contains(f));
                }
            }
            settings.set_access(&access)?;
            settings.save()?;
            print_setup(&settings)
        }
    }
}

/// Runs a hook. Any error or panic is logged to the vault and swallowed: a hook never fails a session.
fn hook(event: HookEvent, vault: Option<PathBuf>) {
    let (event, name) = match event {
        HookEvent::SessionStart => (
            sbrain_clients::claude::hooks::Event::SessionStart,
            "session-start",
        ),
        HookEvent::Prompt => (sbrain_clients::claude::hooks::Event::Prompt, "prompt"),
        HookEvent::SessionEnd => (
            sbrain_clients::claude::hooks::Event::SessionEnd,
            "session-end",
        ),
        HookEvent::ToolUse => (sbrain_clients::claude::hooks::Event::ToolUse, "tool-use"),
        HookEvent::PreCompact => (
            sbrain_clients::claude::hooks::Event::PreCompact,
            "pre-compact",
        ),
    };
    let started = std::time::Instant::now();
    let vault = open_vault(vault).ok();
    let run = || -> Result<Option<String>> {
        let vault = vault.as_ref().context("no vault")?;
        let mut payload = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload)?;
        let settings = Settings::load()?;
        let access = settings.access()?;
        let today = chrono::Local::now().date_naive();
        Ok(sbrain_clients::claude::hooks::run(
            event, &payload, vault, access, today,
        )?)
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("panic")));
    if let Some(vault) = &vault {
        sbrain_core::context::note_hook_time(vault, name, started.elapsed().as_millis());
    }
    match outcome {
        Ok(Some(line)) => println!("{line}"),
        Ok(None) => {}
        Err(err) => {
            let Some(vault) = vault else { return };
            // One line, control characters dropped, never through a symlink.
            let line = format!(
                "{} {name}: {err:#}",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
            );
            let _ =
                sbrain_core::safe::append_line(&vault.cache_dir().join("hook-errors.log"), &line);
        }
    }
}

#[derive(Serialize)]
struct ClientInfo {
    #[serde(flatten)]
    detection: sbrain_clients::Detection,
    /// `null` when sbrain cannot be connected to this client.
    connected: Option<bool>,
}

/// Makes the vault commits that failed before (the job ledger keeps them), when git lets them now. A
/// failure here never stops the command; the commit stays for the next try.
fn retry_vault_commits(vault: Option<&std::path::Path>) {
    let vault = match vault {
        Some(path) => Vault::open(path),
        None => Vault::discover(),
    };
    if let Ok(vault) = vault {
        let _ = sbrain_core::git::retry_commits(vault.root(), chrono::Local::now().timestamp());
    }
}

/// Keeps connections and the scheduled sweep working (`Connector::refresh`) and says on stderr what
/// changed or is broken. A failure here never stops the command.
fn refresh_connections() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    refresh_schedule(&exe);
    for client in sbrain_clients::all() {
        let Some(connector) = client.connector() else {
            continue;
        };
        let name = client.display_name();
        match connector.refresh(&exe) {
            Ok(Refresh::Updated) => eprintln!("sbrain: added the hooks {name} lacked"),
            Ok(Refresh::Moved { from, to }) => eprintln!(
                "sbrain: {name} hooks now run {} ({} is gone)",
                to.display(),
                from.display()
            ),
            Ok(Refresh::Broken { sbrain }) => eprintln!(
                "sbrain: {name} hooks run {}, which is gone; `sbrain connect {} --yes` points them here",
                sbrain.display(),
                client.id().as_str()
            ),
            Ok(Refresh::Unchanged) | Err(_) => {}
        }
    }
}

fn connectable(id: &str) -> Result<Box<dyn sbrain_clients::Client>> {
    let client = sbrain_clients::get(id).with_context(|| format!("unknown client: {id}"))?;
    if client.connector().is_none() {
        bail!("sbrain cannot be connected to {}", client.display_name());
    }
    Ok(client)
}

fn print_setup(settings: &Settings) -> Result<()> {
    print_json(&SetupInfo {
        vault: settings.vault.as_deref(),
        access: settings.access.as_ref(),
        settings_file: Settings::default_path()?,
    })
}

fn item(action: ItemAction, vault: Option<PathBuf>) -> Result<()> {
    let vault = open_vault(vault)?;
    if let ItemAction::Verify { id, repo, timeout } = action {
        let settings = Settings::load()?;
        let access = settings
            .access()
            .cloned()
            .unwrap_or_else(|_| Access::auto());
        let opts = sbrain_core::verify::Options {
            repo,
            timeout: std::time::Duration::from_secs(timeout),
        };
        let now = chrono::Local::now().naive_local();
        let result = sbrain_core::verify::verify_item(&vault, &access, &id, &opts, now)?;
        let verdict = if result.passed { "passed" } else { "failed" };
        sbrain_core::git::commit(
            vault.root(),
            std::slice::from_ref(&result.path),
            &format!("item: verify {id} ({verdict})"),
        );
        print_json(&result)?;
        if !result.passed {
            std::process::exit(1);
        }
        return Ok(());
    }
    let today = chrono::Local::now().date_naive();
    let (written, change) = match action {
        ItemAction::Add {
            project,
            title,
            kind,
            status,
            body,
            captured_during,
            aliases,
            files,
            related,
        } => {
            let project = project_or_cwd(&vault, project)?;
            let item = sbrain_core::store::NewItem {
                title,
                kind: kind.parse()?,
                status: status.parse()?,
                body,
                captured_during,
                aliases: csv(&aliases),
                files: csv(&files),
                related: csv(&related),
            };
            (
                sbrain_core::store::add_item(&vault, &project, &item, today)?,
                "add",
            )
        }
        ItemAction::Set { id, changes } => {
            let changes = changes
                .iter()
                .map(|c| sbrain_core::store::Change::parse(c))
                .collect::<sbrain_core::Result<Vec<_>>>()?;
            (
                sbrain_core::store::set_item(&vault, &id, &changes, today)?,
                "change",
            )
        }
        ItemAction::Verify { .. } => unreachable!("handled above"),
    };
    sbrain_core::git::commit(
        vault.root(),
        std::slice::from_ref(&written.path),
        &format!("item({}): {change} {}", written.project, written.id),
    );
    print_json(&written)
}

/// `project`, or the project of the working directory.
fn project_or_cwd(vault: &Vault, project: Option<String>) -> Result<String> {
    if let Some(p) = project {
        return Ok(p);
    }
    let settings = Settings::load()?;
    let access = settings
        .access()
        .context("accessible folders are not set up; run `sbrain setup set`")?;
    let cwd = std::env::current_dir()?;
    Resolver::new(vault, access)
        .resolve(&cwd)
        .project
        .context("this folder belongs to no project; pass --project")
}

/// HEAD of the working directory's repository, when that repository belongs to `project`.
fn project_head(vault: &Vault, project: &str) -> Option<String> {
    let settings = Settings::load().ok()?;
    let cwd = std::env::current_dir().ok()?;
    let res = Resolver::new(vault, settings.access().ok()?).resolve(&cwd);
    if res.project.as_deref() != Some(project) {
        return None;
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(res.repo?)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    let head = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !head.is_empty()).then_some(head)
}

fn csv(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}

fn decision(action: DecisionAction, vault: Option<PathBuf>) -> Result<()> {
    use sbrain_core::store::{self, Change};
    let vault = open_vault(vault)?;
    let today = chrono::Local::now().date_naive();
    match action {
        DecisionAction::Add {
            project,
            title,
            decision,
            context,
            why,
            alternatives,
            files,
            related,
            commit,
            supersedes,
        } => {
            let project = project_or_cwd(&vault, project)?;
            // The decision it replaces must exist before anything is written.
            if let Some(old) = &supersedes {
                store::find_decision(&vault, old)?;
            }
            let commit = commit.or_else(|| project_head(&vault, &project));
            let mut related = csv(&related);
            if let Some(old) = &supersedes
                && !related.contains(old)
            {
                related.push(old.clone());
            }
            let new = store::NewDecision {
                title,
                context,
                decision,
                why,
                alternatives,
                related,
                files: csv(&files),
                commit: commit.unwrap_or_default(),
            };
            let written = store::add_decision(&vault, &project, &new, today)?;
            let mut files = vec![written.path.clone()];
            let mut message = format!("decision({project}): add {}", written.id);
            if let Some(old) = &supersedes {
                let changes = [
                    Change::parse("status=superseded")?,
                    Change::Set {
                        key: "superseded_by".into(),
                        value: written.id.clone(),
                    },
                ];
                match store::set_decision(&vault, old, &changes) {
                    Ok(w) => {
                        files.push(w.path);
                        message.push_str(&format!(", superseding {old}"));
                    }
                    Err(e) => {
                        // The new decision is kept, and committed, even when the old one cannot change.
                        sbrain_core::git::commit(vault.root(), &files, &message);
                        return Err(e).with_context(|| {
                            format!(
                                "{} was written, but {old} was not marked superseded",
                                written.id
                            )
                        });
                    }
                }
            }
            sbrain_core::git::commit(vault.root(), &files, &message);
            print_json(&written)
        }
        DecisionAction::Set { id, changes } => {
            let changes = changes
                .iter()
                .map(|c| Change::parse(c))
                .collect::<sbrain_core::Result<Vec<_>>>()?;
            let written = store::set_decision(&vault, &id, &changes)?;
            sbrain_core::git::commit(
                vault.root(),
                std::slice::from_ref(&written.path),
                &format!("decision({}): change {id}", written.project),
            );
            print_json(&written)
        }
    }
}

fn session(action: SessionAction, vault: Option<PathBuf>) -> Result<()> {
    use sbrain_core::store::{self, Card, CardSource};
    let vault = open_vault(vault)?;
    let SessionAction::Write {
        session,
        project,
        date,
        done,
        decisions,
        open,
        next_step,
        touched,
    } = action;
    let card = Card {
        done,
        decisions,
        open,
        next_step,
        touched,
    };
    if card.is_empty() {
        bail!("an empty card says nothing; give --done, --open or --next at least");
    }
    let project = project_or_cwd(&vault, project)?;
    let date = date.unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d %H:%M").to_string());
    let written = store::write_card(
        &vault,
        &project,
        &session,
        &date,
        CardSource::Handoff,
        &card.body(),
    )?;
    sbrain_core::git::commit(
        vault.root(),
        std::slice::from_ref(&written.path),
        &format!("session({project}): handoff {}", written.id),
    );
    print_json(&written)
}

fn print_doctor(report: &sbrain_core::doctor::Report) {
    use sbrain_core::doctor::Level;
    if report.findings.is_empty() {
        println!("Healthy: nothing found.");
        return;
    }
    let count = |l| report.counts.get(&l).copied().unwrap_or(0);
    println!(
        "{} errors, {} warnings, {} notes",
        count(Level::Error),
        count(Level::Warning),
        count(Level::Info)
    );
    for f in &report.findings {
        let level = match f.level {
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Info => "note",
        };
        println!("[{level}] {}: {}", f.check, f.msg);
        if !f.fix.is_empty() {
            println!("    → {}", f.fix);
        }
    }
}

fn rule(action: RuleAction, vault: Option<PathBuf>) -> Result<()> {
    let vault = open_vault(vault)?;
    let today = chrono::Local::now().date_naive();
    match action {
        RuleAction::List => print_json(&sbrain_core::store::proposals(&vault)?),
        RuleAction::Accept { id } => {
            let files = sbrain_core::store::accept_rule(&vault, &id, today)?;
            sbrain_core::git::commit(vault.root(), &files, &format!("rule: accept {id}"));
            println!("Rule {id} is in effect.");
            Ok(())
        }
        RuleAction::Reject { id } => {
            let file = sbrain_core::store::reject_rule(&vault, &id)?;
            sbrain_core::git::commit(vault.root(), &[file], &format!("rule: reject {id}"));
            println!("Rule {id} is dropped.");
            Ok(())
        }
    }
}

/// `sbrain sweep schedule`: saves how often, then installs the job that runs `sbrain sweep --due`.
fn schedule(every: Option<u32>, per_run: Option<usize>, vault: Option<PathBuf>) -> Result<()> {
    let mut settings = Settings::load()?;
    if let Some(hours) = every {
        if hours == 0 || !SweepSettings::EVERY_HOURS.contains(&hours) {
            bail!(
                "--every takes 1, 3, 6, 12 or 24 hours; `sbrain sweep unschedule` stops the sweep"
            );
        }
        settings.sweep.every_hours = hours;
    }
    if let Some(n) = per_run {
        if !SweepSettings::PER_RUN.contains(&n) {
            bail!("--per-run takes 2, 4 or 8 sessions");
        }
        settings.sweep.per_run = n;
    }
    if settings.sweep.every_hours == 0 {
        bail!("the sweep is set to run only by hand; choose how often with --every");
    }
    let vault = open_vault(vault.clone())?;
    if every.is_some() || per_run.is_some() {
        settings.save()?;
    }

    let (sbrain, _) = sbrain_clients::runnable_sbrain(&std::env::current_exe()?)?;
    let mut job = schedule::Job::new(sbrain);
    if sbrain_core::env_var("VAULT").is_none() && settings.vault.as_deref() != Some(vault.root()) {
        job.env
            .push(("SBRAIN_VAULT".into(), vault.root().display().to_string()));
    }
    let host = schedule::Host::current()?;
    if let Some(old) = Schedule::load()? {
        host.uninstall(&old)?;
        Schedule::forget()?;
    }
    let installed = host.install(&job)?;
    installed.save()?;

    let hours = settings.sweep.every_hours;
    println!(
        "The sweep runs every {hours} hour{} on this computer: {} checks every {} minutes whether it is due.",
        if hours == 1 { "" } else { "s" },
        installed.service,
        schedule::TICK_MINUTES
    );
    println!("Job: {}", installed.files[0].display());
    match &installed.log {
        Some(log) => println!("Log: {}", log.display()),
        None => println!(
            "Log: journalctl --user -u {}",
            installed.name.replace(".timer", ".service")
        ),
    }
    println!("`sbrain sweep unschedule` stops it.");
    Ok(())
}

#[derive(Serialize)]
struct EngineInfo {
    /// The chosen CLI (`claude` when none is chosen), with its model and effort.
    client: String,
    model: Option<String>,
    effort: Option<String>,
    choices: Vec<EngineChoice>,
}

#[derive(Serialize)]
struct EngineChoice {
    client: &'static str,
    installed: bool,
    models: &'static [&'static str],
    efforts: Vec<String>,
}

fn print_engine(settings: &Settings) -> Result<()> {
    let chosen = settings.engine.clone().unwrap_or(sbrain_core::Engine {
        client: "claude".into(),
        model: None,
        effort: None,
    });
    let choices = sbrain_clients::all()
        .iter()
        .filter(|c| c.sweeper().is_some())
        .map(|c| EngineChoice {
            client: c.id().as_str(),
            installed: c.detect().installed(),
            models: c.model_aliases(),
            efforts: c.effort_levels(),
        })
        .collect();
    print_json(&EngineInfo {
        client: chosen.client,
        model: chosen.model,
        effort: chosen.effort,
        choices,
    })
}

fn set_engine(client: &str, model: Option<String>, effort: Option<String>) -> Result<()> {
    let c = sbrain_clients::get(client).with_context(|| format!("unknown client: {client}"))?;
    if c.sweeper().is_none() {
        bail!("{} cannot run sbrain's work", c.display_name());
    }
    let effort = effort
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty());
    if let Some(e) = &effort {
        let levels = c.effort_levels();
        if !levels.contains(e) {
            bail!(
                "{} takes no effort {e:?}; it takes: {}",
                c.display_name(),
                levels.join(", ")
            );
        }
    }
    let mut settings = Settings::load()?;
    settings.engine = Some(
        sbrain_core::Engine {
            client: c.id().as_str().to_owned(),
            model,
            effort,
        }
        .normalized(),
    );
    settings.save()?;
    print_engine(&settings)
}

fn unschedule() -> Result<()> {
    let Some(installed) = Schedule::load()? else {
        println!("The sweep is not scheduled on this computer.");
        return Ok(());
    };
    schedule::Host::current()?.uninstall(&installed)?;
    Schedule::forget()?;
    println!("The sweep no longer runs by itself; `sbrain sweep` runs it by hand.");
    Ok(())
}

/// A scheduled job whose sbrain is gone moves to the running sbrain when PATH finds it.
fn refresh_schedule(exe: &std::path::Path) {
    let Ok(Some(installed)) = Schedule::load() else {
        return;
    };
    if installed.sbrain.exists() {
        return;
    }
    let moved = (|| -> Result<Option<PathBuf>> {
        let (sbrain, on_path) = sbrain_clients::runnable_sbrain(exe)?;
        if !on_path {
            return Ok(None);
        }
        let host = schedule::Host::current()?;
        host.uninstall(&installed)?;
        let mut job = schedule::Job::new(sbrain.clone());
        job.env = installed.env.clone();
        host.install(&job)?.save()?;
        Ok(Some(sbrain))
    })();
    match moved {
        Ok(Some(sbrain)) => eprintln!(
            "sbrain: the scheduled sweep now runs {} (it ran {})",
            sbrain.display(),
            installed.sbrain.display()
        ),
        _ => eprintln!(
            "sbrain: the scheduled sweep runs {}, which is gone; `sbrain sweep schedule` points it here",
            installed.sbrain.display()
        ),
    }
}

fn open_vault(path: Option<PathBuf>) -> Result<Vault> {
    match path {
        Some(path) => {
            Vault::open(&path).with_context(|| format!("could not open vault: {}", path.display()))
        }
        None => Vault::discover()
            .context("could not open the vault; choose one with `sbrain setup set`"),
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
