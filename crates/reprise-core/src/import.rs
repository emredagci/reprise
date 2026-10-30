//! Imports a format 1 vault into a format 2 vault. The source is only read; the target only
//! gains files. A file that already exists in the target is never overwritten: identical content counts
//! as already imported, anything else is reported as a conflict and left alone. The only exceptions
//! are the profile files while they still hold their untouched templates.
//!
//! What moves where: docs/vault-format.md, "Importing a format 1 vault".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::project::{ProjectToml, origin_remote};
use crate::{Error, FolderKind, Result, Vault, scaffold};

/// Top-level folders of a format 1 vault that hold records, and where they go.
const RECORD_ROOTS: &[(&str, &str)] = &[
    ("knowledge", "notes"),
    ("inbox", "notes/inbox"),
    ("archive", "notes/archive"),
];

/// Top-level entries that are neither records nor tooling worth mentioning.
const IGNORED: &[&str] = &[".git", ".DS_Store"];

/// Format 1 headings → format 2, per kind of record. A heading may carry a suffix such as a date:
/// `## Çalışıyor (2026-10-24)` becomes `## Working (2026-10-24)`.
const ITEM_HEADINGS: &[(&str, &str)] = &[
    ("Belirti", "Problem"),
    ("Kök neden", "Cause"),
    ("Çözüm", "Solution"),
    ("Doğrulama", "Verification"),
    ("Notlar", "Notes"),
];
const DECISION_HEADINGS: &[(&str, &str)] = &[
    ("Bağlam", "Context"),
    ("Karar", "Decision"),
    ("Neden", "Why"),
    ("Alternatifler", "Alternatives"),
];
const SESSION_HEADINGS: &[(&str, &str)] = &[
    ("Yapılan", "Done"),
    ("Kararlar", "Decisions"),
    ("Açık kalan", "Open"),
    ("Belirsiz", "Uncertain"),
    ("Sonraki adım", "Next step"),
    ("Dokunulan kayıtlar", "Touched records"),
];
const STATE_HEADINGS: &[(&str, &str)] = &[
    ("Ne", "What"),
    ("Mimari", "Architecture"),
    ("Çalışmıyor / bilinen sorunlar", "Not working"),
    ("Çalışıyor", "Working"),
    ("Nasıl çalıştırılır / test edilir", "Run and test"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Profile,
    Note,
    Project,
    State,
    Item,
    Decision,
    Session,
    Other,
    Settings,
}

/// One file the import writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    /// Relative to the source vault.
    pub from: PathBuf,
    /// Relative to the target vault.
    pub to: PathBuf,
    pub kind: Kind,
    /// Whether the text changes on the way (headings, fields).
    pub converted: bool,
    #[serde(skip)]
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A project's repository found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo {
    pub project: String,
    pub path: PathBuf,
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub source: PathBuf,
    pub vault: PathBuf,
    pub projects: Vec<String>,
    /// Files to write, per kind.
    pub counts: BTreeMap<Kind, usize>,
    pub steps: Vec<Step>,
    /// Files already in the target with the same content.
    pub unchanged: usize,
    pub conflicts: Vec<Conflict>,
    /// Top-level entries of the source that are tooling, not records (scripts, skills, local state).
    pub skipped: Vec<PathBuf>,
    pub repos: Vec<Repo>,
    /// Things the import could not carry over and the user may want to know.
    pub notes: Vec<String>,
}

