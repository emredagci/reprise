//! `sbrain decision add|set` and `sbrain session write` end to end: records are written in the vault's
//! format and committed.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let env = Self { _tmp: tmp, root };
        let vault = env.vault();
        std::fs::create_dir_all(&vault).unwrap();
        let out = env.sbrain(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"]);
        assert!(out.status.success(), "{}", stderr(&out));
        std::fs::create_dir_all(vault.join("projects/app")).unwrap();
        assert_eq!(
            env.git(&["status", "--porcelain"]),
            "",
            "a new vault is committed"
        );
        env
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn sbrain(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sbrain"))
            .args(args)
            .current_dir(&self.root)
            .env("SBRAIN_SETTINGS", self.root.join("settings.json"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("SBRAIN_BIN_DIR", self.root.join("bin"))
            .env_remove("SBRAIN_VAULT")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let out = self.sbrain(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.vault())
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn read(&self, w: &Value) -> String {
        std::fs::read_to_string(self.vault().join(w["path"].as_str().unwrap())).unwrap()
    }

    /// Nothing is left uncommitted, and the last commit says `message`.
    fn committed(&self, message: &str) {
        assert_eq!(self.git(&["status", "--porcelain"]), "", "all committed");
        let last = self.git(&["log", "-1", "--format=%s"]);
        assert_eq!(last.trim(), message);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn decisions_are_added_superseded_and_changed() {
    let env = Env::new();
    let old = env.ok(&[
        "decision",
        "add",
        "--project",
        "app",
        "--title",
        "Session in a cookie",
        "--decision",
        "Keep the session in an http-only cookie.",
        "--why",
        "No token in JS.",
        "--commit",
        "abc1234",
    ]);
    assert_eq!(old["id"], "app-D-0001");
    let text = env.read(&old);
    assert!(text.contains("status: accepted"));
    assert!(text.contains("commit: abc1234"));
    assert!(text.contains("## Decision\n\nKeep the session in an http-only cookie."));
    assert!(text.contains("## Why\n\nNo token in JS."));
    env.committed("decision(app): add app-D-0001");

    let new = env.ok(&[
        "decision",
        "add",
        "--project",
        "app",
        "--title",
        "Session in a token",
        "--decision",
        "Use a short-lived token.",
        "--supersedes",
        "app-D-0001",
    ]);
    assert_eq!(new["id"], "app-D-0002");
    assert!(env.read(&new).contains("related: [app-D-0001]"));
    let old_text = env.read(&old);
    assert!(old_text.contains("status: superseded"), "{old_text}");
    assert!(old_text.contains("superseded_by: app-D-0002"));
    env.committed("decision(app): add app-D-0002, superseding app-D-0001");

    env.ok(&["decision", "set", "app-D-0002", "files+=src/auth.rs"]);
    assert!(env.read(&new).contains("files: [src/auth.rs]"));
    env.committed("decision(app): change app-D-0002");

    let bad = env.sbrain(&["decision", "set", "app-D-0002", "status=superseded"]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("superseded_by"));
    let missing = env.sbrain(&[
        "decision",
        "add",
        "--project",
        "app",
        "--title",
        "x",
        "--decision",
        "y",
        "--supersedes",
        "app-D-0042",
    ]);
    assert!(
        !missing.status.success(),
        "nothing is written for a missing decision"
    );
    assert_eq!(env.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_handoff_card_is_written_and_rewritten() {
    let env = Env::new();
    let session = "1a2b3c4d-0000-4000-8000-000000000000";
    let first = env.ok(&[
        "session",
        "write",
        "--project",
        "app",
        "--session",
        session,
        "--date",
        "2026-10-26 14:30",
        "--done",
        "Cached the login query",
        "--done",
        "Wrote its tests",
        "--next",
        "Measure it.",
        "--touched",
        "app-0001",
    ]);
    assert_eq!(
        first["path"],
        "projects/app/sessions/2026-10-26-1430-1a2b3c4d.md"
    );
    let text = env.read(&first);
    assert!(text.contains("source: handoff"));
    assert!(text.contains("## Done\n\n- Cached the login query\n- Wrote its tests\n"));
    assert!(text.contains("## Next step\n\nMeasure it."));
    env.committed("session(app): handoff 1a2b3c4d");

    // Later in the same session: the same card, rewritten.
    let again = env.ok(&[
        "session",
        "write",
        "--project",
        "app",
        "--session",
        session,
        "--date",
        "2026-10-26 16:00",
        "--open",
        "Logout is slow",
    ]);
    assert_eq!(again["path"], first["path"]);
    assert!(env.read(&again).contains("## Open\n\n- Logout is slow"));
    env.committed("session(app): handoff 1a2b3c4d");

    let empty = env.sbrain(&["session", "write", "--project", "app", "--session", session]);
    assert!(!empty.status.success());
    let bad = env.sbrain(&[
        "session",
        "write",
        "--project",
        "app",
        "--session",
        "../x",
        "--done",
        "y",
    ]);
    assert!(!bad.status.success());
}
