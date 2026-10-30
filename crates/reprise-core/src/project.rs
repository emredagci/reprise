//! Works out which project a working directory belongs to (docs/vault-format.md, "Projects").
//!
//! Candidates are the directory itself and, for a git worktree, the main repository root.
//! For each candidate, in order:
//! 1. inside the vault → no project (the vault is memory, not code),
//! 2. not accessible (manual mode, not under a listed folder) → skip,
//! 3. the repository's `origin` remote is listed in `projects/<name>/project.toml` → `<name>`,
//! 4. under a listed folder `F` (manual mode only; auto mode has no list):
//!    - `F` is itself a git repository → `F`'s name,
//!    - `F/<container>/<name>/...` → `<name>`; `F/<container>` alone → `<container>`,
//!    - `F/<name>/...` → `<name>`; `F` alone → no project.
//!
//! If nothing matched, in auto mode:
//! - a git repository is named after its main root (with the remote's owner appended when that name
//!   belongs to another remote),
//! - outside git, the nearest folder (the directory or one above it, below the home folder) named
//!   like a project the vault already has;
//!
//! otherwise there is no project.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{Access, AccessMode, Vault};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// The repository's remote is listed in a `project.toml`.
    Remote,
    /// The listed folder is itself the project's repository.
    Folder,
    /// First segment under a listed folder.
    FolderChild,
    Container,
    ContainerRoot,
    /// Auto mode, outside the listed folders: named after the git main root.
    Git,
    /// Auto mode, outside git: a folder named like a project the vault has.
    Named,
    /// Inside the vault: not a project.
    Vault,
    /// Accessible, but not a project.
    Outside,
    /// Manual mode and not under any listed folder.
    NotAccessible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    pub project: Option<String>,
    pub repo: Option<PathBuf>,
    /// The repository's normalized `origin` remote, when it has one.
    pub remote: Option<String>,
    pub rule: Rule,
}

impl Resolution {
    fn found(project: &str, repo: PathBuf, remote: Option<&str>, rule: Rule) -> Self {
        Self {
            project: Some(project.to_owned()),
            repo: Some(repo),
            remote: remote.map(str::to_owned),
            rule,
        }
    }

    fn none(rule: Rule) -> Self {
        Self {
            project: None,
            repo: None,
            remote: None,
            rule,
        }
    }
}

/// `projects/<name>/project.toml`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectToml {
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub repos: Vec<String>,
}

impl ProjectToml {
    /// `None` when the file is missing or invalid.
    pub fn read(path: &Path) -> Option<Self> {
        toml::from_str(&crate::safe::read_to_string(path).ok()?).ok()
    }

    /// The id prefix; derived from the project name when unset or not a valid prefix (it becomes part
    /// of file names, so a synced `project.toml` cannot steer a write out of the project's folder).
    pub fn prefix(&self, project: &str) -> String {
        self.prefix
            .as_deref()
            .map(str::trim)
            .filter(|p| crate::scaffold::is_prefix(p))
            .map_or_else(|| crate::scaffold::default_prefix(project), str::to_owned)
    }

    /// The listed remotes, normalized; entries that are not remotes are dropped.
    pub fn remotes(&self) -> Vec<String> {
        self.repos
            .iter()
            .filter_map(|r| normalize_remote(r))
            .collect()
    }
}

pub struct Resolver<'a> {
    vault: &'a Vault,
    access: &'a Access,
}

impl<'a> Resolver<'a> {
    pub fn new(vault: &'a Vault, access: &'a Access) -> Self {
        Self { vault, access }
    }

