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
/// whitespace a single space. The digest's indenting of quoted lines disappears with it. Turkish
/// `I`, `İ`, `ı` and `i` all become `i`, so a quote matches whichever way its capitals were folded.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars().flat_map(char::to_lowercase) {
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' | '`' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{00ab}' | '\u{00bb}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00a0}' => ' ',
            '\u{0131}' => 'i',
            '\u{0307}' => continue,
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
            "## User [2026-10-30 10:00]\nBundan sonra   migration YAZMA.\n    Assistant: “Tamam”, migration’ı kaldırdım.",
        );
        assert!(found_in("bundan sonra migration yazma", &digest));
        assert!(found_in("\"Tamam\", migration'ı kaldırdım", &digest));
        assert!(found_in("Bundan sonra … kaldırdım", &digest));
        assert!(found_in("Bundan sonra ... migration yazma", &digest));
    }

    #[test]
    fn made_up_or_too_short_quotes_do_not_count() {
        let digest = normalize("## User [2026-10-30 10:00]\nevet, yap bunu lütfen");
        assert!(!found_in("migration yazılmayacak", &digest));
        assert!(!found_in("evet", &digest), "too short to tie anything");
        assert!(
            !found_in("lütfen … evet, yap", &digest),
            "parts out of order"
        );
        assert!(!found_in("evet, yap … ab", &digest), "a part too short");
        assert!(!found_in("", &digest));
        assert!(found_in("evet, yap bunu", &digest));
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
    fn turkish_capitals_match() {
        let source = normalize("İLK sürüm ÇIKTI");
        assert!(found_in("ilk sürüm çıktı", &source));
        assert!(found_in("İlk Sürüm Çıktı", &source));
        assert!(found_in("İLK sürüm ÇIKTI", &source));
    }
}
