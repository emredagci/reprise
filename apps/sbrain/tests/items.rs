//! `sbrain item verify` end to end: the CLI runs the command, prints the result and exits with its verdict.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
        let vault = env.root.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let out = env.sbrain(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"]);
        assert!(out.status.success(), "{}", stderr(&out));
        std::fs::create_dir_all(vault.join("projects/app")).unwrap();
        std::fs::create_dir_all(env.repo()).unwrap();
        env
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn sbrain(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sbrain"))
            .args(args)
            .env("SBRAIN_SETTINGS", self.root.join("settings.json"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("SBRAIN_BIN_DIR", self.root.join("bin"))
            .env_remove("SBRAIN_VAULT")
            .output()
            .unwrap()
    }

    /// A done-unverified item with this verify command; its id.
    fn item(&self, verify: &str) -> String {
        let out = self.sbrain(&[
            "item",
            "add",
            "--project",
            "app",
            "--title",
            "Cache the login query",
            "--status",
            "done-unverified",
        ]);
        assert!(out.status.success(), "{}", stderr(&out));
        let id = json(&out)["id"].as_str().unwrap().to_owned();
        let set = format!("verify={verify}");
        assert!(self.sbrain(&["item", "set", &id, &set]).status.success());
        id
    }

    fn verify(&self, id: &str) -> Output {
        self.sbrain(&[
            "item",
            "verify",
            id,
            "--repo",
            self.repo().to_str().unwrap(),
        ])
    }

    fn record(&self, id: &str) -> String {
        let items = self.root.join("vault/projects/app/items");
        let file = std::fs::read_dir(items)
            .unwrap()
            .flatten()
            .find(|f| {
                f.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{id}-"))
            })
            .unwrap();
        std::fs::read_to_string(file.path()).unwrap()
    }
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "").unwrap();
}

#[test]
fn verify_passes_and_fails_with_the_command() {
    let env = Env::new();
    let id = env.item("test -f built");

    let failed = env.verify(&id);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "a failing command fails the CLI"
    );
    let result = json(&failed);
    assert_eq!(result["passed"], false);
    assert_eq!(result["exit"], 1);
    assert_eq!(result["status"], "done-unverified");

    touch(&env.repo(), "built");
    let passed = env.verify(&id);
    assert!(passed.status.success(), "{}", stderr(&passed));
    let result = json(&passed);
    assert_eq!(result["passed"], true);
    assert_eq!(result["status"], "verified");
    let record = env.record(&id);
    assert!(record.contains("status: verified"), "{record}");
    assert!(record.contains("`test -f built` failed, exit 1"));
    assert!(record.contains("`test -f built` passed"));

    // Only a passing verify sets it.
    let by_hand = env.sbrain(&["item", "set", &id, "status=verified"]);
    assert!(!by_hand.status.success());

    // What sbrain wrote is in the vault's history, one commit each.
    let vault = env.root.join("vault");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&vault)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let log = git(&["log", "--format=%s"]);
    for line in [
        format!("item(app): add {id}"),
        format!("item(app): change {id}"),
        format!("item: verify {id} (failed)"),
        format!("item: verify {id} (passed)"),
    ] {
        assert!(log.contains(&line), "{line} in\n{log}");
    }
    assert_eq!(
        git(&["status", "--porcelain"]),
        "",
        "nothing left uncommitted"
    );
}

#[test]
fn verify_needs_a_repository() {
    let env = Env::new();
    let id = env.item("true");
    let out = env.sbrain(&["item", "verify", &id]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("pass --repo"), "{}", stderr(&out));
    assert!(env.record(&id).contains("status: done-unverified"));
}

#[test]
fn rules_wait_until_accepted_and_each_step_is_committed() {
    let env = Env::new();
    let vault = env.root.join("vault");
    std::fs::write(
        vault.join("projects/app/proposed-rules.md"),
        "# Proposed rules\n\n- 2026-10-26 (1a2b3c4d): Measure before caching.\n",
    )
    .unwrap();
    let listed = json(&env.sbrain(&["rule", "list"]));
    let id = listed[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(listed[0]["project"], "app");
    assert_eq!(listed[0]["rule"], "Measure before caching.");

    let accepted = env.sbrain(&["rule", "accept", &id]);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
    let gotchas = std::fs::read_to_string(vault.join("projects/app/gotchas.md")).unwrap();
    assert!(gotchas.contains(": Measure before caching."), "{gotchas}");
    assert_eq!(json(&env.sbrain(&["rule", "list"])), serde_json::json!([]));
    let log = Command::new("git")
        .arg("-C")
        .arg(&vault)
        .args(["log", "-1", "--format=%s"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        format!("rule: accept {id}")
    );

    assert!(
        !env.sbrain(&["rule", "accept", &id]).status.success(),
        "accepted once"
    );
}
