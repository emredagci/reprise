//! Runs an item's verify command in its repo and marks it verified or done-unverified.
//!
//! The command runs in the user's login shell (so the tools their terminal finds are found), from the
//! project's repository, with a time limit. A pass sets `verified` and `verified_at`; a failure moves a
//! verified item back to `done-unverified` and leaves other statuses as they are. Either way a line goes
//! under `## Verification` with the HEAD it ran on, and `updated` becomes today. `verified` is set
//! nowhere else (`store::set_item` refuses it).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use serde::Serialize;

use crate::record::{Record, Value};
use crate::schema::ItemStatus;
use crate::{Access, Error, Result, Vault};

/// How long a verify command may run unless told otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
/// How long the output is waited for once the command has ended.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// Lines of the command's output kept in the result.
const TAIL_LINES: usize = 15;

#[derive(Debug, Clone)]
pub struct Options {
    /// The repository to run in; by default the project's (`stale::repo_for`).
    pub repo: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            repo: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub id: String,
    pub passed: bool,
    /// The command's exit code; `None` when it was stopped or killed by a signal.
    pub exit: Option<i32>,
    pub timed_out: bool,
    /// The item's status afterwards.
    pub status: String,
    pub command: String,
    pub repo: PathBuf,
    /// The repository's HEAD the command ran on, when it is a git repository.
    pub head: Option<String>,
    /// The record, relative to the vault.
    pub path: PathBuf,
    /// The last lines the command printed (stdout, then stderr).
    pub output_tail: String,
}

/// Runs item `id`'s verify command and records the result. `now` stamps `verified_at` and the line.
pub fn verify_item(
    vault: &Vault,
    access: &Access,
    id: &str,
    opts: &Options,
    now: NaiveDateTime,
) -> Result<Verification> {
    let (project, path) = crate::store::find_item(vault, id)?;
    let mut record = Record::read(&path)?;
    let command = record.text("verify").trim().to_owned();
    if command.is_empty() {
        return Err(Error::InvalidChange(format!(
            "{id} has no verify command; add one with `reprise item set {id} verify=\"<command>\"`"
        )));
    }
    let status: Option<ItemStatus> = record.text("status").parse().ok();
    if !matches!(
        status,
        Some(ItemStatus::Doing | ItemStatus::DoneUnverified | ItemStatus::Verified)
    ) {
        return Err(Error::InvalidChange(format!(
            "{id} is {}; only doing, done-unverified or verified items are verified",
            record.text("status")
        )));
    }
    let repo = match &opts.repo {
        Some(repo) => repo.clone(),
        None => crate::stale::repo_for(vault, access, &project).ok_or_else(|| {
            Error::InvalidChange(format!(
                "no repository of {project} is known on this machine; pass --repo <folder>"
            ))
        })?,
    };
    if !repo.is_dir() {
        return Err(Error::InvalidChange(format!(
            "{} is not a folder",
            repo.display()
        )));
    }

    let run = run_shell(&command, &repo, opts.timeout)?;
    let head = crate::stale::git(&repo, &["rev-parse", "--short", "HEAD"])
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty());
    let passed = run.exit == Some(0) && !run.timed_out;
    let stamp = now.format("%Y-%m-%d %H:%M").to_string();
    let on = head
        .as_deref()
        .map_or_else(String::new, |h| format!(" (HEAD {h})"));
    let line = if passed {
        record.set("status", Value::Text(ItemStatus::Verified.as_str().into()));
        record.set("verified_at", Value::Text(stamp.clone()));
        format!("- {stamp}: `{command}` passed{on}.")
    } else {
        if status == Some(ItemStatus::Verified) {
            record.set(
                "status",
                Value::Text(ItemStatus::DoneUnverified.as_str().into()),
            );
        }
        let why = match (run.timed_out, run.exit) {
            (true, _) => format!("timed out after {} s", opts.timeout.as_secs()),
            (false, Some(code)) => format!("failed, exit {code}"),
            (false, None) => "was stopped by a signal".to_owned(),
        };
        format!("- {stamp}: `{command}` {why}{on}.")
    };
    record.body = crate::store::append_to_section(&record.body, "Verification", &line);
    record.set(
        "updated",
        Value::Text(now.date().format("%Y-%m-%d").to_string()),
    );
    crate::scaffold::write(&path, &record.to_text())?;

    Ok(Verification {
        id: id.to_owned(),
        passed,
        exit: run.exit,
        timed_out: run.timed_out,
        status: record.text("status").to_owned(),
        command,
        repo,
        head,
        path: path
            .strip_prefix(vault.root())
            .map_or_else(|_| path.clone(), Path::to_path_buf),
        output_tail: tail(&run.output, TAIL_LINES),
    })
}

