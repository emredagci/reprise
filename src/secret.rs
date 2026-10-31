//! Finding and masking secrets in text before it reaches the engine or the vault: API keys and
//! tokens by their known shapes, private key blocks, JSON Web Tokens, passwords in URLs, bearer
//! tokens, and values assigned to names like `API_KEY`, `password` or `clientSecret`.
//!
//! Masking errs toward hiding: a vault is pushed to a remote, and a masked line costs less than a
//! leaked key. Commit hashes, ids and ordinary code stay as they are.

use std::borrow::Cow;

/// What a masked secret is replaced with.
pub const MASK: &str = "[redacted]";

/// A secret found in a text: byte range and what it looked like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
}

/// Prefix, the shortest whole token with it, and what it is.
const PREFIXES: &[(&str, usize, &str)] = &[
    ("sk-ant-", 30, "Anthropic key"),
    ("sk-proj-", 30, "OpenAI key"),
    ("sk-or-", 30, "OpenRouter key"),
    ("sk-", 24, "API key"),
    ("sk_live_", 20, "Stripe key"),
    ("sk_test_", 20, "Stripe key"),
    ("rk_live_", 20, "Stripe key"),
    ("rk_test_", 20, "Stripe key"),
    ("whsec_", 20, "webhook secret"),
    ("github_pat_", 40, "GitHub token"),
    ("ghp_", 36, "GitHub token"),
    ("gho_", 36, "GitHub token"),
    ("ghu_", 36, "GitHub token"),
    ("ghs_", 36, "GitHub token"),
    ("ghr_", 36, "GitHub token"),
    ("glpat-", 24, "GitLab token"),
    ("xoxb-", 20, "Slack token"),
    ("xoxp-", 20, "Slack token"),
    ("xoxa-", 20, "Slack token"),
    ("xoxr-", 20, "Slack token"),
    ("xoxs-", 20, "Slack token"),
    ("xapp-", 20, "Slack token"),
    ("npm_", 40, "npm token"),
    ("pypi-", 50, "PyPI token"),
    ("hf_", 30, "Hugging Face token"),
    ("dop_v1_", 40, "DigitalOcean token"),
    ("shpat_", 30, "Shopify token"),
    ("shpss_", 30, "Shopify token"),
    ("sbp_", 40, "Supabase token"),
    ("lin_api_", 40, "Linear key"),
    ("glsa_", 40, "Grafana token"),
];

/// Words in a name that say its value is a secret, matched on the name lowercased with `-` and `.`
/// as `_`. `token` alone also matches counters like `max_tokens`; their values are numbers, which
/// are never taken.
const SECRET_NAMES: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "passphrase",
    "api_key",
    "apikey",
    "private_key",
    "privatekey",
    "access_key",
    "accesskey",
    "auth_key",
    "credential",
    "session_key",
];

/// Values that stand for a secret without being one.
const PLACEHOLDERS: &[&str] = &[
    "redacted",
    "example",
    "placeholder",
    "changeme",
    "your_",
    "your-",
    "<",
    "xxxx",
    "****",
    "....",
];

/// Every secret in `text`, in order, without overlaps.
pub fn find(text: &str) -> Vec<Found> {
    let mut found = Vec::new();
    private_keys(text, &mut found);
    shaped_tokens(text, &mut found);
    json_web_tokens(text, &mut found);
    url_passwords(text, &mut found);
    bearer_tokens(text, &mut found);
    assignments(text, &mut found);
    found.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
    let mut merged: Vec<Found> = Vec::new();
    for f in found {
        match merged.last_mut() {
            Some(last) if f.start < last.end => last.end = last.end.max(f.end),
            _ => merged.push(f),
        }
    }
    merged
}

/// Whether `text` holds anything that looks like a secret.
pub fn contains(text: &str) -> bool {
    !find(text).is_empty()
}

