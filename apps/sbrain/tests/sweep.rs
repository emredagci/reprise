//! `sbrain sweep` end to end: a temporary vault, Claude folder and settings, and a script in place of
//! `claude` that answers with fixed JSON.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const ANSWER: &str = r#"{"is_error":false,"subtype":"success","structured_output":{"card":{"done":["Cached the login query"],"decisions":[],"open":[],"next_step":"Measure it.","uncertain":[]},"new_items":[{"title":"Cache the login query","type":"feature","status":"done-unverified","problem":"Slow.","solution":"Cached.","files":[],"aliases":[],"commit":"","verify":"cargo test login","evidence":["cache the login query"]}],"item_updates":[],"new_decisions":[],"rules":[],"facts":[],"events":[],"state":null}}"#;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    /// A vault in git (with `skip_cwd_prefixes` emptied, since the temporary folder is one of them), a
    /// repository with a remote, and a `claude` script that answers `answer`.
    fn new(answer: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in ["vault", "work/app", "claude", "bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let env = Self { _tmp: tmp, root };
        let vault = env.root.join("vault");
        assert!(
            env.sbrain(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"])
                .status
                .success()
        );
        let toml = vault.join(".sbrain/vault.toml");
        let text = std::fs::read_to_string(&toml).unwrap();
        std::fs::write(&toml, format!("{text}skip_cwd_prefixes = []\n")).unwrap();
        git(&vault, &["init", "-q"]);
        git(&vault, &["add", "-A"]);
        git(
            &vault,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        );
        let work = env.root.join("work/app");
        git(&work, &["init", "-q"]);
        git(
            &work,
            &["remote", "add", "origin", "git@github.com:acme/app.git"],
        );
        env.answer_with(answer);
        env
    }

    fn answer_with(&self, answer: &str) {
        let script = self.root.join("claude.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat > /dev/null\nprintf '%s' '{answer}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn sbrain(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_sbrain"))
            .args(args)
            .env("SBRAIN_SETTINGS", self.root.join("settings.json"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("SBRAIN_BIN_DIR", self.root.join("bin"))
            .env("SBRAIN_CLAUDE_BIN", self.root.join("claude.sh"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("SBRAIN_CODEX_BIN", self.root.join("codex.sh"))
            .env_remove("SBRAIN_VAULT")
            .output()
            .unwrap()
    }

    /// Runs `sbrain sweep --json --settle 0` and returns its events.
    fn sweep(&self, extra: &[&str]) -> Vec<Value> {
        let mut args = vec!["sweep", "--json", "--settle", "0"];
        args.extend(extra);
        let out = self.sbrain(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Writes a transcript of a session in `cwd` with these user messages.
    fn transcript(&self, session: &str, cwd: &Path, messages: &[&str]) {
        let enc: String = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = self.root.join("claude/projects").join(enc);
        std::fs::create_dir_all(&dir).unwrap();
        let lines: Vec<String> = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                serde_json::json!({
                    "type": "user",
                    "cwd": cwd,
                    "timestamp": format!("2026-10-25T09:{i:02}:00Z"),
                    "message": {"content": m}
                })
                .to_string()
            })
            .collect();
        std::fs::write(
            dir.join(format!("{session}.jsonl")),
            lines.join("\n") + "\n",
        )
        .unwrap();
    }
}

impl Env {
    /// A `codex` script that writes `answer` as its last message.
    fn codex_answers(&self, answer: &str) {
        let script = self.root.join("codex.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat > /dev/null\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output-last-message ] && out=\"$2\"; shift; done\nprintf '%s' '{answer}' > \"$out\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Writes a Codex transcript of a session in `cwd` with these user messages.
    fn codex_transcript(&self, session: &str, cwd: &Path, messages: &[&str]) {
        let dir = self.root.join("codex/sessions/2026/10/25");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = serde_json::json!({
            "timestamp": "2026-10-25T09:00:00Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": cwd}
        });
        let lines: Vec<String> = std::iter::once(meta.to_string())
            .chain(messages.iter().enumerate().map(|(i, m)| {
                serde_json::json!({
                    "timestamp": format!("2026-10-25T09:{i:02}:30Z"),
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": m}
                })
                .to_string()
            }))
            .collect();
        std::fs::write(
            dir.join(format!("rollout-2026-10-25T09-00-00-{session}.jsonl")),
            lines.join("\n") + "\n",
        )
        .unwrap();
    }
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn sessions(events: &[Value]) -> Vec<&Value> {
    events.iter().filter(|e| e["event"] == "session").collect()
}

/// The first run only marks where to start; later sessions are distilled once, written and committed.
#[test]
fn distills_new_sessions_once() {
    let env = Env::new(ANSWER);
    let work = env.root.join("work/app");
    env.transcript("0ld0ld00-0000", &work, &["old", "history"]);
    let first = env.sweep(&[]);
    assert_eq!(first[0]["event"], "baseline-set");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    env.transcript(
        "1a2b3c4d-0000",
        &work,
        &["cache the login query", "commit it"],
    );

    let dry = env.sweep(&["--dry-run"]);
    let s = sessions(&dry);
    assert_eq!(
        s.len(),
        1,
        "history before the first run is left alone: {dry:?}"
    );
    assert_eq!(s[0]["status"], "would-distill");
    assert!(
        !env.vault().join("projects/app").exists(),
        "a dry run writes nothing"
    );

    let real = env.sweep(&[]);
    let s = sessions(&real);
    assert_eq!(s[0]["status"], "ok", "{real:?}");
    assert_eq!(s[0]["project"], "app");
    assert_eq!(s[0]["applied"]["items_added"][0], "app-0001");
    let card = s[0]["applied"]["card"].as_str().unwrap();
    assert!(
        card.starts_with("projects/app/sessions/") && card.ends_with("-1a2b3c4d.md"),
        "{card}"
    );
    assert!(s[0]["commit"].is_string(), "the vault is committed");
    let status = Command::new("git")
        .arg("-C")
        .arg(env.vault())
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "",
        "everything written is in the commit"
    );

    assert!(sessions(&env.sweep(&[])).is_empty(), "nothing is due twice");

    // The last run is kept: `--due` schedules from it and the doctor reads it.
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(env.vault().join(".sbrain/cache/sweep.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["last_run"]["remaining"], 0);
    assert!(state["last_run"]["sessions"].as_array().unwrap().is_empty());
}

#[test]
fn a_run_stops_at_its_limit_and_says_how_many_wait() {
    let env = Env::new(ANSWER);
    env.sweep(&[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let work = env.root.join("work/app");
    for id in ["11111111-a", "22222222-b", "33333333-c"] {
        env.transcript(id, &work, &["cache the login query", "b"]);
    }
    let events = env.sweep(&["--max", "2"]);
    assert_eq!(sessions(&events).len(), 2);
    let done = events.last().unwrap();
    assert_eq!(done["event"], "done");
    assert_eq!(done["remaining"], 1);
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(env.vault().join(".sbrain/cache/sweep.json")).unwrap(),
    )
    .unwrap();
    let run = &state["last_run"];
    assert_eq!(run["remaining"], 1);
    assert_eq!(run["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(run["sessions"][0]["status"], "ok");
    assert_eq!(run["sessions"][0]["items_added"], 1);
    assert_eq!(run["sessions"][0]["left_out"], 0);
    assert!(
        run["sessions"][0]["card"]
            .as_str()
            .unwrap()
            .contains("/sessions/")
    );
    assert_eq!(
        sessions(&env.sweep(&[])).len(),
        1,
        "the one left is done next"
    );
}

#[test]
fn skips_what_needs_no_distilling() {
    let env = Env::new(ANSWER);
    env.sweep(&[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let work = env.root.join("work/app");
    env.transcript("aaaaaaaa-short", &work, &["just one question"]);
    env.transcript("bbbbbbbb-outside", &env.root.join("work"), &["a", "b"]);
    // A handoff card written after the user's last message covers the session.
    env.transcript("cccccccc-handoff", &work, &["a", "b"]);
    let sessions_dir = env.vault().join("projects/app/sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(
        sessions_dir.join("2026-10-25-2359-cccccccc.md"),
        "---\nsession: cccccccc\nproject: app\ndate: 2099-01-01 00:00\nsource: handoff\n---\n\n## Done\n",
    )
    .unwrap();
    std::fs::write(
        env.vault().join("projects/app/project.toml"),
        "prefix = \"app\"\nrepos = [\"github.com/acme/app\"]\n",
    )
    .unwrap();

    let events = env.sweep(&[]);
    let reason = |id: &str| {
        sessions(&events)
            .into_iter()
            .find(|e| e["session"] == id)
            .map(|e| {
                (
                    e["status"].as_str().unwrap().to_owned(),
                    e["reason"].as_str().unwrap_or("").to_owned(),
                )
            })
            .unwrap()
    };
    assert_eq!(
        reason("aaaaaaaa-short"),
        ("skipped".into(), "short (1 messages)".into())
    );
    assert_eq!(reason("bbbbbbbb-outside").0, "skipped");
    assert!(reason("bbbbbbbb-outside").1.starts_with("not a project"));
    assert_eq!(
        reason("cccccccc-handoff"),
        ("skipped".into(), "covered by a handoff card".into())
    );
}

#[test]
fn a_failed_run_is_tried_again() {
    let env = Env::new(r#"{"is_error":true,"result":"Not logged in"}"#);
    env.sweep(&[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    env.transcript("dddddddd-0000", &env.root.join("work/app"), &["a", "b"]);
    let failed = env.sweep(&[]);
    let s = sessions(&failed);
    assert_eq!(s[0]["status"], "failed");
    assert!(s[0]["reason"].as_str().unwrap().contains("Not logged in"));
    assert!(!env.vault().join("projects/app/sessions").exists());

    // Tried again after a wait, not on the very next run; asked for by name, at once.
    assert!(sessions(&env.sweep(&[])).is_empty(), "backing off");
    env.answer_with(ANSWER);
    let again = env.sweep(&["--only", "dddddddd-0000"]);
    assert_eq!(sessions(&again)[0]["status"], "ok", "{again:?}");
}

/// After `ledger::MAX_ATTEMPTS` failures a session is given up; `sweep retry` brings it back.
#[test]
fn a_session_failing_every_time_is_given_up_until_retried() {
    let env = Env::new(r#"{"is_error":true,"result":"Not logged in"}"#);
    env.sweep(&[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let id = "99999999-0000";
    env.transcript(id, &env.root.join("work/app"), &["a", "b"]);
    let mut last = Vec::new();
    for _ in 0..8 {
        last = env.sweep(&["--only", id]);
        assert_eq!(sessions(&last)[0]["status"], "failed");
    }
    let reason = sessions(&last)[0]["reason"].as_str().unwrap().to_owned();
    assert!(
        reason.contains("given up after 8 tries") && reason.contains("sweep retry"),
        "{reason}"
    );

    let doctor = env.sbrain(&["doctor", "--quick", "--json"]);
    let report = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        report.contains("gave up") && report.contains(id),
        "{report}"
    );

    let retry = env.sbrain(&["sweep", "retry"]);
    assert!(String::from_utf8_lossy(&retry.stdout).contains("1 session will be tried again"));
    env.answer_with(ANSWER);
    assert_eq!(
        sessions(&env.sweep(&[]))[0]["status"],
        "ok",
        "due again at once"
    );
    let report =
        String::from_utf8_lossy(&env.sbrain(&["doctor", "--quick", "--json"]).stdout).into_owned();
    assert!(!report.contains("gave up"), "{report}");
}

#[test]
fn an_answer_off_the_schema_writes_nothing() {
    let env = Env::new(r#"{"is_error":false,"structured_output":{"card":{}}}"#);
    env.sweep(&[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    env.transcript("eeeeeeee-0000", &env.root.join("work/app"), &["a", "b"]);
    let s = env.sweep(&[]);
    assert_eq!(sessions(&s)[0]["status"], "failed");
    assert!(
        sessions(&s)[0]["reason"]
            .as_str()
            .unwrap()
            .contains("distill output")
    );
    assert!(!env.vault().join("projects/app/sessions").exists());
}

#[test]
fn an_ended_session_does_not_wait_to_settle() {
    let env = Env::new(ANSWER);
    env.sweep(&[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let work = env.root.join("work/app");
    env.transcript("ffffffff-0000", &work, &["a", "b"]);
    let sweep = |env: &Env| -> Vec<Value> {
        // The default settle time (an hour): only the SessionEnd mark makes it due.
        let out = env.sbrain(&["sweep", "--json"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    assert!(sessions(&sweep(&env)).is_empty(), "still settling");

    let mut hook = Command::new(env!("CARGO_BIN_EXE_sbrain"))
        .args(["hook", "session-end"])
        .env("SBRAIN_SETTINGS", env.root.join("settings.json"))
        .env_remove("SBRAIN_VAULT")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    let payload = serde_json::json!({"session_id": "ffffffff-0000", "cwd": work, "reason": "exit"});
    hook.stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = hook.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "SessionEnd prints nothing");

    let events = sweep(&env);
    assert_eq!(sessions(&events)[0]["status"], "ok", "{events:?}");
    let ledger = sbrain_core::ledger::Ledger::open_root(&env.vault()).unwrap();
    assert!(
        ledger
            .list(
                sbrain_core::sweep::ENDED,
                sbrain_core::ledger::State::Pending
            )
            .unwrap()
            .is_empty(),
        "the mark is gone once handled"
    );
}

#[test]
fn a_second_run_waits_for_the_first() {
    let env = Env::new(ANSWER);
    let vault = sbrain_core::Vault::open(env.vault()).unwrap();
    let _held = sbrain_core::sweep::lock(&vault).unwrap().unwrap();
    assert_eq!(env.sweep(&[])[0]["event"], "busy");
}

fn edit_json(path: &Path, f: impl FnOnce(&mut Value)) {
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A dry first run says what a real one would do and writes nothing.
#[test]
fn a_dry_first_run_sets_no_baseline() {
    let env = Env::new(ANSWER);
    let dry = env.sweep(&["--dry-run"]);
    assert_eq!(dry.len(), 1, "{dry:?}");
    assert_eq!(dry[0]["event"], "would-set-baseline");
    assert!(!env.vault().join(".sbrain/cache/sweep.json").exists());
    assert_eq!(env.sweep(&["--dry-run"])[0]["event"], "would-set-baseline");

    let out = env.sbrain(&["sweep", "--dry-run"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Nothing was written"), "{text}");

    assert_eq!(env.sweep(&[])[0]["event"], "baseline-set");
    assert!(env.vault().join(".sbrain/cache/sweep.json").exists());
}

/// `--due` runs on the schedule from the settings, follows up soon when it left sessions, and takes
/// its limit from the settings.
#[test]
fn due_runs_follow_the_schedule() {
    let env = Env::new(ANSWER);
    let settings = env.root.join("settings.json");
    let state = env.vault().join(".sbrain/cache/sweep.json");
    edit_json(&settings, |v| {
        v["sweep"] = serde_json::json!({"every_hours": 3, "per_run": 2});
    });

    assert_eq!(
        env.sweep(&["--due"])[0]["event"],
        "baseline-set",
        "never run: due"
    );
    let not_due = env.sweep(&["--due"]);
    assert_eq!(not_due.len(), 1);
    assert_eq!(not_due[0]["event"], "not-due");
    assert!(not_due[0]["next"].is_string(), "{not_due:?}");
    let quiet = env.sbrain(&["sweep", "--due"]);
    assert!(quiet.status.success());
    assert_eq!(
        String::from_utf8_lossy(&quiet.stdout),
        "",
        "the scheduled job's log keeps only runs"
    );

    // Three hours later, a run is due; it distills `per_run` of the sessions written since.
    edit_json(&state, |v| {
        v["baseline"] = serde_json::json!(now() - 3 * 3600 - 60);
    });
    let work = env.root.join("work/app");
    for id in ["11111111-a", "22222222-b", "33333333-c"] {
        env.transcript(id, &work, &["cache the login query", "b"]);
    }
    let run = env.sweep(&["--due"]);
    assert_eq!(sessions(&run).len(), 2, "{run:?}");
    assert_eq!(run.last().unwrap()["remaining"], 1);

    // Sessions were left: the next run follows a few minutes later, not in three hours.
    assert_eq!(env.sweep(&["--due"])[0]["event"], "not-due");
    edit_json(&state, |v| {
        let finished = now() - 6 * 60;
        v["last_run"]["finished"] = serde_json::json!(finished);
    });
    let follow_up = env.sweep(&["--due"]);
    assert_eq!(sessions(&follow_up).len(), 1, "{follow_up:?}");
    assert_eq!(env.sweep(&["--due"])[0]["event"], "not-due");

    // By hand only: `--due` never runs.
    edit_json(&settings, |v| {
        v["sweep"]["every_hours"] = serde_json::json!(0)
    });
    edit_json(&state, |v| v["last_run"]["finished"] = serde_json::json!(0));
    let by_hand = env.sweep(&["--due"]);
    assert_eq!(by_hand[0]["event"], "not-due");
    assert!(by_hand[0]["next"].is_null());
}

#[test]
fn unschedule_without_a_schedule_says_so() {
    let env = Env::new(ANSWER);
    let out = env.sbrain(&["sweep", "unschedule"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("not scheduled"));
    let bad = env.sbrain(&["sweep", "schedule", "--every", "2"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("1, 3, 6, 12 or 24"));
    let bad = env.sbrain(&["sweep", "schedule", "--per-run", "3"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("2, 4 or 8"));
}

/// Without accessible folders the sweep does nothing rather than treat every folder as allowed.
#[test]
fn the_sweep_needs_accessible_folders() {
    let env = Env::new(ANSWER);
    env.sweep(&[]);
    let settings = env.root.join("settings.json");
    edit_json(&settings, |v| {
        v.as_object_mut().unwrap().remove("access");
    });
    let vault = env.vault();
    let out = env.sbrain(&["sweep", "--vault", vault.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("accessible folders are not set up"));

    std::fs::write(&settings, "{broken").unwrap();
    let out = env.sbrain(&["sweep", "--vault", vault.to_str().unwrap()]);
    assert!(!out.status.success(), "unreadable settings stop it too");
}

/// Sessions of every agent are read and the engine distills them all: with Codex as the
/// engine, a Claude Code session and a Codex session are both distilled by `codex exec`.
#[test]
fn the_engine_distills_every_agents_sessions() {
    let env = Env::new(r#"{"is_error":true,"result":"claude must not distill"}"#);
    let structured: Value = serde_json::from_str(ANSWER).unwrap();
    env.codex_answers(&structured["structured_output"].to_string());
    assert!(
        env.sbrain(&["engine", "set", "codex", "--effort", "low"])
            .status
            .success()
    );
    // Without a connection only the engine's own sessions are read.
    assert_eq!(env.sweep(&[])[0]["event"], "baseline-set");
    let alone = env.sweep(&["--dry-run"]);
    assert_eq!(
        alone[0]["sources"],
        serde_json::json!(["Codex"]),
        "{alone:?}"
    );
    let out = env.sbrain(&["connect", "claude", "--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let work = env.root.join("work/app");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    env.transcript(
        "c1a0de00-0000",
        &work,
        &["cache the login query", "commit it"],
    );
    let codex = "0199f1a2-7b3c-7d10-9a2e-5c4b3a291807";
    env.codex_transcript(codex, &work, &["now the logout", "and test it"]);

    let events = env.sweep(&[]);
    let start = events.iter().find(|e| e["event"] == "start").unwrap();
    assert_eq!(start["client"], "Codex");
    assert_eq!(
        start["sources"],
        serde_json::json!(["Claude Code", "Codex"])
    );
    let s = sessions(&events);
    assert_eq!(s.len(), 2, "{events:?}");
    for session in ["c1a0de00-0000", codex] {
        let o = s.iter().find(|o| o["session"] == session).unwrap();
        assert_eq!(o["status"], "ok", "{o}");
        assert_eq!(o["project"], "app");
    }
    assert!(sessions(&env.sweep(&[])).is_empty(), "nothing is due twice");
}
