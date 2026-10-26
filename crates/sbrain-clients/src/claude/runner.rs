//! Distilling with `claude -p`: the instructions as the system prompt, the input on stdin, the answer as
//! JSON matching the schema. The CLI runs without tools, MCP servers, user settings (so no hooks) or a
//! saved session, so it can only answer; sbrain writes the records.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::{DistillRun, Error, Result};

/// The arguments for a run, after the binary.
pub fn args(run: &DistillRun) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--json-schema",
        run.schema,
        "--system-prompt",
        run.system,
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "project",
        "--no-session-persistence",
    ]
    .map(str::to_owned)
    .to_vec();
    if let Some(model) = run.model.filter(|m| !m.is_empty()) {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    if let Some(effort) = run.effort.filter(|e| !e.is_empty()) {
        args.extend(["--effort".to_owned(), effort.to_owned()]);
    }
    args
}

/// Runs `bin` and returns the structured answer. Stops it when `run.timeout` passes.
pub fn distill(bin: &Path, run: &DistillRun) -> Result<Value> {
    std::fs::create_dir_all(run.cwd).map_err(crate::error::io(run.cwd))?;
    let mut cmd = Command::new(bin);
    cmd.args(args(run)).current_dir(run.cwd);
    let out = crate::process::run(cmd, &bin.display().to_string(), run.input, run.timeout)?;
    answer(out.success, &out.stdout, &out.stderr)
}

/// The structured answer in the CLI's JSON result, or why there is none.
fn answer(success: bool, stdout: &str, stderr: &str) -> Result<Value> {
    let clip = |t: &str| sbrain_core::text::clip(t, 400);
    let Ok(result) = serde_json::from_str::<Value>(stdout.trim()) else {
        let said = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(Error::Engine(format!(
            "claude did not answer with JSON: {}",
            clip(said)
        )));
    };
    if !success || result["is_error"].as_bool() == Some(true) {
        let said = result["result"].as_str().unwrap_or(stderr);
        return Err(Error::Engine(format!("claude failed: {}", clip(said))));
    }
    match &result["structured_output"] {
        Value::Object(_) => Ok(result["structured_output"].clone()),
        // Without `structured_output`, the JSON is read from `result`.
        _ => result["result"]
            .as_str()
            .and_then(|r| serde_json::from_str::<Value>(r).ok())
            .filter(Value::is_object)
            .ok_or_else(|| Error::Engine("claude's answer has no structured output".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn run<'a>(cwd: &'a Path) -> DistillRun<'a> {
        DistillRun {
            system: "sys",
            input: "in",
            schema: "{}",
            model: Some("sonnet"),
            effort: None,
            cwd,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn arguments_shut_out_tools_and_settings() {
        let a = args(&run(Path::new("/x")));
        let joined = a.join(" ");
        assert!(joined.starts_with("-p --output-format json --json-schema {} --system-prompt sys"));
        assert!(a.windows(2).any(|w| w == ["--tools", ""]));
        assert!(a.windows(2).any(|w| w == ["--setting-sources", "project"]));
        assert!(a.contains(&"--no-session-persistence".to_owned()));
        assert!(a.windows(2).any(|w| w == ["--model", "sonnet"]));
        assert!(!a.contains(&"--effort".to_owned()));
    }

    #[test]
    fn reads_the_answer() {
        let ok = r#"{"is_error":false,"result":"x","structured_output":{"a":1}}"#;
        assert_eq!(answer(true, ok, "").unwrap()["a"], 1);
        let in_result = r#"{"is_error":false,"result":"{\"a\":2}"}"#;
        assert_eq!(answer(true, in_result, "").unwrap()["a"], 2);
        let failed = r#"{"is_error":true,"result":"Credit balance is too low"}"#;
        assert!(
            answer(true, failed, "")
                .unwrap_err()
                .to_string()
                .contains("Credit balance")
        );
        assert!(
            answer(false, "", "not logged in")
                .unwrap_err()
                .to_string()
                .contains("not logged in")
        );
        assert!(answer(true, r#"{"is_error":false,"result":"plain"}"#, "").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_cli_and_stops_a_slow_one() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let p = tmp.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        // Echoes its stdin back inside the answer, so the input is known to arrive.
        let echo = script(
            "echo.sh",
            r#"input=$(cat); printf '{"is_error":false,"structured_output":{"got":"%s"}}' "$input""#,
        );
        let cwd = tmp.path().join("distill");
        assert_eq!(distill(&echo, &run(&cwd)).unwrap()["got"], "in");
        assert!(cwd.is_dir());

        let slow = script("slow.sh", "sleep 5");
        let mut quick = run(&cwd);
        quick.timeout = Duration::from_millis(300);
        let err = distill(&slow, &quick).unwrap_err().to_string();
        assert!(err.contains("no answer within"), "{err}");
        assert!(distill(&tmp.path().join("missing"), &run(&cwd)).is_err());
    }
}