/// Works out the import of `source` into `vault` without writing anything.
pub fn plan(source: &Path, vault: &Vault) -> Result<Plan> {
    let source = crate::error::canonical(source)?;
    if Vault::inspect(&source)? != FolderKind::Format1 {
        return Err(Error::NotFormat1Vault(source));
    }
    if source.starts_with(vault.root()) || vault.root().starts_with(&source) {
        return Err(Error::ImportOverlap(source));
    }
    let mut plan = Plan {
        source: source.clone(),
        vault: vault.root().to_path_buf(),
        ..Plan::default()
    };
    let mut wanted: Vec<Step> = Vec::new();

    for (from, to) in [("core.md", "user.md"), ("rules.md", "rules.md")] {
        let from = Path::new("companion").join(from);
        if let Ok(text) = crate::safe::read_to_string(source.join(&from)) {
            wanted.push(step(
                from,
                Path::new("profile").join(to),
                Kind::Profile,
                text,
                false,
            ));
        }
    }
    for (root, to) in RECORD_ROOTS {
        for rel in files(&source.join(root)) {
            let text = read(&source.join(root).join(&rel))?;
            wanted.push(step(
                Path::new(root).join(&rel),
                Path::new(to).join(&rel),
                Kind::Note,
                text,
                false,
            ));
        }
    }

    let projects_dir = source.join("projects");
    let mut names: Vec<String> = std::fs::read_dir(&projects_dir)
        .map_err(|source| Error::Io {
            path: projects_dir.clone(),
            source,
        })?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    for name in &names {
        let dir = projects_dir.join(name);
        let mut ids = Vec::new();
        let mut repo = None;
        for rel in files(&dir) {
            let from = Path::new("projects").join(name).join(&rel);
            let to = from.clone();
            let raw = read(&dir.join(&rel))?;
            let first = rel.components().next().and_then(|c| c.as_os_str().to_str());
            let (kind, text) = match (first, rel.extension().and_then(|e| e.to_str())) {
                (Some("items"), Some("md")) => {
                    ids.extend(field(&raw, "id"));
                    (Kind::Item, convert_headings(&raw, ITEM_HEADINGS))
                }
                (Some("decisions"), Some("md")) => {
                    (Kind::Decision, convert_headings(&raw, DECISION_HEADINGS))
                }
                (Some("sessions"), Some("md")) => (
                    Kind::Session,
                    convert_headings(&rename_source(&raw), SESSION_HEADINGS),
                ),
                (Some("state.md"), _) => {
                    repo = field(&raw, "repo").map(PathBuf::from);
                    (
                        Kind::State,
                        convert_headings(&drop_field(&raw, "repo"), STATE_HEADINGS),
                    )
                }
                (Some("project.json"), _) => {
                    plan.notes.push(format!(
                        "projects/{name}/project.json is not copied: its prefix and remote go into \
                         project.toml, and its repository path is remembered on this machine"
                    ));
                    repo = repo.or_else(|| first_mapped_path(&raw));
                    continue;
                }
                _ => (Kind::Other, raw.clone()),
            };
            let converted = text != raw;
            wanted.push(step(from, to, kind, text, converted));
        }

        // The repository on this machine, unless it is inside the source vault.
        let repo = repo
            .filter(|p| p.is_dir() && !p.starts_with(&source))
            .map(|p| p.canonicalize().unwrap_or(p));
        let remote = repo.as_deref().and_then(origin_remote);
        if let Some(path) = &repo {
            plan.repos.push(Repo {
                project: name.clone(),
                path: path.clone(),
                remote: remote.clone(),
            });
        }
        let prefix = ids
            .iter()
            .find_map(|id| id_prefix(id))
            .unwrap_or_else(|| scaffold::default_prefix(name));
        let repos = remote.map(|r| format!("\"{r}\"")).unwrap_or_default();
        wanted.push(step(
            Path::new("projects").join(name).join("project.json"),
            Path::new("projects").join(name).join("project.toml"),
            Kind::Project,
            scaffold::render(
                scaffold::PROJECT_TOML,
                &[("prefix", &prefix), ("repos", &repos)],
            ),
            true,
        ));
    }
    plan.projects = names;

    if let Some(step) = containers_step(&source, vault, &mut plan.notes) {
        wanted.push(step);
    }

    for s in wanted {
        let target = vault.root().join(&s.to);
        match crate::safe::read_to_string(&target) {
            // Computed from the target's current settings, so it never overwrites anything.
            _ if s.kind == Kind::Settings => plan.steps.push(s),
            Err(_) => plan.steps.push(s),
            Ok(existing) if existing == s.text => plan.unchanged += 1,
            Ok(existing) if is_template(&s.to, &existing) => plan.steps.push(s),
            Ok(existing) if s.kind == Kind::Project && same_project(&existing, &s.text) => {
                plan.unchanged += 1
            }
            Ok(_) => plan.conflicts.push(Conflict {
                from: s.from,
                to: s.to,
            }),
        }
    }
    for s in &plan.steps {
        *plan.counts.entry(s.kind).or_insert(0) += 1;
    }

    let known: Vec<&str> = ["companion", "projects"]
        .into_iter()
        .chain(RECORD_ROOTS.iter().map(|(r, _)| *r))
        .chain(IGNORED.iter().copied())
        .collect();
    let mut skipped: Vec<PathBuf> = std::fs::read_dir(&source)
        .map_err(|e| Error::Io {
            path: source.clone(),
            source: e,
        })?
        .flatten()
        .map(|e| e.file_name())
        .filter(|n| !known.iter().any(|k| n == *k))
        .map(PathBuf::from)
        .collect();
    skipped.sort();
    plan.skipped = skipped;
    Ok(plan)
}