    pub fn resolve(&self, cwd: &Path) -> Resolution {
        let cwd = absolute(cwd);
        let main = git_main_root(&cwd);
        let remote = main.as_deref().and_then(origin_remote);
        let remote = remote.as_deref();
        let mut candidates = vec![cwd.clone()];
        if let Some(main) = main.clone().filter(|m| *m != cwd) {
            candidates.push(main);
        }

        let containers = self.vault.config().projects.containers;
        let owners = project_remotes(self.vault);
        let mut any_allowed = false;

        for path in &candidates {
            if path.starts_with(self.vault.root()) {
                return Resolution::none(Rule::Vault);
            }
            if !self.access.allows(path) {
                continue;
            }
            any_allowed = true;
            if let (Some(remote), Some(main)) = (remote, &main)
                && let Some((_, name)) = owners.iter().find(|(r, _)| r == remote)
            {
                return Resolution::found(name, main.clone(), Some(remote), Rule::Remote);
            }
            if let Some(folder) = self.access.folder_for(path)
                && let Some(mut found) = in_folder(folder, path, &containers)
            {
                if found.repo == main {
                    found.remote = remote.map(str::to_owned);
                }
                return found;
            }
        }

        match main {
            Some(main) if any_allowed && self.access.mode == AccessMode::Auto => {
                match main.file_name().and_then(|n| n.to_str()) {
                    Some(name) => {
                        let name = self.unclaimed_name(name, remote);
                        Resolution::found(&name, main.clone(), remote, Rule::Git)
                    }
                    None => Resolution::none(Rule::Outside),
                }
            }
            None if any_allowed && self.access.mode == AccessMode::Auto => {
                match self.named_folder(&cwd, dirs::home_dir().as_deref()) {
                    Some((name, folder)) => Resolution::found(&name, folder, None, Rule::Named),
                    None => Resolution::none(Rule::Outside),
                }
            }
            _ if any_allowed => Resolution::none(Rule::Outside),
            _ => Resolution::none(Rule::NotAccessible),
        }
    }

    /// The nearest of `cwd` and the folders above it, below `home` (never `home`, a folder above it or
    /// the root), whose name is a project in the vault. A project's `repos` do not rule it out: a plain
    /// folder holding a project's repositories (`shop/` around `shop-ios/`) is that project.
    fn named_folder(&self, cwd: &Path, home: Option<&Path>) -> Option<(String, PathBuf)> {
        let home = home.map(absolute);
        cwd.ancestors()
            .take_while(|dir| {
                dir.parent().is_some() && home.as_deref().is_none_or(|h| !h.starts_with(dir))
            })
            .find_map(|dir| {
                let name = dir.file_name()?.to_str()?;
                if name.starts_with('.') {
                    return None;
                }
                // Not through a link: the vault's own folder, nothing it points to.
                std::fs::symlink_metadata(self.vault.project_dir(name))
                    .is_ok_and(|m| m.is_dir())
                    .then(|| (name.to_owned(), dir.to_path_buf()))
            })
    }

    /// `name`, or `<name>-<owner>` when a project called `name` already belongs to other remotes.
    fn unclaimed_name(&self, name: &str, remote: Option<&str>) -> String {
        let Some(remote) = remote else {
            return name.to_owned();
        };
        let claimed = ProjectToml::read(&self.vault.project_dir(name).join("project.toml"))
            .map(|cfg| cfg.remotes())
            .is_some_and(|remotes| !remotes.is_empty() && !remotes.iter().any(|r| r == remote));
        let owner = remote.rsplit('/').nth(1);
        match owner {
            Some(owner) if claimed => format!("{name}-{owner}"),
            _ => name.to_owned(),
        }
    }
}

/// (normalized remote, project name) for every remote listed in a `project.toml`, sorted.
pub(crate) fn project_remotes(vault: &Vault) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(vault.projects_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(cfg) = ProjectToml::read(&entry.path().join("project.toml")) {
            out.extend(cfg.remotes().into_iter().map(|r| (r, name.clone())));
        }
    }
    out.sort();
    out
}

