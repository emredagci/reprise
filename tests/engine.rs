//! `reprise engine`: choosing the coding CLI, model and effort that run reprise's own work.

use std::process::{Command, Output};

use serde_json::Value;

fn reprise(settings: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reprise"))
        .args(args)
        .env("REPRISE_SETTINGS", settings)
        .env_remove("REPRISE_VAULT")
        .output()
        .unwrap()
}

fn json(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn choose_the_engine_model_and_effort() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");

    let shown = json(&reprise(&settings, &["engine"]));
    assert_eq!(shown["client"], "claude", "Claude Code when none is chosen");
    assert!(shown["model"].is_null());
    let claude = shown["choices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["client"] == "claude")
        .unwrap();
    assert!(
        claude["models"]
            .as_array()
            .unwrap()
            .contains(&"opus".into())
    );
    let effort = claude["efforts"][0].as_str().unwrap().to_owned();

    let set = json(&reprise(
        &settings,
        &[
            "engine", "set", "claude", "--model", " opus ", "--effort", &effort,
        ],
    ));
    assert_eq!(set["model"], "opus");
    assert_eq!(set["effort"], effort.as_str());
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(saved["engine"]["model"], "opus");

    let cleared = json(&reprise(
        &settings,
        &["engine", "set", "claude", "--model", ""],
    ));
    assert!(
        cleared["model"].is_null(),
        "an empty model is the CLI's default"
    );

    let bad = reprise(
        &settings,
        &["engine", "set", "claude", "--effort", "enormous"],
    );
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("takes no effort"));
    let codex = json(&reprise(
        &settings,
        &["engine", "set", "codex", "--effort", "minimal"],
    ));
    assert_eq!(codex["client"], "codex", "Codex can distill");
    let bad = reprise(&settings, &["engine", "set", "codex", "--effort", "max"]);
    assert!(!bad.status.success(), "Codex's levels are its own");
    let unknown = reprise(&settings, &["engine", "set", "nope"]);
    assert!(!unknown.status.success());
}