struct Run {
    exit: Option<i32>,
    timed_out: bool,
    output: String,
}

/// Runs `command` in the user's login shell in `dir`. On a timeout the command and everything it
/// started are stopped.
fn run_shell(command: &str, dir: &Path, timeout: Duration) -> Result<Run> {
    let mut cmd = shell(command);
    cmd.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Its own process group, so a timeout can stop what the command started too.
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|source| Error::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    // Each pipe is read on its own thread and handed over when it closes. Something the command left
    // running can keep a pipe open after the command ends; then what was read so far is not waited for.
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut out);
            }
            let _ = tx.send(out);
        });
        rx
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));

    let started = Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                stop(&mut child);
                break (child.wait().ok(), true);
            }
            Err(source) => {
                return Err(Error::Io {
                    path: dir.to_path_buf(),
                    source,
                });
            }
        }
    };
    let collect = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        String::from_utf8_lossy(&rx.recv_timeout(PIPE_GRACE).unwrap_or_default()).into_owned()
    };
    let mut output = collect(stdout);
    output.push_str(&collect(stderr));
    Ok(Run {
        exit: status.and_then(|s| s.code()),
        timed_out,
        output,
    })
}

#[cfg(unix)]
fn shell(command: &str) -> Command {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-c", command]);
    cmd
}

#[cfg(not(unix))]
fn shell(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", command]);
    cmd
}