/// `text` with every secret replaced by [`MASK`]; borrowed when there was none.
pub fn redact(text: &str) -> Cow<'_, str> {
    let found = find(text);
    if found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for f in found {
        out.push_str(&text[at..f.start]);
        out.push_str(MASK);
        at = f.end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// What kinds of secret `text` holds, each once, in order of appearance.
pub fn kinds(text: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for f in find(text) {
        if !out.contains(&f.kind) {
            out.push(f.kind);
        }
    }
    out
}

/// `-----BEGIN <label> PRIVATE KEY-----` up to its END line. Without one, the block is the base64
/// lines that follow the header.
fn private_keys(text: &str, out: &mut Vec<Found>) {
    let mut from = 0;
    while let Some(pos) = text[from..].find("-----BEGIN ") {
        let start = from + pos;
        let Some(head_len) = text[start + 11..].find("-----") else {
            break;
        };
        let label = &text[start + 11..start + 11 + head_len];
        let after_head = start + 11 + head_len + 5;
        let key_label = label.ends_with("PRIVATE KEY")
            && label.bytes().all(|b| b.is_ascii_uppercase() || b == b' ');
        if !key_label {
            from = after_head;
            continue;
        }
        let end = text[after_head..]
            .find("-----END ")
            .and_then(|p| {
                let tail = after_head + p + 9;
                text[tail..].find("-----").map(|q| tail + q + 5)
            })
            .unwrap_or_else(|| base64_lines_end(text, after_head));
        out.push(Found {
            start,
            end,
            kind: "private key",
        });
        from = end;
    }
}

/// Where the base64 lines starting at `from` end; header lines like `Proc-Type: …` belong to them.
fn base64_lines_end(text: &str, from: usize) -> usize {
    let mut end = from;
    let mut at = from;
    for line in text[from..].split_inclusive('\n') {
        let body = line.trim();
        let base64 = !body.is_empty()
            && body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b));
        if at > from && !(base64 || body.contains(": ")) {
            break;
        }
        at += line.len();
        if base64 {
            end = at - (line.len() - line.trim_end().len());
        }
    }
    end
}

/// Runs of the characters tokens are made of, as byte ranges.
fn runs(text: &str, part_of: impl Fn(u8) -> bool) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if part_of(bytes[i]) {
            let start = i;
            while i < bytes.len() && part_of(bytes[i]) {
                i += 1;
            }
            out.push((start, i));
        } else {
            i += 1;
        }
    }
    out
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn has_digit_and_letter(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit()) && s.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Keys and tokens recognised by their prefix or fixed shape.
fn shaped_tokens(text: &str, out: &mut Vec<Found>) {
    for (start, end) in runs(text, is_token_byte) {
        let word = &text[start..end];
        let kind = PREFIXES
            .iter()
            .find(|(prefix, min, _)| {
                word.starts_with(prefix)
                    && word.len() >= *min
                    && has_digit_and_letter(&word[prefix.len()..])
            })
            .map(|(_, _, kind)| *kind)
            .or_else(|| {
                let aws = (word.starts_with("AKIA") || word.starts_with("ASIA"))
                    && word.len() == 20
                    && word
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
                aws.then_some("AWS access key")
            })
            .or_else(|| {
                (word.starts_with("AIza") && word.len() == 39 && has_digit_and_letter(word))
                    .then_some("Google API key")
            });
        if let Some(kind) = kind {
            out.push(Found { start, end, kind });
        }
    }
}

/// `eyJ….….…`: three base64url parts, the first a JSON header.
fn json_web_tokens(text: &str, out: &mut Vec<Found>) {
    for (start, end) in runs(text, |b| is_token_byte(b) || b == b'.') {
        let word = text[start..end].trim_end_matches('.');
        let parts: Vec<&str> = word.split('.').collect();
        if word.starts_with("eyJ") && parts.len() == 3 && parts.iter().all(|p| p.len() >= 10) {
            out.push(Found {
                start,
                end: start + word.len(),
                kind: "JSON Web Token",
            });
        }
    }
}

/// The password of `scheme://user:password@host`.
fn url_passwords(text: &str, out: &mut Vec<Found>) {
    let mut from = 0;
    while let Some(pos) = text[from..].find("://") {
        let auth_start = from + pos + 3;
        from = auth_start;
        let auth_end = text[auth_start..]
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"' || c == '\''
            })
            .map_or(text.len(), |p| auth_start + p);
        let auth = &text[auth_start..auth_end];
        let Some(at) = auth.rfind('@') else { continue };
        let Some(colon) = auth[..at].find(':') else {
            continue;
        };
        let password = &auth[colon + 1..at];
        if password.is_empty() || is_placeholder(password) {
            continue;
        }
        out.push(Found {
            start: auth_start + colon + 1,
            end: auth_start + at,
            kind: "password in a URL",
        });
    }
}

