//! Creates the vault layout and project folders from the templates embedded in the binary
//! (`templates/`). Layout and fields: docs/vault-format.md.

use std::path::Path;

use chrono::NaiveDate;

use crate::project::ProjectToml;
use crate::{Result, Vault};

pub const VAULT_TOML: &str = include_str!("../templates/vault/vault.toml");
pub const GITIGNORE: &str = include_str!("../templates/vault/gitignore");
pub const AGENTS_MD: &str = include_str!("../templates/vault/AGENTS.md");
pub const CLAUDE_MD: &str = include_str!("../templates/vault/CLAUDE.md");
pub const README_MD: &str = include_str!("../templates/vault/README.md");
pub const USER_MD: &str = include_str!("../templates/vault/profile/user.md");
pub const RULES_MD: &str = include_str!("../templates/vault/profile/rules.md");

pub const PROJECT_TOML: &str = include_str!("../templates/project/project.toml");
pub const STATE_MD: &str = include_str!("../templates/project/state.md");
pub const GOTCHAS_MD: &str = include_str!("../templates/project/gotchas.md");

pub const ITEM_MD: &str = include_str!("../templates/record/item.md");
pub const DECISION_MD: &str = include_str!("../templates/record/decision.md");
pub const SESSION_MD: &str = include_str!("../templates/record/session.md");

/// Vault files relative to the root, in creation order. `.reprise/vault.toml` comes last: a folder only
/// becomes a vault once everything else is in place.
const VAULT_FILES: &[(&str, &str)] = &[
    (".gitignore", GITIGNORE),
    ("AGENTS.md", AGENTS_MD),
    ("CLAUDE.md", CLAUDE_MD),
    ("README.md", README_MD),
    ("profile/user.md", USER_MD),
    ("profile/rules.md", RULES_MD),
    (".reprise/vault.toml", VAULT_TOML),
];