/// Writes the plan's files and remembers the repositories found. Plans again first, so nothing that
/// changed in the meantime is overwritten.
pub fn apply(source: &Path, vault: &Vault) -> Result<Plan> {
    let plan = plan(source, vault)?;
    for s in &plan.steps {
        scaffold::write(&vault.root().join(&s.to), &s.text)?;
    }
    for r in &plan.repos {
        crate::cache::remember_repo(vault, &r.project, &r.path)?;
    }
    Ok(plan)
}

fn step(from: PathBuf, to: PathBuf, kind: Kind, text: String, converted: bool) -> Step {
    Step {
        from,
        to,
        kind,
        converted,
        text,
    }
}

fn read(path: &Path) -> Result<String> {
    crate::safe::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Files below `dir` relative to it, sorted, without hidden files and folders.
fn files(dir: &Path) -> Vec<PathBuf> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Byte range of the frontmatter's inner lines, if the text has one.
fn frontmatter(text: &str) -> Option<std::ops::Range<usize>> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some(4..4 + end + 1)
}

fn field(text: &str, key: &str) -> Option<String> {
    let range = frontmatter(text)?;
    text[range].lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key && !v.trim().is_empty()).then(|| v.trim().to_owned())
    })
}

fn drop_field(text: &str, key: &str) -> String {
    let Some(range) = frontmatter(text) else {
        return text.to_owned();
    };
    let kept: String = text[range.clone()]
        .split_inclusive('\n')
        .filter(|line| line.split_once(':').is_none_or(|(k, _)| k.trim() != key))
        .collect();
    format!("{}{kept}{}", &text[..range.start], &text[range.end..])
}

/// `source: devret` → `source: handoff` in a session card.
fn rename_source(text: &str) -> String {
    let Some(range) = frontmatter(text) else {
        return text.to_owned();
    };
    let inner: String = text[range.clone()]
        .split_inclusive('\n')
        .map(|line| match line.trim_end() {
            "source: devret" => line.replacen("devret", "handoff", 1),
            _ => line.to_owned(),
        })
        .collect();
    format!("{}{inner}{}", &text[..range.start], &text[range.end..])
}

/// Renames `## ` headings outside code fences. A heading matches a name exactly or followed by a space
/// or `(`; longer names are tried first, so `Çalışmıyor …` is not taken for `Çalışıyor`.
fn convert_headings(text: &str, map: &[(&str, &str)]) -> String {
    let mut map = map.to_vec();
    map.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    let mut fenced = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let renamed = (!fenced)
            .then(|| line.strip_prefix("## "))
            .flatten()
            .and_then(|heading| {
                map.iter().find_map(|(from, to)| {
                    let rest = heading.strip_prefix(from)?;
                    (rest.trim_end().is_empty() || rest.starts_with([' ', '(']))
                        .then(|| format!("## {to}{rest}"))
                })
            });
        out.push_str(renamed.as_deref().unwrap_or(line));
    }
    out
}

