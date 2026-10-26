//! Distilling with `codex exec`: the instructions and the input as one prompt on stdin, the answer as
//! JSON matching the schema in the last message. Codex has no flag for a system prompt or for running
//! without tools, so it runs in a read-only sandbox, without MCP servers, in the distiller's own folder;
//! it can only answer, and sbrain writes the records.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{DistillRun, Error, Result};

/// Where a run keeps the schema it hands Codex and the answer Codex writes, inside `run.cwd`.
struct Files {
    schema: PathBuf,
    answer: PathBuf,
}

impl Files {
    fn new(cwd: &Path) -> Self {
        let id = std::process::id();
        Self {
            schema: cwd.join(format!("codex-schema-{id}.json")),
            answer: cwd.join(format!("codex-answer-{id}.json")),
        }
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.schema);
        let _ = std::fs::remove_file(&self.answer);
    }
}

/// The arguments for a run, after the binary; the prompt comes on stdin (`-`).
fn args(run: &DistillRun, files: &Files) -> Vec<String> {
    let mut args: Vec<String> = [
        "exec",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--color",
        "never",
        "-c",
        "mcp_servers={}",
        "--output-schema",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(files.schema.to_string_lossy().into_owned());
    args.push("--output-last-message".to_owned());
    args.push(files.answer.to_string_lossy().into_owned());
    args.push("--cd".to_owned());
    args.push(run.cwd.to_string_lossy().into_owned());
    if let Some(model) = run.model.filter(|m| !m.is_empty()) {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    if let Some(effort) = run.effort.filter(|e| !e.is_empty()) {
        // A TOML string, so the level cannot add another setting.
        let quoted = serde_json::to_string(effort).expect("strings serialize");
        args.extend(["-c".to_owned(), format!("model_reasoning_effort={quoted}")]);
    }
    args.push("-".to_owned());
    args
}

/// The instructions, then the session to distill.
fn prompt(run: &DistillRun) -> String {
    format!(
        "{}\n\nAnswer with JSON matching the given schema only.\n\n---\n\n{}",
        run.system.trim_end(),
        run.input
    )
}

/// Runs `bin` and returns the structured answer. Stops it when `run.timeout` passes.
pub fn distill(bin: &Path, run: &DistillRun) -> Result<Value> {
    std::fs::create_dir_all(run.cwd).map_err(crate::error::io(run.cwd))?;
    let files = Files::new(run.cwd);
    files.remove();
    std::fs::write(&files.schema, run.schema).map_err(crate::error::io(&files.schema))?;
    let mut cmd = Command::new(bin);
    cmd.args(args(run, &files)).current_dir(run.cwd);
    let out = crate::process::run(cmd, &bin.display().to_string(), &prompt(run), run.timeout);
    let answer_text = sbrain_core::safe::read_to_string(&files.answer).ok();
    files.remove();
    let out = out?;
    answer(out.success, answer_text.as_deref(), &out.stderr)
}

/// The JSON in Codex's last message, or why there is none.
fn answer(success: bool, last: Option<&str>, stderr: &str) -> Result<Value> {
    let clip = |t: &str| sbrain_core::text::clip(t.trim(), 400);
    if !success {
        return Err(Error::Engine(format!("codex failed: {}", clip(stderr))));
    }
    let Some(last) = last.map(str::trim).filter(|l| !l.is_empty()) else {
        return Err(Error::Engine(format!(
            "codex left no answer: {}",
            clip(stderr)
        )));
    };
    // A model may still wrap the JSON in a code fence.
    let body = last
        .strip_prefix("```json")
        .or_else(|| last.strip_prefix("```"))
        .and_then(|b| b.strip_suffix("```"))
        .unwrap_or(last);
    serde_json::from_str::<Value>(body.trim())
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| Error::Engine(format!("codex did not answer with JSON: {}", clip(last))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn run<'a>(cwd: &'a Path) -> DistillRun<'a> {
        DistillRun {
            system: "sys",
            input: "in",
            schema: r#"{"type":"object"}"#,
            model: Some("gpt-5-codex"),
            effort: Some("high"),
            cwd,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn arguments_keep_codex_to_answering() {
        let cwd = Path::new("/x");
        let files = Files::new(cwd);
        let a = args(&run(cwd), &files);
        assert_eq!(a[0], "exec");
        assert_eq!(
            a.last().map(String::as_str),
            Some("-"),
            "the prompt comes on stdin"
        );
        let pair = |k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
        assert!(pair("--sandbox", "read-only"));
        assert!(pair("-c", "mcp_servers={}"));
        assert!(pair("--cd", "/x"));
        assert!(pair("--model", "gpt-5-codex"));
        assert!(pair("-c", "model_reasoning_effort=\"high\""));
        assert!(pair("--output-schema", &files.schema.to_string_lossy()));
        assert!(pair(
            "--output-last-message",
            &files.answer.to_string_lossy()
        ));
        assert!(a.contains(&"--skip-git-repo-check".to_owned()));

        let mut sly = run(cwd);
        sly.effort = Some("high\"\nsandbox_mode=\"danger-full-access");
        let a = args(&sly, &files);
        let effort = a
            .iter()
            .find(|x| x.starts_with("model_reasoning_effort="))
            .unwrap();
        assert!(!effort.contains('\n'), "{effort}");
    }

    #[test]
    fn the_prompt_holds_the_instructions_then_the_input() {
        let p = prompt(&run(Path::new("/x")));
        assert!(p.starts_with("sys\n"));
        assert!(p.ends_with("---\n\nin"));
    }

    #[test]
    fn reads_the_answer() {
        assert_eq!(answer(true, Some(r#"{"a":1}"#), "").unwrap()["a"], 1);
        assert_eq!(
            answer(true, Some("```json\n{\"a\":2}\n```"), "").unwrap()["a"],
            2
        );
        let err = |r: Result<Value>| r.unwrap_err().to_string();
        assert!(err(answer(false, None, "Not logged in")).contains("Not logged in"));
        assert!(err(answer(true, None, "")).contains("no answer"));
        assert!(err(answer(true, Some("I cannot"), "")).contains("did not answer with JSON"));
        assert!(answer(true, Some("[1]"), "").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_cli_and_cleans_up() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        // Writes the prompt it got and the schema's text into the answer file it was told about.
        let script = tmp.path().join("codex.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
prompt=$(cat)
while [ $# -gt 0 ]; do
  case "$1" in
    --output-schema) schema=$(cat "$2"); shift ;;
    --output-last-message) out="$2"; shift ;;
  esac
  shift
done
ends=false
case "$prompt" in *in) ends=true ;; esac
printf '{"prompt_ends_with_in":%s,"schema":%s}' "$ends" "$schema" > "$out"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cwd = tmp.path().join("distill");
        let got = distill(&script, &run(&cwd)).unwrap();
        assert_eq!(got["prompt_ends_with_in"], true);
        assert_eq!(got["schema"]["type"], "object");
        assert_eq!(
            std::fs::read_dir(&cwd).unwrap().count(),
            0,
            "the schema and answer files are removed"
        );
        assert!(distill(&tmp.path().join("missing"), &run(&cwd)).is_err());
    }
}