/// Replaces each `{{key}}` in the template with its value, in one pass: text a value brings in is
/// never read as a placeholder, so a title holding `{{body}}` stays a title. Unknown keys stay as they
/// are.
pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}").and_then(|end| {
            values
                .iter()
                .find(|(k, _)| *k == &after[..end])
                .map(|v| (end, v.1))
        }) {
            Some((end, value)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Creates the layout in `root`. Existing files are kept, so a half-finished run can be repeated.
pub(crate) fn init_vault(root: &Path) -> Result<()> {
    for dir in ["projects", "notes", ".reprise/cache"] {
        create_dir(&root.join(dir))?;
    }
    for (rel, text) in VAULT_FILES {
        let path = root.join(rel);
        if !path.exists() {
            write(&path, text)?;
        }
    }
    Ok(())
}

/// Rewrites the managed agent guide when it differs from this binary's version.
pub(crate) fn refresh_guide(root: &Path) -> Result<()> {
    let path = root.join("AGENTS.md");
    if crate::safe::read_to_string(&path).ok().as_deref() != Some(AGENTS_MD) {
        write(&path, AGENTS_MD)?;
    }
    Ok(())
}

/// Makes sure `projects/<name>` exists with `project.toml`, `state.md` and `gotchas.md`. A project
/// without remotes takes `remote` as its first one. Returns whether anything was written.
pub fn ensure_project(
    vault: &Vault,
    name: &str,
    remote: Option<&str>,
    today: NaiveDate,
) -> Result<bool> {
    let dir = vault.project_dir(name);
    let mut wrote = false;
    let toml_path = dir.join("project.toml");
    let existing = ProjectToml::read(&toml_path);
    let needs_toml = match &existing {
        None => !toml_path.exists(),
        Some(cfg) => cfg.repos.is_empty() && remote.is_some(),
    };
    if needs_toml {
        let prefix = existing
            .as_ref()
            .map(|c| c.prefix(name))
            .unwrap_or_else(|| default_prefix(name));
        // A TOML string, escaped, whatever the remote holds.
        let repos = remote
            .map(|r| toml::Value::String(r.to_owned()).to_string())
            .unwrap_or_default();
        write(
            &toml_path,
            &render(PROJECT_TOML, &[("prefix", &prefix), ("repos", &repos)]),
        )?;
        wrote = true;
    }
    let date = today.format("%Y-%m-%d").to_string();
    for (file, template) in [("state.md", STATE_MD), ("gotchas.md", GOTCHAS_MD)] {
        let path = dir.join(file);
        if !path.exists() {
            write(
                &path,
                &render(template, &[("project", name), ("date", &date)]),
            )?;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// Whether `prefix` can start a record id and its file name: `[a-z0-9]`, then `[a-z0-9-]`, at most 40.
pub fn is_prefix(prefix: &str) -> bool {
    prefix.len() <= 40
        && prefix.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The project name folded to `[a-z0-9-]`; `p` when nothing is left.
pub fn default_prefix(name: &str) -> String {
    let prefix = crate::text::tokens(name).join("-");
    if prefix.is_empty() {
        "p".to_owned()
    } else {
        prefix
    }
}

fn create_dir(path: &Path) -> Result<()> {
    crate::safe::create_dir_all(path)
}

/// Writes through a temporary file and a rename, so a half-written file never remains; refuses a
/// symlink inside the vault (`safe::write`). Secrets are masked: the vault is pushed to a remote.
pub(crate) fn write(path: &Path, text: &str) -> Result<()> {
    crate::safe::write(path, &crate::secret::redact(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()
    }

    #[test]
    fn render_replaces_every_placeholder() {
        assert_eq!(
            render("{{a}}-{{b}}-{{a}}", &[("a", "1"), ("b", "2")]),
            "1-2-1"
        );
    }

    #[test]
    fn init_creates_the_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_vault(root).unwrap();
        for rel in [
            ".reprise/vault.toml",
            ".gitignore",
            "AGENTS.md",
            "CLAUDE.md",
            "README.md",
            "profile/user.md",
            "profile/rules.md",
        ] {
            assert!(root.join(rel).is_file(), "{rel}");
        }
        for rel in ["projects", "notes", ".reprise/cache"] {
            assert!(root.join(rel).is_dir(), "{rel}");
        }
        assert!(
            fs::read_dir(root.join(".reprise"))
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().contains(".reprise-tmp-"))
        );
    }

    #[test]
    fn init_keeps_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("profile")).unwrap();
        fs::write(tmp.path().join("profile/user.md"), "mine").unwrap();
        init_vault(tmp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("profile/user.md")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn refresh_rewrites_a_changed_guide() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("AGENTS.md");
        fs::write(&path, "old").unwrap();
        refresh_guide(tmp.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), AGENTS_MD);
    }

    #[test]
    fn ensure_project_creates_and_fills_remote_once() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = Vault::choose(tmp.path()).unwrap();

        assert!(ensure_project(&vault, "My App", None, day()).unwrap());
        let dir = vault.project_dir("My App");
        let cfg = ProjectToml::read(&dir.join("project.toml")).unwrap();
        assert_eq!(cfg.prefix("My App"), "my-app");
        assert!(cfg.repos.is_empty());
        let state = fs::read_to_string(dir.join("state.md")).unwrap();
        assert!(state.contains("project: My App\nupdated: 2026-10-25\n"));
        assert!(state.contains("## What\n"));
        assert!(!state.contains("{{"));

        assert!(ensure_project(&vault, "My App", Some("github.com/a/app"), day()).unwrap());
        let cfg = ProjectToml::read(&dir.join("project.toml")).unwrap();
        assert_eq!(cfg.repos, ["github.com/a/app"]);
        assert_eq!(cfg.prefix("My App"), "my-app", "prefix is kept");

        assert!(!ensure_project(&vault, "My App", Some("github.com/b/other"), day()).unwrap());
        let cfg = ProjectToml::read(&dir.join("project.toml")).unwrap();
        assert_eq!(
            cfg.repos,
            ["github.com/a/app"],
            "set remotes are not replaced"
        );
    }

    #[test]
    fn prefixes() {
        assert_eq!(default_prefix("reprise"), "reprise");
        assert_eq!(default_prefix("Web App"), "web-app");
        assert_eq!(default_prefix("Café_2"), "cafe-2");
        assert_eq!(default_prefix("—"), "p");
    }

    #[test]
    fn record_templates_have_fixed_headings() {
        for (template, headings) in [
            (
                ITEM_MD,
                &["Problem", "Cause", "Solution", "Verification", "Notes"][..],
            ),
            (DECISION_MD, &["Context", "Decision", "Why", "Alternatives"]),
            (
                SESSION_MD,
                &["Done", "Decisions", "Open", "Next step", "Touched records"],
            ),
            (
                STATE_MD,
                &[
                    "What",
                    "Architecture",
                    "Working",
                    "Not working",
                    "Run and test",
                ],
            ),
        ] {
            for h in headings {
                assert!(template.contains(&format!("\n## {h}\n")), "{h}");
                assert!(AGENTS_MD.contains(&format!("## {h}")) || template == STATE_MD);
            }
        }
    }

    /// A value is never read as a placeholder: a title holding `{{body}}` cannot pull the body into
    /// the frontmatter.
    #[test]
    fn render_fills_each_placeholder_once() {
        let out = render(
            "title: {{title}}\n---\n{{body}}\n{{unknown}}",
            &[("title", "x {{body}}"), ("body", "status: verified\n---")],
        );
        assert_eq!(
            out,
            "title: x {{body}}\n---\nstatus: verified\n---\n{{unknown}}"
        );
        assert_eq!(render("{{a}}{{", &[("a", "1")]), "1{{");
    }

    #[test]
    fn prefixes_are_file_name_safe() {
        for good in ["app", "web-app", "p", "x9"] {
            assert!(is_prefix(good), "{good}");
        }
        for bad in ["", "../x", "a/b", "-a", "App", "a b", "a.b"] {
            assert!(!is_prefix(bad), "{bad}");
        }
    }

    #[test]
    fn a_remote_is_written_as_an_escaped_toml_string() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        // normalize_remote refuses quotes; ensure_project escapes whatever it is given anyway.
        ensure_project(
            &vault,
            "app",
            Some("evil\", \"github.com/acme/api"),
            chrono::NaiveDate::MIN,
        )
        .unwrap();
        let cfg = crate::project::ProjectToml::read(&vault.project_dir("app").join("project.toml"))
            .unwrap();
        assert_eq!(
            cfg.repos,
            ["evil\", \"github.com/acme/api"],
            "one entry, not two"
        );
    }
}
