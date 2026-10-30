//! `reprise serve`: the vault over HTTP on 127.0.0.1.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

fn request(port: u16, head: &str, body: &str) -> (u16, serde_json::Value) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "{head}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out[9..12].parse().unwrap();
    let json = out.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(json).unwrap())
}

#[test]
fn the_vault_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let settings = root.join("settings.json");
    let vault = root.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let reprise = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_reprise"))
            .args(args)
            .env("REPRISE_SETTINGS", &settings)
            .env_remove("REPRISE_VAULT")
            .output()
            .unwrap()
    };
    assert!(
        reprise(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"])
            .status
            .success()
    );
    std::fs::create_dir_all(vault.join("projects/app")).unwrap();
    std::fs::write(
        vault.join("projects/app/project.toml"),
        "prefix = \"app\"\n",
    )
    .unwrap();
    assert!(
        reprise(&[
            "fact",
            "set",
            "--project",
            "app",
            "port",
            "9090",
            "--from",
            "2026-10-01"
        ])
        .status
        .success()
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_reprise"))
        .args(["serve", "--port", "0"])
        .env("REPRISE_SETTINGS", &settings)
        .env_remove("REPRISE_VAULT")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().rsplit(':').next().unwrap().parse().unwrap();
    let host = "Host: 127.0.0.1";

    let (status, v) = request(port, &format!("GET /v1/status HTTP/1.1\r\n{host}"), "");
    assert_eq!(status, 200);
    assert!(v["vault"].as_str().unwrap().ends_with("/vault"));

    let (status, v) = request(
        port,
        &format!("GET /v1/facts?project=app HTTP/1.1\r\n{host}"),
        "",
    );
    assert_eq!(status, 200, "{v}");
    assert_eq!(v[0]["value"], "9090");
    let (_, v) = request(
        port,
        &format!("GET /v1/facts?project=app&at=2026-09-01 HTTP/1.1\r\n{host}"),
        "",
    );
    assert_eq!(v, serde_json::json!([]));

    let body = r#"{"project": "app", "queries": ["port"]}"#;
    let (status, v) = request(
        port,
        &format!("POST /v1/search HTTP/1.1\r\n{host}\r\nContent-Type: application/json"),
        body,
    );
    assert_eq!(status, 200, "{v}");
    assert!(v["hits"].is_array());

    let (status, _) = request(port, "GET /v1/status HTTP/1.1\r\nHost: evil.example", "");
    assert_eq!(status, 403, "another host name is refused");
    let (status, _) = request(port, &format!("POST /v1/search HTTP/1.1\r\n{host}"), body);
    assert_eq!(status, 415, "a POST must be JSON");
    let (status, _) = request(port, &format!("GET /v1/nothing HTTP/1.1\r\n{host}"), "");
    assert_eq!(status, 404);
    let (status, v) = request(port, &format!("GET /v1/facts HTTP/1.1\r\n{host}"), "");
    assert_eq!(status, 400, "{v}");

    child.kill().unwrap();
    let _ = child.wait();
}