/// Stops the command's process group, then the command itself.
fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // `--` so the negative id (the process group) is not read as an option.
        let _ = Command::new("kill")
            .args(["-s", "KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.trim().lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    struct Env {
        _tmp: tempfile::TempDir,
        vault: Vault,
        repo: PathBuf,
    }

    /// A vault with project `app` and a git repository with one commit.
    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let vault = crate::vault::tests::new_vault(&root.join("vault"));
        crate::scaffold::ensure_project(&vault, "app", None, at("2026-10-26 09:00").date())
            .unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        Env {
            _tmp: tmp,
            vault,
            repo,
        }
    }

    fn item(e: &Env, status: &str, verify: &str) -> String {
        let written = crate::store::add_item(
            &e.vault,
            "app",
            &crate::store::NewItem {
                status: status.parse().unwrap(),
                ..crate::store::NewItem::note("Cache the login query")
            },
            at("2026-10-26 09:00").date(),
        )
        .unwrap();
        if !verify.is_empty() {
            crate::store::set_item(
                &e.vault,
                &written.id,
                &[crate::store::Change::parse(&format!("verify={verify}")).unwrap()],
                at("2026-10-26 09:00").date(),
            )
            .unwrap();
        }
        written.id
    }

    fn opts(e: &Env) -> Options {
        Options {
            repo: Some(e.repo.clone()),
            timeout: Duration::from_secs(20),
        }
    }

    fn read(e: &Env, id: &str) -> Record {
        Record::read(&crate::store::find_item(&e.vault, id).unwrap().1).unwrap()
    }

    #[test]
    fn a_passing_command_verifies_and_a_failing_one_takes_it_back() {
        let e = env();
        std::fs::write(e.repo.join("ok"), "").unwrap();
        let id = item(&e, "done-unverified", "test -f ok && echo checked");
        let v = verify_item(
            &e.vault,
            &Access::auto(),
            &id,
            &opts(&e),
            at("2026-10-26 10:30"),
        )
        .unwrap();
        assert!(v.passed);
        assert_eq!(v.exit, Some(0));
        assert_eq!(v.status, "verified");
        assert_eq!(v.output_tail, "checked");
        assert_eq!(v.repo, e.repo);
        let head = v.head.clone().unwrap();
        let r = read(&e, &id);
        assert_eq!(r.text("status"), "verified");
        assert_eq!(r.text("verified_at"), "2026-10-26 10:30");
        assert_eq!(r.text("updated"), "2026-10-26");
        assert!(
            r.body.contains(&format!(
                "## Verification\n\n- 2026-10-26 10:30: `test -f ok && echo checked` passed (HEAD {head}).\n"
            )),
            "{}",
            r.body
        );

        std::fs::remove_file(e.repo.join("ok")).unwrap();
        let v = verify_item(
            &e.vault,
            &Access::auto(),
            &id,
            &opts(&e),
            at("2026-10-27 08:00"),
        )
        .unwrap();
        assert!(!v.passed);
        assert_eq!(v.exit, Some(1));
        assert_eq!(
            v.status, "done-unverified",
            "a verified item that fails is no longer verified"
        );
        let r = read(&e, &id);
        assert_eq!(
            r.text("verified_at"),
            "2026-10-26 10:30",
            "the last pass is kept"
        );
        assert!(
            r.body
                .contains("`test -f ok && echo checked` failed, exit 1")
        );
        assert!(
            r.body.find("passed").unwrap() < r.body.find("failed").unwrap(),
            "lines are added in order"
        );
    }

    #[test]
    fn a_failing_doing_item_stays_doing() {
        let e = env();
        let id = item(&e, "doing", "exit 3");
        let v = verify_item(
            &e.vault,
            &Access::auto(),
            &id,
            &opts(&e),
            at("2026-10-26 10:30"),
        )
        .unwrap();
        assert_eq!(
            (v.passed, v.exit, v.status.as_str()),
            (false, Some(3), "doing")
        );
        assert_eq!(read(&e, &id).text("verified_at"), "");
    }

    #[test]
    fn a_command_past_its_time_is_stopped() {
        let e = env();
        let id = item(&e, "done-unverified", "sleep 30 & sleep 30");
        let started = Instant::now();
        let v = verify_item(
            &e.vault,
            &Access::auto(),
            &id,
            &Options {
                repo: Some(e.repo.clone()),
                timeout: Duration::from_secs(1),
            },
            at("2026-10-26 10:30"),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it did not wait for the command"
        );
        assert!(v.timed_out && !v.passed);
        assert_eq!(v.status, "done-unverified");
        assert!(read(&e, &id).body.contains("timed out after 1 s"));
    }

    #[test]
    fn refuses_what_cannot_be_verified() {
        let e = env();
        let none = item(&e, "done-unverified", "");
        let err = verify_item(
            &e.vault,
            &Access::auto(),
            &none,
            &opts(&e),
            at("2026-10-26 10:30"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("has no verify command"), "{err}");

        let inbox = item(&e, "inbox", "true");
        let err = verify_item(
            &e.vault,
            &Access::auto(),
            &inbox,
            &opts(&e),
            at("2026-10-26 10:30"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("only doing, done-unverified or verified"),
            "{err}"
        );

        let err = verify_item(
            &e.vault,
            &Access::auto(),
            &item(&e, "doing", "true"),
            &Options::default(),
            at("2026-10-26 10:30"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("pass --repo"), "{err}");

        assert!(matches!(
            verify_item(
                &e.vault,
                &Access::auto(),
                "app-9999",
                &opts(&e),
                at("2026-10-26 10:30")
            ),
            Err(Error::RecordNotFound(_))
        ));
        assert_eq!(
            read(&e, &inbox).text("status"),
            "inbox",
            "nothing was written"
        );
    }
}
