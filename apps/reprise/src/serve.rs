//! `reprise serve`: the vault over HTTP on 127.0.0.1, for tools that would rather ask than run the CLI.
//! It is optional: hooks and commands never need it. One request at a time, JSON in and out:
//!
//! - `GET /v1/status`: the vault and reprise's version
//! - `POST /v1/search` `{"project", "queries", "limit"}`: as `reprise search`
//! - `GET /v1/facts?project=<p>[&at=<date>]`: the facts that hold now, or on that day
//! - `GET /v1/timeline?project=<p>[&kind=<k>]`: as `reprise timeline --json`
//! - `POST /v1/hook/<event>`: a Claude Code hook payload; answers what `reprise hook <event>` prints
//!
//! Only `127.0.0.1` is bound, a request must name `127.0.0.1` or `localhost` as its host (a web page
//! cannot reach it through another name), and a POST must be `application/json`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use anyhow::Result;
use serde_json::{Value, json};

use reprise_core::Vault;

/// The largest request body taken.
const MAX_BODY: usize = 1 << 20;

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    host: String,
    json: bool,
    body: Vec<u8>,
}

impl Request {
    fn param(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Serves until the process is stopped. `on_listen` gets the port, once it listens.
pub fn serve(vault: &Vault, port: u16, on_listen: impl FnOnce(u16)) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    on_listen(listener.local_addr()?.port());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let (status, body) = match read(&mut stream) {
            Ok(req) => handle(vault, &req),
            Err(e) => (400, json!({"error": e})),
        };
        let _ = respond(&mut stream, status, &body);
    }
    Ok(())
}

fn read(stream: &mut TcpStream) -> std::result::Result<Request, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("reading the request: {e}"))?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err("not an HTTP request".into());
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect();
    let (mut host, mut json, mut length) = (String::new(), false, 0usize);
    loop {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|e| format!("reading the headers: {e}"))?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => host = value.to_owned(),
            "content-type" => json = value.starts_with("application/json"),
            "content-length" => length = value.parse().map_err(|_| "a bad Content-Length")?,
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Err(format!("the body is over {MAX_BODY} bytes"));
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("reading the body: {e}"))?;
    Ok(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query,
        host,
        json,
        body,
    })
}

/// `%XX` and `+` decoded.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(u8::try_from(h * 16 + l).expect("two hex digits"));
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let text = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        415 => "Unsupported Media Type",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    )
}

fn handle(vault: &Vault, req: &Request) -> (u16, Value) {
    let host = req
        .host
        .rsplit_once(':')
        .map_or(req.host.as_str(), |(h, _)| h);
    if host != "127.0.0.1" && host != "localhost" {
        return (403, json!({"error": "only 127.0.0.1 or localhost"}));
    }
    if req.method == "POST" && !req.json {
        return (
            415,
            json!({"error": "POST needs Content-Type: application/json"}),
        );
    }
    let body: Value = if req.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(e) => return (400, json!({"error": format!("the body is not JSON: {e}")})),
        }
    };
    match route(vault, req, &body) {
        Ok(Some(v)) => (200, v),
        Ok(None) => (
            404,
            json!({"error": format!("no {} {}", req.method, req.path)}),
        ),
        Err(e) => (400, json!({"error": format!("{e:#}")})),
    }
}

fn route(vault: &Vault, req: &Request, body: &Value) -> Result<Option<Value>> {
    let today = chrono::Local::now().date_naive();
    let project = || {
        req.param("project")
            .or_else(|| body["project"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("name the project"))
    };
    Ok(Some(match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/status") => json!({
            "vault": vault.root(),
            "version": env!("CARGO_PKG_VERSION"),
        }),
        ("POST", "/v1/search") => {
            let queries: Vec<String> = body["queries"]
                .as_array()
                .map(|q| {
                    q.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let limit = body["limit"].as_u64().unwrap_or(10) as usize;
            serde_json::to_value(reprise_core::search::search(
                vault,
                &[project()?],
                &queries,
                limit,
            )?)?
        }
        ("GET", "/v1/facts") => {
            let facts = reprise_core::facts::read(vault, Some(&project()?))?;
            let shown = match req.param("at") {
                Some(at) => {
                    let day = reprise_core::dates::resolve(at, today)
                        .ok_or_else(|| anyhow::anyhow!("{at:?} is not a date"))?;
                    reprise_core::facts::on(&facts, day.start)
                }
                None => reprise_core::facts::current(&facts),
            };
            serde_json::to_value(shown)?
        }
        ("GET", "/v1/timeline") => {
            let events = reprise_core::events::read(vault, &project()?)?;
            serde_json::to_value(reprise_core::events::timeline(
                &events,
                req.param("kind"),
                None,
                today,
            ))?
        }
        ("POST", path) if path.starts_with("/v1/hook/") => {
            use reprise_clients::claude::hooks::Event;
            let event = match &path["/v1/hook/".len()..] {
                "session-start" => Event::SessionStart,
                "prompt" => Event::Prompt,
                "session-end" => Event::SessionEnd,
                "tool-use" => Event::ToolUse,
                "pre-compact" => Event::PreCompact,
                _ => return Ok(None),
            };
            let settings = reprise_core::Settings::load()?;
            let out = reprise_clients::claude::hooks::run(
                event,
                &body.to_string(),
                vault,
                settings.access()?,
                today,
            )?;
            out.map_or(Value::Null, |line| {
                serde_json::from_str(&line).unwrap_or(Value::String(line))
            })
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(decode("geçen+ay"), "geçen ay");
        assert_eq!(decode("a%2Fb%20c"), "a/b c");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