/// `api-0012` → `api`; decision ids (`api-D-0003`) are not used.
fn id_prefix(id: &str) -> Option<String> {
    let (prefix, number) = id.rsplit_once('-')?;
    (crate::scaffold::is_prefix(prefix)
        && !prefix.ends_with("-D")
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit()))
    .then(|| prefix.to_owned())
}

/// The first entry of a format 1 `project.json` `paths` list.
fn first_mapped_path(text: &str) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let path = value.get("paths")?.as_array()?.first()?.as_str()?;
    Some(match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    })
}

/// Whether the target file still holds the template the vault was created with.
fn is_template(to: &Path, existing: &str) -> bool {
    match to.to_str() {
        Some("profile/user.md") => existing == scaffold::USER_MD,
        Some("profile/rules.md") => existing == scaffold::RULES_MD,
        _ => false,
    }
}

/// A `project.toml` already in the target is fine when prefix and remotes agree.
fn same_project(existing: &str, wanted: &str) -> bool {
    let parse = |t: &str| toml::from_str::<ProjectToml>(t).ok();
    match (parse(existing), parse(wanted)) {
        (Some(a), Some(b)) => a.prefix == b.prefix && (b.repos.is_empty() || a.repos == b.repos),
        _ => false,
    }
}

/// Carries `containers` from `.claude/brain.json` into an untouched `containers = []` line.
fn containers_step(source: &Path, vault: &Vault, notes: &mut Vec<String>) -> Option<Step> {
    let text = crate::safe::read_to_string(source.join(".claude/brain.json")).ok()?;
    let config: serde_json::Value = serde_json::from_str(&text).ok()?;
    let containers: Vec<String> = config
        .get("containers")?
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    if containers.is_empty() {
        return None;
    }
    if vault.config().projects.containers == containers {
        return None;
    }
    let current = crate::safe::read_to_string(vault.config_path()).ok()?;
    let line = "containers = []\n";
    let list = containers
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    if current.matches(line).count() != 1 {
        notes.push(format!(
            "set containers = [{list}] under [projects] in .reprise/vault.toml by hand"
        ));
        return None;
    }
    Some(step(
        PathBuf::from(".claude/brain.json"),
        PathBuf::from(".reprise/vault.toml"),
        Kind::Settings,
        current.replacen(line, &format!("containers = [{list}]\n"), 1),
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn w(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    struct Env {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        source: PathBuf,
        vault: Vault,
    }

    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let source = base.join("format1");
        let repo = base.join("code/api");
        fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &["remote", "add", "origin", "git@github.com:acme/api.git"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        w(
            &source,
            "companion/core.md",
            "# Core\n\nPrefers short answers.\n",
        );
        w(
            &source,
            "companion/rules.md",
            "# Kurallar\n\n- 2026-10-24: Read first.\n",
        );
        w(&source, "knowledge/rust/toolchain.md", "rustup via brew\n");
        w(&source, ".claude/brain.json", r#"{"containers": ["work"]}"#);
        w(&source, ".claude/scripts/tool.py", "print()\n");
        w(&source, ".state/sweep.json", "{}");
        w(&source, "skills/handoff/SKILL.md", "x");
        w(&source, "CLAUDE.md", "# Vault\n");
        w(
            &source,
            "projects/api/state.md",
            &format!(
                "---\nproject: api\nrepo: {}\nupdated: 2026-10-24\n---\n\n# api\n\n## Ne\nAn API.\n\n## Çalışıyor (2026-10-24)\n- login\n\n## Çalışmıyor / bilinen sorunlar\n\n## Nasıl çalıştırılır / test edilir\n```\n## Ne\n```\n",
                repo.display()
            ),
        );
        w(
            &source,
            "projects/api/items/api-0001-login.md",
            "---\nid: api-0001\ntitle: Login\nstatus: inbox\n---\n\n## Belirti\nbroken\n\n## Kök neden\n\n## Çözüm\n\n## Doğrulama\n\n## Notlar\n",
        );
        w(
            &source,
            "projects/api/decisions/api-D-0001-cookie.md",
            "---\nid: api-D-0001\ntitle: Cookie\nstatus: accepted\n---\n\n## Bağlam\n\n## Karar\nCookie.\n\n## Neden\n\n## Alternatifler\n",
        );
        w(
            &source,
            "projects/api/sessions/2026-10-24-1000-aaaa.md",
            "---\nsession: aaaa\nsource: devret\n---\n\n## Yapılan\n\n## Sonraki adım\nship\n\n## Belirsiz\n",
        );
        w(
            &source,
            "projects/api/gotchas.md",
            "# api: tuzaklar\n\n- 2026-10-24: x\n",
        );
        w(&source, "projects/api/reference.md", "hosts\n");
        w(&source, "projects/api/.DS_Store", "");
        let vault = crate::vault::tests::new_vault(&base.join("New"));
        Env {
            _tmp: tmp,
            base,
            source,
            vault,
        }
    }

    fn read(env: &Env, rel: &str) -> String {
        fs::read_to_string(env.vault.root().join(rel)).unwrap()
    }

    #[test]
    fn plan_writes_nothing_and_lists_everything() {
        let env = env();
        let before = files(env.vault.root());
        let p = plan(&env.source, &env.vault).unwrap();
        assert_eq!(files(env.vault.root()), before, "planning writes nothing");
        assert_eq!(p.projects, ["api"]);
        assert_eq!(
            p.counts,
            BTreeMap::from([
                (Kind::Profile, 2),
                (Kind::Note, 1),
                (Kind::Project, 1),
                (Kind::State, 1),
                (Kind::Item, 1),
                (Kind::Decision, 1),
                (Kind::Session, 1),
                (Kind::Other, 2),
                (Kind::Settings, 1),
            ])
        );
        assert_eq!(
            p.skipped,
            [".claude", ".state", "CLAUDE.md", "skills"].map(PathBuf::from)
        );
        assert!(p.conflicts.is_empty());
        assert_eq!(p.repos.len(), 1);
        assert_eq!(p.repos[0].remote.as_deref(), Some("github.com/acme/api"));
    }

    #[test]
    fn apply_converts_records() {
        let env = env();
        apply(&env.source, &env.vault).unwrap();

        assert_eq!(
            read(&env, "profile/user.md"),
            "# Core\n\nPrefers short answers.\n"
        );
        assert_eq!(read(&env, "notes/rust/toolchain.md"), "rustup via brew\n");
        let state = read(&env, "projects/api/state.md");
        assert!(state.starts_with("---\nproject: api\nupdated: 2026-10-24\n---\n"));
        assert!(state.contains("\n## What\nAn API.\n"));
        assert!(state.contains("\n## Working (2026-10-24)\n"));
        assert!(state.contains("\n## Not working\n"));
        assert!(
            state.contains("\n## Run and test\n```\n## Ne\n```\n"),
            "fenced code is kept"
        );
        let item = read(&env, "projects/api/items/api-0001-login.md");
        assert!(item.contains(
            "## Problem\nbroken\n\n## Cause\n\n## Solution\n\n## Verification\n\n## Notes\n"
        ));
        let decision = read(&env, "projects/api/decisions/api-D-0001-cookie.md");
        assert!(
            decision.contains("## Context\n\n## Decision\nCookie.\n\n## Why\n\n## Alternatives\n")
        );
        let card = read(&env, "projects/api/sessions/2026-10-24-1000-aaaa.md");
        assert!(card.contains("source: handoff\n"));
        assert!(card.contains("## Done\n\n## Next step\nship\n\n## Uncertain\n"));
        assert_eq!(read(&env, "projects/api/reference.md"), "hosts\n");
        assert!(!env.vault.root().join("projects/api/.DS_Store").exists());

        let cfg = ProjectToml::read(&env.vault.root().join("projects/api/project.toml")).unwrap();
        assert_eq!(cfg.prefix("api"), "api");
        assert_eq!(cfg.repos, ["github.com/acme/api"]);
        assert_eq!(
            crate::cache::repo_path(&env.vault, "api"),
            Some(env.base.join("code/api"))
        );
        assert_eq!(env.vault.config().projects.containers, ["work"]);

        // The imported vault reads like any other.
        let report =
            crate::report::report(&env.vault, "api", chrono::Local::now().date_naive()).unwrap();
        assert_eq!(report.last_session.unwrap().next, "ship");
    }

    #[test]
    fn source_is_untouched_and_rerun_is_a_no_op() {
        let env = env();
        let snapshot = |root: &Path| {
            files(root)
                .into_iter()
                .map(|f| (f.clone(), fs::read(root.join(f)).unwrap()))
                .collect::<Vec<_>>()
        };
        let before = snapshot(&env.source);
        apply(&env.source, &env.vault).unwrap();
        assert_eq!(snapshot(&env.source), before);

        let again = plan(&env.source, &env.vault).unwrap();
        assert!(again.steps.is_empty(), "{:?}", again.steps);
        assert!(again.conflicts.is_empty());
        assert!(again.unchanged > 0);
    }

    #[test]
    fn existing_target_files_are_conflicts_not_overwritten() {
        let env = env();
        w(env.vault.root(), "profile/user.md", "mine\n");
        w(
            env.vault.root(),
            "projects/api/items/api-0001-login.md",
            "other\n",
        );
        let p = apply(&env.source, &env.vault).unwrap();
        let conflicts: Vec<_> = p.conflicts.iter().map(|c| c.to.clone()).collect();
        assert_eq!(
            conflicts,
            [
                PathBuf::from("profile/user.md"),
                PathBuf::from("projects/api/items/api-0001-login.md")
            ]
        );
        assert_eq!(read(&env, "profile/user.md"), "mine\n");
        assert_eq!(
            read(&env, "projects/api/items/api-0001-login.md"),
            "other\n"
        );
    }

    #[test]
    fn project_created_by_a_session_is_kept() {
        let env = env();
        crate::scaffold::ensure_project(
            &env.vault,
            "api",
            Some("github.com/acme/api"),
            chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
        )
        .unwrap();
        let p = plan(&env.source, &env.vault).unwrap();
        let conflicts: Vec<_> = p.conflicts.iter().map(|c| c.to.clone()).collect();
        // The session's empty state.md and gotchas.md are in the way; project.toml agrees.
        assert_eq!(
            conflicts,
            [
                PathBuf::from("projects/api/gotchas.md"),
                PathBuf::from("projects/api/state.md")
            ]
        );
    }

    #[test]
    fn refuses_wrong_sources() {
        let env = env();
        assert!(matches!(
            plan(env.vault.root(), &env.vault),
            Err(Error::NotFormat1Vault(_))
        ));
        let empty = env.base.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            plan(&empty, &env.vault),
            Err(Error::NotFormat1Vault(_))
        ));
        let inner = env.vault.root().join("notes/inner");
        fs::create_dir_all(inner.join("projects")).unwrap();
        assert!(matches!(
            plan(&inner, &env.vault),
            Err(Error::ImportOverlap(_))
        ));
    }

    #[test]
    fn helpers() {
        assert_eq!(id_prefix("api-0012").as_deref(), Some("api"));
        assert_eq!(id_prefix("web-app-0001").as_deref(), Some("web-app"));
        assert_eq!(id_prefix("api-D-0003"), None);
        assert_eq!(id_prefix("notes"), None);
        assert_eq!(
            convert_headings(
                "## Çalışmıyor / bilinen sorunlar\n## Çalışıyor\n## Nedir\n",
                STATE_HEADINGS
            ),
            "## Not working\n## Working\n## Nedir\n"
        );
        assert_eq!(
            drop_field("no frontmatter\nrepo: x\n", "repo"),
            "no frontmatter\nrepo: x\n"
        );
    }
}