/// `Bearer <token>` and `Basic <credentials>` as in an Authorization header.
fn bearer_tokens(text: &str, out: &mut Vec<Found>) {
    for (scheme, min) in [("Bearer ", 16), ("Basic ", 12)] {
        let mut from = 0;
        while let Some(pos) = text[from..].find(scheme) {
            let start = from + pos + scheme.len();
            let end = text[start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-._~+/=".contains(c)))
                .map_or(text.len(), |p| start + p);
            from = start;
            let token = &text[start..end];
            if token.len() >= min && has_digit_and_letter(token) && !is_placeholder(token) {
                out.push(Found {
                    start,
                    end,
                    kind: "bearer token",
                });
            }
        }
    }
}

/// `NAME=value`, `name: value`, `"name": "value"` where the name says the value is a secret.
fn assignments(text: &str, out: &mut Vec<Found>) {
    let bytes = text.as_bytes();
    for (start, end) in runs(text, |b| is_token_byte(b) || b == b'.') {
        let raw = &text[start..end];
        let name = raw.to_ascii_lowercase().replace(['-', '.'], "_");
        if !SECRET_NAMES.iter().any(|w| name.contains(w)) || is_file_name(raw) {
            continue;
        }
        // Past a closing quote on the name, spaces, the separator and spaces.
        let mut i = end;
        if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            i += 1;
        }
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() || !(bytes[i] == b'=' || bytes[i] == b':') {
            continue;
        }
        i += 1;
        if i < bytes.len() && (bytes[i] == b'=' || (bytes[i] == b':' && bytes[i - 1] == b':')) {
            // `==` is a comparison, `::` a path (`secret::tests`).
            continue;
        }
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        let Some((value_start, value_end, quoted)) = value_at(text, i) else {
            continue;
        };
        let value = &text[value_start..value_end];
        if looks_like_secret(value, quoted) {
            out.push(Found {
                start: value_start,
                end: value_end,
                kind: "secret value",
            });
        }
    }
}

/// The value starting at byte `i`: inside quotes when it is quoted (`true` then), else up to
/// whitespace, a comma, a semicolon or a closing bracket.
fn value_at(text: &str, i: usize) -> Option<(usize, usize, bool)> {
    let bytes = text.as_bytes();
    let first = *bytes.get(i)?;
    if first == b'"' || first == b'\'' || first == b'`' {
        let close = text[i + 1..].find(first as char)?;
        let line_end = text[i + 1..].find('\n').unwrap_or(usize::MAX);
        return (close < line_end).then_some((i + 1, i + 1 + close, true));
    }
    // A backslash ends it too: `\n` in JSON or a shell line is not part of the value.
    let end = text[i..]
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '}' | ')' | '\\'))
        .map_or(text.len(), |p| i + p);
    (end > i).then_some((i, end, false))
}

/// `notes/api-key-rotation.md`: a file named after a secret, not one.
fn is_file_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && ext.bytes().any(|b| b.is_ascii_lowercase())
    })
}

