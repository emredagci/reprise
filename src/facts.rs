//! Facts: values that hold for a while (a version, a port, the host something runs on), each with the
//! day it became true and, once something replaced it, the day it stopped. A new value closes the
//! old one; rows are never deleted, so "what was it in September" is a lookup and an old value can
//! never pass for the current one. A project's facts are in `projects/<name>/facts.md`; facts about
//! no single project in `profile/facts.md`.

use std::path::PathBuf;

use chrono::NaiveDate;
use serde::Serialize;

use crate::dates::When;
use crate::{Error, Result, Vault};

pub const FILE: &str = "facts.md";
const COLUMNS: [&str; 6] = ["Key", "Value", "From", "To", "Source", "Noted"];
const HEAD: &str = "# Facts\n\nValues that hold for a while. A new value closes the old one in `To`; rows are never \
deleted. Written with `reprise fact`.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub key: String,
    pub value: String,
    /// When it became true.
    #[serde(serialize_with = "display")]
    pub from: When,
    /// When it stopped being true; `None` while it holds.
    #[serde(serialize_with = "display_opt")]
    pub to: Option<When>,
    /// The record or session it came from; may be empty.
    pub source: String,
    /// The day it was written down, `YYYY-MM-DD`.
    pub noted: String,
}

fn display<S: serde::Serializer>(w: &When, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&w.display())
}

fn display_opt<S: serde::Serializer>(
    w: &Option<When>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match w {
        Some(w) => s.serialize_str(&w.display()),
        None => s.serialize_none(),
    }
}

impl Fact {
    /// Whether it held on `day`: from its start, up to (not on) the start of its end.
    pub fn holds_on(&self, day: NaiveDate) -> bool {
        self.from.start <= day && self.to.is_none_or(|to| day < to.start)
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.key.clone(),
            self.value.clone(),
            self.from.display(),
            self.to.map(|t| t.display()).unwrap_or_default(),
            self.source.clone(),
            self.noted.clone(),
        ]
    }
}

/// What `set` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "outcome")]
pub enum Set {
    /// The key had no value yet.
    Added,
    /// The key already holds this value; nothing was written.
    Same,
    /// The value it replaced, now closed.
    Replaced { old: String },
}

/// `projects/<project>/facts.md`, or `profile/facts.md` with no project.
pub fn path(vault: &Vault, project: Option<&str>) -> Result<PathBuf> {
    Ok(match project {
        Some(p) => {
            let dir = vault.project_dir(p);
            if p.is_empty() || p.contains(['/', '\\']) || p.starts_with('.') || !dir.is_dir() {
                return Err(Error::ProjectNotFound(p.to_owned()));
            }
            dir.join(FILE)
        }
        None => vault.profile_dir().join(FILE),
    })
}

/// Every fact, in the order written; rows that do not parse are left out.
pub fn read(vault: &Vault, project: Option<&str>) -> Result<Vec<Fact>> {
    let text = crate::safe::read_to_string(path(vault, project)?).unwrap_or_default();
    Ok(crate::table::rows(&text)
        .into_iter()
        .filter_map(|r| {
            let from = When::parse_display(&r[2])?;
            let to = if r[3].is_empty() {
                None
            } else {
                Some(When::parse_display(&r[3])?)
            };
            (!r[0].is_empty()).then(|| Fact {
                key: r[0].clone(),
                value: r[1].clone(),
                from,
                to,
                source: r[4].clone(),
                noted: r[5].clone(),
            })
        })
        .collect())
}

fn write(vault: &Vault, project: Option<&str>, facts: &[Fact]) -> Result<PathBuf> {
    let path = path(vault, project)?;
    let rows: Vec<Vec<String>> = facts.iter().map(Fact::row).collect();
    crate::scaffold::write(&path, &crate::table::render(HEAD, &COLUMNS, &rows))?;
    Ok(path
        .strip_prefix(vault.root())
        .unwrap_or(&path)
        .to_path_buf())
}

