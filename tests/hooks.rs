//! `reprise hook …`: a record the agent wrote to the vault is checked at once and its secrets masked;
//! the user's messages are kept through a compaction.
#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn reprise(settings: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reprise"))
        .args(args)
        .env("REPRISE_SETTINGS", settings)
        .env_remove("REPRISE_VAULT")
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
    reprise(
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
        reprise(&settings, &["hook", "tool-use"], &payload.to_string())
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

    // A secret written into the vault is masked before the commit, and the agent is told.
    let key = format!("sk-ant-api03-{}", "Kd8".repeat(12));
    let out = wrote(
        "app-0003-key.md",
        &format!("---\nid: app-0003\ntitle: Key\ntype: bug\nstatus: inbox\n---\n\nUse {key}\n"),
    );
    let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let reason = answer["reason"].as_str().unwrap();
    assert!(reason.contains("Anthropic key"), "{reason}");
    let written = std::fs::read_to_string(items.join("app-0003-key.md")).unwrap();
    assert!(
        written.contains("Use [redacted]") && !written.contains(&key),
        "{written}"
    );
    assert!(
        git(&["log", "-1", "-p"]).contains("Use [redacted]"),
        "the masked file is what is committed"
    );

    // A file outside the vault's records is none of the hook's business.
    let code = root.join("main.rs");
    std::fs::write(&code, "fn main() {}").unwrap();
    let payload =
        serde_json::json!({"tool_name": "Edit", "cwd": root, "tool_input": {"file_path": code}});
    assert_eq!(
        reprise(&settings, &["hook", "tool-use"], &payload.to_string()),
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
    reprise(
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
        vault.join(".reprise/cache/stale.json"),
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
    let limit = reprise::context::SLOW_HOOK_MS;
    for (hook, payload) in payloads {
        let payload = payload.to_string();
        reprise(&settings, &["hook", hook], &payload);
        let best = (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                reprise(&settings, &["hook", hook], &payload);
                t.elapsed().as_millis()
            })
            .min()
            .unwrap();
        assert!(best < limit, "{hook} took {best} ms (limit {limit} ms)");
        eprintln!("{hook}: {best} ms");
    }
}

#[test]
fn the_users_words_come_back_after_a_compaction() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let settings = root.join("settings.json");
    let vault = root.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    reprise(
        &settings,
        &["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"],
        "",
    );
    let transcript = root.join("t.jsonl");
    let lines: Vec<String> = [
        "bundan sonra migration yazma, format bozulabilir",
        "ok",
        "now add the PreCompact hook",
    ]
    .iter()
    .enumerate()
    .map(|(i, m)| {
        serde_json::json!({
            "type": "user",
            "cwd": root,
            "timestamp": format!("2026-10-30T09:{i:02}:00Z"),
            "message": {"content": m}
        })
        .to_string()
    })
    .collect();
    std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();

    let compact = serde_json::json!({
        "hook_event_name": "PreCompact",
        "session_id": "abc-123",
        "transcript_path": transcript,
        "cwd": root,
        "trigger": "auto"
    });
    assert_eq!(
        reprise(&settings, &["hook", "pre-compact"], &compact.to_string()),
        "",
        "PreCompact answers nothing"
    );

    let start = |source: &str| {
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "abc-123",
            "source": source,
            "cwd": root
        });
        let out = reprise(&settings, &["hook", "session-start"], &payload.to_string());
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let after = start("compact");
    assert!(after.contains("## Kept through compaction"), "{after}");
    assert!(
        after.contains("bundan sonra migration yazma, format bozulabilir"),
        "{after}"
    );
    assert!(after.contains("now add the PreCompact hook"), "{after}");
    assert!(after.chars().count() < 10_000, "within a hook's limit");
    assert!(
        !start("startup").contains("Kept through compaction"),
        "only a session start after a compaction gets them"
    );

    // Every prompt after it carries the instructions among them.
    let prompt = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "abc-123",
        "prompt": "devam edelim",
        "cwd": root
    });
    let out = reprise(&settings, &["hook", "prompt"], &prompt.to_string());
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("Still in effect"), "{ctx}");
    assert!(ctx.contains("bundan sonra migration yazma"), "{ctx}");
    assert!(!ctx.contains("now add the PreCompact hook"), "{ctx}");

    // The status line says what reprise last did in the session.
    let status = serde_json::json!({"session_id": "abc-123", "workspace": {"current_dir": root}});
    let line = reprise(&settings, &["statusline"], &status.to_string());
    assert!(
        line.starts_with("reprise · no project · ") && line.trim().ends_with("(just now)"),
        "{line}"
    );
    let other = serde_json::json!({"session_id": "other-1"});
    assert_eq!(
        reprise(&settings, &["statusline"], &other.to_string()).trim(),
        "reprise · no project"
    );
}