fn looks_like_secret(value: &str, quoted: bool) -> bool {
    if is_placeholder(value) {
        return false;
    }
    if value
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
    {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if ["true", "false", "null", "none", "nil", "undefined"].contains(&lower.as_str()) {
        return false;
    }
    if quoted {
        // A long quoted sentence is prose about a secret, not one.
        let len = value.chars().count();
        return len >= 6 && (len <= 64 || value.split_whitespace().count() <= 4);
    }
    // Unquoted: code (a call, an index, a variable) is not a literal secret.
    if value.chars().count() < 8 || value.contains(['(', '[', '{', '$', '%']) {
        return false;
    }
    // A file path (`~/.config/token.json`) names where a secret is, not the secret.
    let path = value.starts_with(['~', '/'])
        || value.starts_with("./")
        || value.starts_with("../")
        || value.rsplit_once('.').is_some_and(|(_, ext)| {
            (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphabetic())
        });
    if path {
        return false;
    }
    has_digit_and_letter(value)
        || value.chars().count() >= 20
        || value
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
}

fn is_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with('$')
        || lower.starts_with('{')
        || lower.starts_with('%')
        || lower.starts_with('[')
        || PLACEHOLDERS.iter().any(|p| lower.contains(p))
        || ["password", "pass", "secret", "token", "key"].contains(&lower.as_str())
        || value
            .chars()
            .all(|c| c == value.chars().next().unwrap_or(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(text: &str) -> String {
        redact(text).into_owned()
    }

    #[test]
    fn keys_by_shape() {
        let key = format!("sk-ant-api03-{}", "aB3".repeat(12));
        assert_eq!(masked(&format!("key {key} here")), "key [redacted] here");
        let gh = format!("ghp_{}", "x1Y2".repeat(9));
        assert_eq!(masked(&format!("token: {gh}")), "token: [redacted]");
        assert_eq!(masked("id AKIAIOSFODNN7EXAMPLE."), "id [redacted].");
        assert_eq!(
            kinds(&format!("{key} {gh}")),
            ["Anthropic key", "GitHub token"]
        );
    }

    #[test]
    fn ordinary_text_is_kept() {
        for text in [
            "commit 732fdc4 and f5dc25f7d1e2a9b0c3d4e5f60718293a4b5c6d7e",
            "the sk-learn package and task-runner",
            "max_tokens: 4096, input_tokens = 10",
            "let token = parse_token(input);",
            "if password == expected { return }",
            "password: ${{ secrets.DB_PASSWORD }}",
            "API_KEY=<your key>",
            "OPENAI_API_KEY=$OPENAI_API_KEY",
            "records app-0067 and app-D-0039",
            "https://github.com/emredagci/reprise",
            "export TOKEN_FILE=~/.config/token.json",
            "test secret::tests::keys_by_shape ... ok",
            r"persist-credentials: false\n",
            "items/app-0010-no-api-key.md:8:created: 2026-10-24",
            "masked (`*_TOKEN=` values, then everything else in the turn, up to its end, as written)",
            "/// `-----BEGIN … PRIVATE KEY-----` up to its END line",
            "/// The password of `scheme://user:password@host`.",
        ] {
            assert_eq!(masked(text), text, "{text}");
        }
    }

    #[test]
    fn assigned_values() {
        assert_eq!(
            masked("export OPENAI_API_KEY=abc123def456ghi"),
            "export OPENAI_API_KEY=[redacted]"
        );
        assert_eq!(
            masked(r#"{"clientSecret": "s3cr3t-value", "name": "x"}"#),
            r#"{"clientSecret": "[redacted]", "name": "x"}"#
        );
        assert_eq!(masked("password: hunter22"), "password: [redacted]");
        assert_eq!(
            masked("DB_PASSWORD='correct horse'"),
            "DB_PASSWORD='[redacted]'"
        );
    }

    #[test]
    fn urls_bearers_jwts_and_key_blocks() {
        assert_eq!(
            masked("postgres://app:pa55word@db:5432/x"),
            "postgres://app:[redacted]@db:5432/x"
        );
        assert_eq!(
            masked("Authorization: Bearer abc123def456ghi789"),
            "Authorization: Bearer [redacted]"
        );
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(masked(&format!("t={jwt}.")), "t=[redacted].");
        let pem = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\nb";
        assert_eq!(masked(pem), "a\n[redacted]\nb");
        let unclosed = "key:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNz\naC1rZXk=\n\nafter";
        assert_eq!(masked(unclosed), "key:\n[redacted]\n\nafter");
        assert_eq!(
            masked("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----"),
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----"
        );
    }

    #[test]
    fn masking_is_idempotent_and_keeps_unicode() {
        let text = "mot de passe: PASSWORD=éèà1234abcd and token sk-proj-abcdefghij0123456789xyz";
        let once = masked(text);
        assert_eq!(masked(&once), once);
        assert!(
            once.starts_with("mot de passe: PASSWORD=[redacted]"),
            "{once}"
        );
        assert!(!contains(&once));
    }
}