fn same_key(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The facts that hold now.
pub fn current(facts: &[Fact]) -> Vec<&Fact> {
    facts.iter().filter(|f| f.to.is_none()).collect()
}

/// The facts that held on `day`.
pub fn on(facts: &[Fact], day: NaiveDate) -> Vec<&Fact> {
    facts.iter().filter(|f| f.holds_on(day)).collect()
}

/// Gives `key` the value `value` from `from`: the value it held is closed there. A value from before
/// the current one began is history and needs its end (`history`). Returns what happened and the
/// file, relative to the vault.
pub fn set(
    vault: &Vault,
    project: Option<&str>,
    key: &str,
    value: &str,
    from: When,
    source: &str,
    today: NaiveDate,
) -> Result<(Set, PathBuf)> {
    let (key, value) = (one_line(key), one_line(value));
    if key.is_empty() || value.is_empty() {
        return Err(Error::InvalidChange(
            "a fact needs a key and a value".into(),
        ));
    }
    let mut facts = read(vault, project)?;
    let mut outcome = Set::Added;
    if let Some(open) = facts
        .iter_mut()
        .find(|f| f.to.is_none() && same_key(&f.key, &key))
    {
        if open.value.eq_ignore_ascii_case(&value) {
            return Ok((Set::Same, path(vault, project)?));
        }
        if from.start < open.from.start {
            return Err(Error::InvalidChange(format!(
                "{key} is {} since {}; a value from before that is history: give when it ended",
                open.value,
                open.from.display()
            )));
        }
        open.to = Some(from);
        outcome = Set::Replaced {
            old: open.value.clone(),
        };
    }
    facts.push(Fact {
        key,
        value,
        from,
        to: None,
        source: one_line(source),
        noted: today.format("%Y-%m-%d").to_string(),
    });
    Ok((outcome, write(vault, project, &facts)?))
}

/// Writes down a value `key` held from `from` until `to`, which closed before now.
pub fn history(
    vault: &Vault,
    project: Option<&str>,
    key: &str,
    value: &str,
    (from, to): (When, When),
    source: &str,
    today: NaiveDate,
) -> Result<PathBuf> {
    let (key, value) = (one_line(key), one_line(value));
    if key.is_empty() || value.is_empty() {
        return Err(Error::InvalidChange(
            "a fact needs a key and a value".into(),
        ));
    }
    if to.start < from.start {
        return Err(Error::InvalidChange(format!(
            "{key}: it ended ({}) before it began ({})",
            to.display(),
            from.display()
        )));
    }
    let mut facts = read(vault, project)?;
    facts.push(Fact {
        key,
        value,
        from,
        to: Some(to),
        source: one_line(source),
        noted: today.format("%Y-%m-%d").to_string(),
    });
    write(vault, project, &facts)
}

/// `key` stops holding on `on`, with nothing in its place. Returns the value it had.
pub fn end(vault: &Vault, project: Option<&str>, key: &str, on: When) -> Result<(String, PathBuf)> {
    let mut facts = read(vault, project)?;
    let Some(open) = facts
        .iter_mut()
        .find(|f| f.to.is_none() && same_key(&f.key, key))
    else {
        return Err(Error::InvalidChange(format!("{key} has no current value")));
    };
    if on.start < open.from.start {
        return Err(Error::InvalidChange(format!(
            "{key} began on {}; it cannot end before that",
            open.from.display()
        )));
    }
    open.to = Some(on);
    let old = open.value.clone();
    Ok((old, write(vault, project, &facts)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn w(s: &str) -> When {
        When::parse_display(s).unwrap()
    }

    fn vault() -> (tempfile::TempDir, Vault) {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(tmp.path());
        crate::scaffold::ensure_project(&vault, "app", None, d("2026-10-01")).unwrap();
        (tmp, vault)
    }

    #[test]
    fn a_new_value_closes_the_old_one() {
        let (_tmp, vault) = vault();
        let p = Some("app");
        let today = d("2026-10-30");
        let (o, path) = set(
            &vault,
            p,
            "Node version",
            "20",
            w("2026-03"),
            "app-0001",
            today,
        )
        .unwrap();
        assert_eq!(o, Set::Added);
        assert_eq!(path, PathBuf::from("projects/app/facts.md"));
        assert_eq!(
            set(&vault, p, "node  version", "20", w("2026-05-01"), "", today)
                .unwrap()
                .0,
            Set::Same
        );
        let (o, _) = set(
            &vault,
            p,
            "node version",
            "22",
            w("2026-10-20"),
            "1a2b3c4d",
            today,
        )
        .unwrap();
        assert_eq!(o, Set::Replaced { old: "20".into() });

        let facts = read(&vault, p).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(current(&facts)[0].value, "22");
        assert_eq!(on(&facts, d("2026-09-15"))[0].value, "20");
        assert_eq!(
            on(&facts, d("2026-10-20"))[0].value,
            "22",
            "the new value from its first day"
        );
        assert!(on(&facts, d("2026-02-01")).is_empty());
        assert_eq!(facts[0].to.unwrap().display(), "2026-10-20");

        let err = set(&vault, p, "node version", "18", w("2026-01"), "", today).unwrap_err();
        assert!(err.to_string().contains("history"), "{err}");
        history(
            &vault,
            p,
            "node version",
            "18",
            (w("2026-01"), w("2026-03")),
            "",
            today,
        )
        .unwrap();
        assert_eq!(
            on(&read(&vault, p).unwrap(), d("2026-01-10"))[0].value,
            "18"
        );

        let text = std::fs::read_to_string(vault.project_dir("app").join(FILE)).unwrap();
        assert!(
            text.contains("| Node version | 20 | 2026-03 | 2026-10-20 | app-0001 | 2026-10-30 |"),
            "{text}"
        );
    }

    #[test]
    fn ending_a_fact_and_general_facts() {
        let (_tmp, vault) = vault();
        let today = d("2026-10-30");
        set(&vault, None, "editor", "zed", w("2026"), "", today).unwrap();
        let (old, path) = end(&vault, None, "Editor", w("2026-10-29")).unwrap();
        assert_eq!(old, "zed");
        assert_eq!(path, PathBuf::from("profile/facts.md"));
        assert!(current(&read(&vault, None).unwrap()).is_empty());
        assert!(end(&vault, None, "editor", w("2026-10-30")).is_err());
        assert!(read(&vault, Some("nope")).is_err());
        assert!(set(&vault, Some("../x"), "k", "v", w("2026"), "", today).is_err());
    }
}
