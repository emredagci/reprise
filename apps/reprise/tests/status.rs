//! `reprise status`: what reprise does on this computer, and what needs the user, without changing it.
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
        std::fs::create_dir_all(root.join("vault")).unwrap();
        Self { _tmp: tmp, root }
    }

    fn reprise(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_reprise"))
            .args(args)
            .env("REPRISE_SETTINGS", self.root.join("settings.json"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("REPRISE_BIN_DIR", self.root.join("bin"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env_remove("REPRISE_VAULT")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.reprise(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn status(&self) -> Value {
        serde_json::from_slice(&self.ok(&["status", "--json"]).stdout).unwrap()
    }

    fn attention(&self) -> Vec<String> {
        self.status()["attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    }

    fn manifest(&self) -> PathBuf {
        self.root.join("connections/claude.json")
    }
}

fn claude(status: &Value) -> &Value {
    status["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["client"] == "claude")
        .unwrap()
}

#[test]
fn a_new_computer_is_told_what_to_set_up() {
    let env = Env::new();
    let s = env.status();
    assert!(s["vault"].is_null());
    let attention = env.attention();
    assert!(
        attention.iter().any(|l| l.contains("reprise setup set")),
        "{attention:?}"
    );
    assert!(
        attention
            .iter()
            .any(|l| l.contains("reprise connect claude"))
    );
    let text = String::from_utf8_lossy(&env.ok(&["status"]).stdout).into_owned();
    assert!(text.contains("vault     not chosen"), "{text}");
    assert!(text.contains("Needs you"));
}

#[test]
fn a_set_up_computer_needs_nothing_and_shows_where_the_hooks_point() {
    let env = Env::new();
    let vault = env.root.join("vault");
    env.ok(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"]);
    env.ok(&["connect", "claude", "--yes"]);
    let s = env.status();
    assert_eq!(s["vault"]["path"], vault.to_str().unwrap());
    assert_eq!(s["vault"]["git"], "root");
    let c = claude(&s);
    assert_eq!(c["connected"], true);
    assert_eq!(c["hooks_run_this"], true, "{c}");
    assert_eq!(c["swept"], true);
    assert_eq!(s["schedule"]["installed"], false);
    assert!(s["sweep"]["since"].is_null(), "never swept");
    assert_eq!(
        s["attention"],
        serde_json::json!([]),
        "nothing needs the user"
    );
    let text = String::from_utf8_lossy(&env.ok(&["status"]).stdout).into_owned();
    assert!(text.contains("(this reprise)"), "{text}");
    assert!(text.contains("Nothing needs you."));

    // Hooks that run an reprise that is gone are reported, and status leaves them as they are.
    let mut m: Value =
        serde_json::from_str(&std::fs::read_to_string(env.manifest()).unwrap()).unwrap();
    m["reprise"] = env.root.join("gone/reprise").to_string_lossy().into();
    let before = serde_json::to_string(&m).unwrap();
    std::fs::write(env.manifest(), &before).unwrap();
    let attention = env.attention();
    assert!(
        attention.iter().any(|l| l.contains("which is gone")),
        "{attention:?}"
    );
    assert_eq!(
        std::fs::read_to_string(env.manifest()).unwrap(),
        before,
        "status repairs nothing"
    );
}

#[test]
fn work_given_up_and_waiting_rules_need_the_user() {
    let env = Env::new();
    let vault = env.root.join("vault");
    env.ok(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"]);
    env.ok(&["connect", "claude", "--yes"]);
    let ledger = reprise_core::ledger::Ledger::open_root(&vault).unwrap();
    let payload = serde_json::json!({});
    for _ in 0..reprise_core::ledger::MAX_ATTEMPTS {
        ledger
            .fail("distill", "1a2b3c4d", &payload, "claude failed", 0)
            .unwrap();
    }
    ledger.put("ended", "5e6f7a8b", &payload, 0).unwrap();
    std::fs::create_dir_all(vault.join("projects/app")).unwrap();
    std::fs::write(
        vault.join("projects/app/proposed-rules.md"),
        "- 2026-10-26 (1a2b3c4d): Measure before caching.\n",
    )
    .unwrap();

    let s = env.status();
    let distill = s["ledger"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kind"] == "distill")
        .unwrap();
    assert_eq!(distill["gave_up"][0]["key"], "1a2b3c4d");
    assert_eq!(distill["gave_up"][0]["last_error"], "claude failed");
    let ended = s["ledger"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kind"] == "ended")
        .unwrap();
    assert_eq!(ended["pending"], 1);
    let attention = env.attention();
    assert!(
        attention.iter().any(|l| l.contains("reprise sweep retry")),
        "{attention:?}"
    );
    assert!(attention.iter().any(|l| l.contains("reprise rule list")));
    let text = String::from_utf8_lossy(&env.ok(&["status"]).stdout).into_owned();
    assert!(text.contains("distilling: 0 waiting, 1 given up"), "{text}");
}
