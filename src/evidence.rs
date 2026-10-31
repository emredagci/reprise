//! Checking that what the engine distilled is in the session. Every item, item change, decision and
//! rule comes with quotes copied from the transcript digest; a quote counts only when its words are
//! there, in that order. A record left with no quote that counts is not written: the engine may have
//! made it up. A rule must quote the user's own messages, since text a tool printed or a file the
//! agent read can look like a correction.

/// Characters a quote needs, all its parts together, to tie a record to the session.
pub const MIN_QUOTE: usize = 12;
/// Characters each part of a quote cut with `…` needs.
const MIN_PART: usize = 4;

/// Text prepared for matching: lowercase, typographic quotes and dashes made plain, every run of
/// whitespace a single space. The digest's indenting of quoted lines disappears with it.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars().flat_map(char::to_lowercase) {
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' | '`' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{00ab}' | '\u{00bb}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00a0}' => ' ',
            c => c,
        };
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// The parts of a quote, split where the engine left words out (`…`, `...`, `[…]`), each trimmed of
/// surrounding quote marks and spaces; empty parts dropped.
fn parts(quote: &str) -> Vec<String> {
    let quote = normalize(quote);
    quote
        .replace("[…]", "…")
        .replace("[...]", "…")
        .replace("...", "…")
        .split('…')
        .map(|p| {
            p.trim_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '>'))
                .to_owned()
        })
        .filter(|p| !p.is_empty())
        .collect()
}

/// Whether `quote` is in `source` (already [`normalize`]d): every part long enough and found, in
/// order, and the whole long enough to mean something.
pub fn found_in(quote: &str, source: &str) -> bool {
    let parts = parts(quote);
    if parts.is_empty()
        || parts.iter().map(|p| p.chars().count()).sum::<usize>() < MIN_QUOTE
        || parts.iter().any(|p| p.chars().count() < MIN_PART)
    {
        return false;
    }
    let mut from = 0;
    for part in &parts {
        match source[from..].find(part.as_str()) {
            Some(at) => from += at + part.len(),
            None => return false,
        }
    }
    true
}

/// The quotes of `quotes` found in `source` (already [`normalize`]d), each trimmed and on one line.
pub fn supported(quotes: &[String], source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for q in quotes {
        let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
        if found_in(&q, source) && !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_match_across_case_spacing_and_marks() {
        let digest = normalize(
            "## User [2026-10-30 10:00]\nFrom now on   NO migrations.\n    Assistant: “Done”, I’ve removed the migration.",
        );
        assert!(found_in("from now on no migrations", &digest));
        assert!(found_in("\"Done\", I've removed the migration", &digest));
        assert!(found_in("From now on … removed the migration", &digest));
        assert!(found_in("From now on ... no migrations", &digest));
    }

    #[test]
    fn made_up_or_too_short_quotes_do_not_count() {
        let digest = normalize("## User [2026-10-30 10:00]\nyes, do it please");
        assert!(!found_in("no migration will be written", &digest));
        assert!(!found_in("yes", &digest), "too short to tie anything");
        assert!(!found_in("please … yes, do", &digest), "parts out of order");
        assert!(!found_in("yes, do … ab", &digest), "a part too short");
        assert!(!found_in("", &digest));
        assert!(found_in("yes, do it please", &digest));
    }

    #[test]
    fn supported_keeps_found_quotes_once() {
        let source = normalize("the release workflow updates the tap");
        let quotes = vec![
            "the release  workflow updates".to_owned(),
            "the release workflow updates".to_owned(),
            "it pushes to npm".to_owned(),
        ];
        assert_eq!(
            supported(&quotes, &source),
            ["the release workflow updates"]
        );
    }

    #[test]
    fn capitals_and_accents_match() {
        let source = normalize("CAFÉ menu SHIPPED");
        assert!(found_in("café menu shipped", &source));
        assert!(found_in("Café Menu Shipped", &source));
        assert!(
            !found_in("cafe menu shipped", &source),
            "accents are part of the words"
        );
    }
}
