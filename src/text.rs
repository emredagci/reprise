//! Text helpers for hook output and search: clipping, comments, tokens and sections.
//! Lengths are counted in characters, not bytes.

/// Trims, then cuts to `limit` characters and appends ` …[+N]`.
pub fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head} …[+{}]", count - limit)
}

/// The first `limit` characters.
pub fn take_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// Removes every `<!-- … -->` comment (non-greedy, across lines). An unclosed comment is kept.
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        let Some(len) = rest[start + 4..].find("-->") else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + 4 + len + 3..];
    }
    out.push_str(rest);
    out
}

/// Letters transliterated to ASCII (`café` → `cafe`), lowercased, runs of `[a-z0-9]`.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in deunicode::deunicode(text)
        .chars()
        .flat_map(char::to_lowercase)
    {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The body of `## heading` up to the next `## ` line, trimmed.
pub fn section(body: &str, heading: &str) -> String {
    let marker = format!("## {heading}\n");
    let mut from = 0;
    while let Some(pos) = body[from..].find(&marker).map(|p| p + from) {
        if pos == 0 || body.as_bytes()[pos - 1] == b'\n' {
            let start = pos + marker.len();
            // Search from the heading's own newline, so a `## ` line right after it ends an empty section.
            let end = body[start - 1..]
                .find("\n## ")
                .map_or(body.len(), |i| start - 1 + i + 1)
                .max(start);
            return body[start..end].trim().to_owned();
        }
        from = pos + 1;
    }
    String::new()
}

/// `n` with a comma between each group of three digits: `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_counts_characters() {
        assert_eq!(clip("  ééé  ", 3), "ééé");
        assert_eq!(clip("éèêëàâ", 2), "éè …[+4]");
    }

    #[test]
    fn strip_comments_removes_closed_comments_only() {
        assert_eq!(strip_comments("a<!-- x\ny -->b<!--c-->d"), "abd");
        assert_eq!(strip_comments("a<!-- open"), "a<!-- open");
    }

    #[test]
    fn tokens_transliterate_and_split() {
        assert_eq!(
            tokens("Add it to reprise.toml, step-2"),
            ["add", "it", "to", "reprise", "toml", "step", "2"]
        );
        assert_eq!(
            tokens("Crème brûlée, naïve café"),
            ["creme", "brulee", "naive", "cafe"]
        );
    }

    #[test]
    fn section_matches_line_start_only() {
        let body = "x ## Étape\nno\n## Étape\n  do it\nmore\n## Next\nz";
        assert_eq!(section(body, "Étape"), "do it\nmore");
        assert_eq!(section("## A\n## B\nb", "A"), "");
        assert_eq!(section("## A\nlast", "A"), "last");
        assert_eq!(section("none", "A"), "");
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
