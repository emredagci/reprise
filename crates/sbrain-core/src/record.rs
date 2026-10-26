//! Records: `---` frontmatter of `key: value` lines (a `[a, b]` value is a
//! list), then the body. A file without valid frontmatter is all body.

use std::path::Path;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    List(Vec<String>),
}

impl Value {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::List(_) => None,
        }
    }

    /// An empty string or list is false.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Text(s) => !s.is_empty(),
            Self::List(l) => !l.is_empty(),
        }
    }

    /// List items joined with spaces, or a string's characters joined with spaces.
    pub fn joined(&self) -> String {
        match self {
            Self::List(items) => items.join(" "),
            Self::Text(s) => s.chars().map(String::from).collect::<Vec<_>>().join(" "),
        }
    }

    pub fn items(&self) -> Vec<String> {
        match self {
            Self::List(items) => items.clone(),
            Self::Text(s) if s.is_empty() => Vec::new(),
            Self::Text(s) => vec![s.clone()],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    meta: Vec<(String, Value)>,
    pub body: String,
}

impl Record {
    /// Parses frontmatter; `None` when it is missing or unclosed.
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix("---\n")?;
        let end = rest.find("\n---\n")?;
        let mut meta: Vec<(String, Value)> = Vec::new();
        for line in rest[..end].lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let Some((key, val)) = line.split_once(':') else {
                continue;
            };
            let val = val.trim();
            let value = match val.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                Some(inner) => Value::List(
                    inner
                        .split(',')
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .collect(),
                ),
                None => Value::Text(val.to_owned()),
            };
            let key = key.trim().to_owned();
            // A repeated key keeps its first position and takes the last value.
            match meta.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => meta.push((key, value)),
            }
        }
        Some(Self {
            meta,
            body: rest[end + 5..].to_owned(),
        })
    }

    /// A file without valid frontmatter becomes all body.
    pub fn read(path: &Path) -> Result<Self> {
        let text = crate::safe::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self::parse(&text).unwrap_or(Self {
            meta: Vec::new(),
            body: text,
        }))
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.meta.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The text value of `key`; `""` when the key is missing or holds a list.
    pub fn text(&self, key: &str) -> &str {
        self.get(key).and_then(Value::as_text).unwrap_or("")
    }

    /// Sets `key`, keeping its position; a new key goes last.
    pub fn set(&mut self, key: &str, value: Value) {
        match self.meta.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.meta.push((key.to_owned(), value)),
        }
    }

    /// Frontmatter and body as text, keys in their order. Comment lines of
    /// the frontmatter are not kept.
    pub fn to_text(&self) -> String {
        let mut out = String::from("---\n");
        for (key, value) in &self.meta {
            let line = match value {
                Value::Text(s) => format!("{key}: {s}"),
                Value::List(items) => format!("{key}: [{}]", items.join(", ")),
            };
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(&self.body);
        out
    }

    /// The first of `keys` whose value is truthy.
    pub fn first_truthy(&self, keys: &[&str]) -> Option<&str> {
        keys.iter()
            .filter_map(|k| self.get(k))
            .find(|v| v.is_truthy())
            .and_then(Value::as_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_back_what_it_read() {
        let text = "---\nid: a-1\ntitle: x: y\nfiles: [a.rs, b.rs]\nverify:\n---\n\n## Problem\n";
        let mut r = Record::parse(text).unwrap();
        assert_eq!(r.to_text(), text);
        r.set("verify", Value::Text("cargo test".into()));
        r.set("new", Value::List(vec![]));
        assert_eq!(
            r.to_text(),
            "---\nid: a-1\ntitle: x: y\nfiles: [a.rs, b.rs]\nverify: cargo test\nnew: []\n---\n\n## Problem\n"
        );
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let r = Record::parse(
            "---\nid: a-1\ntitle: x: y\n# comment\nnot a pair\nfiles: [a.rs, , b.rs]\nid: a-2\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(r.text("id"), "a-2");
        assert_eq!(r.text("title"), "x: y");
        assert_eq!(
            r.get("files"),
            Some(&Value::List(vec!["a.rs".into(), "b.rs".into()]))
        );
        assert_eq!(r.body, "body\n");
    }

    #[test]
    fn missing_or_unclosed_frontmatter() {
        assert_eq!(Record::parse("id: x\n"), None);
        assert_eq!(Record::parse("---\nid: x\n"), None);
        assert_eq!(Record::parse("---\n\n---\nb").unwrap().body, "b");
    }

    #[test]
    fn joined_puts_spaces_between_items() {
        assert_eq!(Value::List(vec!["a".into(), "b".into()]).joined(), "a b");
        assert_eq!(Value::Text("ab".into()).joined(), "a b");
    }
}
