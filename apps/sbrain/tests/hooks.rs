//! `sbrain hook tool-use`: a record the agent wrote to the vault is checked at once.
#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn sbrain(settings: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sbrain"))
        .args(args)
        .env("SBRAIN_SETTINGS", settings)
        .env_remove("SBRAIN_VAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "a hook never fails");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_record_written_wrong_is_reported_to_the_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let settings = root.join("settings.json");
    let vault = root.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    sbrain(
        &settings,
        &["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"],
        "",
    );
    let items = vault.join("projects/app/items");
    std::fs::create_dir_all(&items).unwrap();
    let wrote = |name: &str, text: &str| {
        let path = items.join(name);
        std::fs::write(&path, text).unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "cwd": root,
            "tool_input": {"file_path": path, "content": text}
        });
        sbrain(&settings, &["hook", "tool-use"], &payload.to_string())
    };

    let fine = wrote(
        "app-0001-ok.md",
        "---\nid: app-0001\ntitle: Ok\ntype: bug\nstatus: inbox\n---\n",
    );
    assert_eq!(fine, "", "a correct record gets no answer");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&vault)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(
        git(&["log", "-1", "--format=%s"]),
        "edit(app): projects/app/items/app-0001-ok.md by the agent",
        "and is committed"
    );

    let out = wrote(
        "app-0002-bad.md",
        "---\nid: app-0002\ntitle: Bad\ntype: bug\nstatus: done\n---\n",
    );
    let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(answer["decision"], "block");
    let reason = answer["reason"].as_str().unwrap();
    assert!(
        reason.contains("projects/app/items/app-0002-bad.md"),
        "{reason}"
    );
    assert!(reason.contains("status \"done\" is not valid"), "{reason}");
    assert!(
        git(&["status", "--porcelain"]).contains("app-0002-bad.md"),
        "a wrong record waits for its fix"
    );

    // A file outside the vault's records is none of the hook's business.
    let code = root.join("main.rs");
    std::fs::write(&code, "fn main() {}").unwrap();
    let payload =
        serde_json::json!({"tool_name": "Edit", "cwd": root, "tool_input": {"file_path": code}});
    assert_eq!(
        sbrain(&settings, &["hook", "tool-use"], &payload.to_string()),
        ""
    );
}

/// Hooks run before every prompt, so they stay under `SLOW_HOOK_MS` on a vault of 400 items, 60 of them
/// verified with files in a repository whose history touched those files.
#[test]
fn hooks_stay_fast_on_a_large_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let settings = root.join("settings.json");
    let vault = root.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    sbrain(
        &settings,
        &["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"],
        "",
    );
    let repo = root.join("app");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    for round in 0..3 {
        for f in 0..20 {
            std::fs::write(repo.join(format!("src/f{f}.rs")), format!("{round}")).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-qm", &format!("round {round}")]);
    }
    let items = vault.join("projects/app/items");
    std::fs::create_dir_all(&items).unwrap();
    for n in 1..=400 {
        let (status, extra) = if n <= 60 {
            (
                "verified",
                format!(
                    "files: [src/f{}.rs, src/f{}.rs]\nverify: cargo test\nverified_at: 2000-01-01 00:00\n",
                    n % 20,
                    (n + 1) % 20
                ),
            )
        } else {
            (
                ["inbox", "planned", "doing", "dropped"][n % 4],
                String::new(),
            )
        };
        std::fs::write(
            items.join(format!("app-{n:04}-item-{n}.md")),
            format!(
                "---\nid: app-{n:04}\ntitle: Item {n} about login caching\ntype: bug\nstatus: {status}\n{extra}---\n\n## Problem\n\nThe login query is slow, item {n}.\n"
            ),
        )
        .unwrap();
    }
    // What the doctor last found; without it session start would start one in the background.
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
    std::fs::write(
        vault.join(".sbrain/cache/stale.json"),
        format!(r#"{{"at": "{now}", "items": []}}"#),
    )
    .unwrap();

    let cwd = repo.to_str().unwrap();
    let payloads = [
        (
            "session-start",
            serde_json::json!({"session_id": "s1", "cwd": cwd}),
        ),
        (
            "prompt",
            serde_json::json!({"session_id": "s1", "cwd": cwd, "prompt": "why is the login caching slow"}),
        ),
        (
            "tool-use",
            serde_json::json!({"cwd": cwd, "tool_name": "Edit", "tool_input": {"file_path": items.join("app-0001-item-1.md")}}),
        ),
        (
            "session-end",
            serde_json::json!({"session_id": "s1", "cwd": cwd}),
        ),
    ];
    let limit = sbrain_core::context::SLOW_HOOK_MS;
    for (hook, payload) in payloads {
        let payload = payload.to_string();
        sbrain(&settings, &["hook", hook], &payload);
        let best = (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                sbrain(&settings, &["hook", hook], &payload);
                t.elapsed().as_millis()
            })
            .min()
            .unwrap();
        assert!(best < limit, "{hook} took {best} ms (limit {limit} ms)");
        eprintln!("{hook}: {best} ms");
    }
}