/// A git remote URL as `host/owner/name`, lowercase, so the ssh and https forms match:
/// `git@github.com:acme/api.git`, `https://github.com/acme/api` and `ssh://git@github.com:22/acme/api`
/// are all `github.com/acme/api`. Local paths and `file://` URLs are machine-specific and give `None`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme == "file" {
            return None;
        }
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?, path)
    } else if let Some((left, path)) = url.split_once(':').filter(|(l, _)| !l.contains('/')) {
        (left.rsplit_once('@').map_or(left, |(_, h)| h), path)
    } else {
        // Already normalized: `host/owner/name`, where the host has a dot (not a relative path).
        let (host, path) = url.split_once('/')?;
        if !host.contains('.') || host.starts_with('.') {
            return None;
        }
        (host, path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let remote = format!("{host}/{path}").to_lowercase();
    // Only what host names and repository paths use; a quote, comma or line break is no remote.
    remote
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._~/+-".contains(c))
        .then_some(remote)
}

/// The normalized `origin` remote of the repository at `repo`.
pub(crate) fn origin_remote(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    normalize_remote(String::from_utf8(out.stdout).ok()?.trim())
}

/// Project for `path` inside the listed folder `folder`, if any.
fn in_folder(folder: &Path, path: &Path, containers: &[String]) -> Option<Resolution> {
    if folder.join(".git").exists() {
        let name = folder.file_name()?.to_str()?;
        return Some(Resolution::found(
            name,
            folder.to_path_buf(),
            None,
            Rule::Folder,
        ));
    }
    let rel = path.strip_prefix(folder).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let is_container = |c: &str| containers.iter().any(|x| x == c);
    match parts.as_slice() {
        [] => None,
        [container, name, ..] if is_container(container) => Some(Resolution::found(
            name,
            folder.join(container).join(name),
            None,
            Rule::Container,
        )),
        [container] if is_container(container) => Some(Resolution::found(
            container,
            folder.join(container),
            None,
            Rule::ContainerRoot,
        )),
        [name, ..] => Some(Resolution::found(
            name,
            folder.join(name),
            None,
            Rule::FolderChild,
        )),
    }
}

/// Main working directory of the repository, worktrees included. `None` without git or outside a repo.
fn git_main_root(path: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    if common.file_name()? != ".git" {
        return None;
    }
    common.parent().map(absolute)
}

/// Real form of an existing path (e.g. /var → /private/var on macOS); other paths are left as they are.
fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Env {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        vault: Vault,
    }

    impl Env {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().canonicalize().unwrap();
            fs::create_dir_all(base.join("Projects")).unwrap();
            let vault = crate::vault::tests::new_vault(&base.join("Vault"));
            Self {
                _tmp: tmp,
                base,
                vault,
            }
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.base.join(rel);
            fs::create_dir_all(&p).unwrap();
            p
        }

        fn write(&self, rel: &str, text: &str) {
            let p = self.base.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }

        fn manual(&self, folders: &[&str]) -> Access {
            Access::manual(folders.iter().map(|f| self.dir(f)))
        }

        fn resolve_with(&self, access: &Access, rel: &str) -> Resolution {
            Resolver::new(&self.vault, access).resolve(&self.dir(rel))
        }

        /// Resolves `rel` in manual mode with only `Projects` listed.
        fn resolve(&self, rel: &str) -> Resolution {
            self.resolve_with(&self.manual(&["Projects"]), rel)
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn folder_child() {
        let env = Env::new();
        let r = env.resolve("Projects/foo/src/deep");
        assert_eq!(r.project.as_deref(), Some("foo"));
        assert_eq!(r.repo, Some(env.base.join("Projects/foo")));
        assert_eq!(r.rule, Rule::FolderChild);
    }

    #[test]
    fn listed_folder_itself_is_not_a_project() {
        let env = Env::new();
        assert_eq!(env.resolve("Projects").rule, Rule::Outside);
    }

    #[test]
    fn listed_folder_that_is_a_repo_is_the_project() {
        let env = Env::new();
        let repo = env.dir("Solo/app");
        init_repo(&repo);
        let r = env.resolve_with(&env.manual(&["Solo/app"]), "Solo/app/src/x");
        assert_eq!(r.project.as_deref(), Some("app"));
        assert_eq!(r.repo, Some(repo));
        assert_eq!(r.rule, Rule::Folder);
    }

    #[test]
    fn container_and_container_root() {
        let env = Env::new();
        env.write(
            "Vault/.reprise/vault.toml",
            "format = 2\nother = 1\n[projects]\ncontainers = [\"clients\"]\n",
        );

        let r = env.resolve("Projects/clients/bar/x");
        assert_eq!(r.project.as_deref(), Some("bar"));
        assert_eq!(r.repo, Some(env.base.join("Projects/clients/bar")));
        assert_eq!(r.rule, Rule::Container);

        let r = env.resolve("Projects/clients");
        assert_eq!(r.project.as_deref(), Some("clients"));
        assert_eq!(r.rule, Rule::ContainerRoot);
    }

    #[test]
    fn invalid_config_means_no_containers() {
        let env = Env::new();
        env.write("Vault/.reprise/vault.toml", "{broken");
        let r = env.resolve("Projects/clients/bar");
        assert_eq!(r.project.as_deref(), Some("clients"));
        assert_eq!(r.rule, Rule::FolderChild);
    }

    #[test]
    fn vault_is_not_a_project() {
        let env = Env::new();
        let r = env.resolve_with(&env.manual(&["Elsewhere"]), "Vault/projects/x");
        assert_eq!(r, Resolution::none(Rule::Vault));
        let r = env.resolve_with(&Access::auto(), "Vault");
        assert_eq!(r, Resolution::none(Rule::Vault));
    }

    #[test]
    fn auto_mode_finds_plain_folders_named_like_a_vault_project() {
        let env = Env::new();
        let auto = Access::auto();
        env.dir("Vault/projects/shop");
        env.dir("Projects/shop/scripts/deep");
        let r = env.resolve_with(&auto, "Projects/shop/scripts/deep");
        assert_eq!(r.project.as_deref(), Some("shop"));
        assert_eq!(r.repo, Some(env.base.join("Projects/shop")));
        assert_eq!(r.rule, Rule::Named);

        // Listing the repositories inside it does not change that.
        env.write(
            "Vault/projects/shop/project.toml",
            "repos = [\"github.com/acme/shop-ios\"]\n",
        );
        assert_eq!(
            env.resolve_with(&auto, "Projects/shop/scripts").rule,
            Rule::Named
        );
        // A folder no project is named after stays outside.
        env.dir("Projects/notes");
        assert_eq!(
            env.resolve_with(&auto, "Projects/notes"),
            Resolution::none(Rule::Outside)
        );
        // A git repository keeps its own name, even inside such a folder.
        let repo = env.dir("Projects/shop/shop-web");
        init_repo(&repo);
        assert_eq!(
            env.resolve_with(&auto, "Projects/shop/shop-web").rule,
            Rule::Git
        );
        // Manual mode has its listed folders instead.
        assert_eq!(
            env.resolve_with(&env.manual(&["Elsewhere"]), "Projects/shop"),
            Resolution::none(Rule::NotAccessible)
        );
    }

    #[test]
    fn the_home_folder_and_above_are_never_named_projects() {
        let env = Env::new();
        let auto = Access::auto();
        let resolver = Resolver::new(&env.vault, &auto);
        env.dir("Vault/projects/home");
        let home = env.dir("home");
        let cwd = env.dir("home/work");
        assert_eq!(resolver.named_folder(&cwd, Some(&home)), None);
        env.dir("Vault/projects/work");
        assert_eq!(
            resolver.named_folder(&cwd, Some(&home)),
            Some(("work".to_owned(), cwd.clone()))
        );
    }

    #[test]
    fn manual_mode_refuses_unlisted_folders() {
        let env = Env::new();
        let repo = env.dir("Work/api");
        init_repo(&repo);
        assert_eq!(
            env.resolve("Work/api"),
            Resolution::none(Rule::NotAccessible)
        );
    }

    #[test]
    fn auto_mode_names_unlisted_git_repos_and_skips_plain_folders() {
        let env = Env::new();
        let repo = env.dir("Work/api");
        init_repo(&repo);
        let auto = Access::auto();

        let r = env.resolve_with(&auto, "Work/api/src");
        assert_eq!(r.project.as_deref(), Some("api"));
        assert_eq!(r.repo, Some(repo));
        assert_eq!(r.rule, Rule::Git);

        assert_eq!(
            env.resolve_with(&auto, "Downloads/stuff"),
            Resolution::none(Rule::Outside)
        );
    }

    #[test]
    fn auto_mode_ignores_listed_folders() {
        let env = Env::new();
        let repo = env.dir("Projects/foo");
        init_repo(&repo);
        let auto = Access {
            mode: AccessMode::Auto,
            folders: vec![env.dir("Projects")],
        };
        let r = env.resolve_with(&auto, "Projects/foo/src");
        assert_eq!(r.project.as_deref(), Some("foo"));
        assert_eq!(r.rule, Rule::Git);
        assert_eq!(
            env.resolve_with(&auto, "Projects/notes").rule,
            Rule::Outside,
            "a plain folder is not a project in auto mode"
        );
    }

    #[test]
    fn listed_remote_wins_but_needs_access() {
        let env = Env::new();
        let repo = env.dir("elsewhere/api");
        init_repo(&repo);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:Acme/api.git"],
        );
        env.write(
            "Vault/projects/baz/project.toml",
            "prefix = \"baz\"\nrepos = [\"https://github.com/acme/api\"]\n",
        );
        env.write("Vault/projects/bad/project.toml", "broken [");

        let r = env.resolve_with(&env.manual(&["elsewhere"]), "elsewhere/api/src");
        assert_eq!(r.project.as_deref(), Some("baz"));
        assert_eq!(r.repo, Some(repo));
        assert_eq!(r.remote.as_deref(), Some("github.com/acme/api"));
        assert_eq!(r.rule, Rule::Remote);

        assert_eq!(env.resolve("elsewhere/api/src").rule, Rule::NotAccessible);
    }

    #[test]
    fn auto_mode_appends_owner_when_name_belongs_to_another_remote() {
        let env = Env::new();
        let auto = Access::auto();
        env.write(
            "Vault/projects/api/project.toml",
            "repos = [\"github.com/acme/api\"]\n",
        );
        let repo = env.dir("Work/api");
        init_repo(&repo);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/someone/api.git",
            ],
        );
        let r = env.resolve_with(&auto, "Work/api");
        assert_eq!(r.project.as_deref(), Some("api-someone"));
        assert_eq!(r.remote.as_deref(), Some("github.com/someone/api"));
        assert_eq!(r.rule, Rule::Git);

        let plain = env.dir("Other/api");
        init_repo(&plain);
        let r = env.resolve_with(&auto, "Other/api");
        assert_eq!(
            r.project.as_deref(),
            Some("api"),
            "no remote: nothing to compare"
        );
    }

    #[test]
    fn folder_rules_carry_the_remote() {
        let env = Env::new();
        let repo = env.dir("Projects/foo");
        init_repo(&repo);
        git(
            &repo,
            &["remote", "add", "origin", "git@gitlab.com:me/foo.git"],
        );
        let r = env.resolve("Projects/foo/src");
        assert_eq!(r.rule, Rule::FolderChild);
        assert_eq!(r.remote.as_deref(), Some("gitlab.com/me/foo"));
    }

    #[test]
    fn remotes_normalize() {
        for (url, want) in [
            ("git@github.com:Acme/API.git", Some("github.com/acme/api")),
            ("https://github.com/acme/api", Some("github.com/acme/api")),
            (
                "https://github.com/acme/api.git/",
                Some("github.com/acme/api"),
            ),
            (
                "https://user:tok@github.com/acme/api.git",
                Some("github.com/acme/api"),
            ),
            (
                "ssh://git@github.com:22/acme/api.git",
                Some("github.com/acme/api"),
            ),
            (
                "git@host.example:group/sub/repo",
                Some("host.example/group/sub/repo"),
            ),
            ("github.com/Acme/api", Some("github.com/acme/api")),
            ("./repo/x", None),
            ("/srv/git/repo.git", None),
            ("file:///srv/git/repo.git", None),
            ("../repo", None),
            ("", None),
        ] {
            assert_eq!(normalize_remote(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn worktree_outside_listed_folder_resolves_to_main_repo() {
        let env = Env::new();
        let repo = env.dir("Projects/foo");
        init_repo(&repo);
        let wt = env.base.join("worktrees/foo-wt");
        git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);

        let r = env.resolve("worktrees/foo-wt");
        assert_eq!(r.project.as_deref(), Some("foo"));
        assert_eq!(r.repo, Some(repo));
        assert_eq!(r.rule, Rule::FolderChild);
    }

    #[test]
    fn remotes_with_toml_or_shell_characters_are_no_remotes() {
        assert_eq!(
            normalize_remote("git@github.com:acme/api.git").as_deref(),
            Some("github.com/acme/api")
        );
        for bad in [
            "https://github.com/acme/api\", \"github.com/other/repo",
            "git@github.com:acme/a\nb",
            "https://github.com/acme/a b",
        ] {
            assert_eq!(normalize_remote(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_invalid_prefix_falls_back_to_the_project_name() {
        let cfg: ProjectToml = toml::from_str("prefix = \"../../x\"").unwrap();
        assert_eq!(cfg.prefix("web app"), "web-app");
    }
}
