//! Running an engine CLI with the input on stdin and a time limit.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::clients::{Error, Result};

/// What the CLI left: whether it succeeded, its stdout and its stderr.
pub(crate) struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `cmd` with `input` on stdin; stops it when `timeout` passes. `name` is for errors.
pub(crate) fn run(mut cmd: Command, name: &str, input: &str, timeout: Duration) -> Result<Output> {
    let mut child = cmd
        .env("PATH", crate::clients::full_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Engine(format!("{name} could not be started: {e}")))?;

    // stdin, stdout and stderr each on their own thread, so a full pipe never blocks the others.
    let input = input.to_owned();
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let mut stderr = child.stderr.take().expect("piped");
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| Error::Engine(e.to_string()))? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Engine(format!(
                "no answer within {} seconds",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = writer.join();
    Ok(Output {
        success: status.success(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}
