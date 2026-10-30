//! Connecting reprise to Claude Code. Everything goes through the `reprise` CLI:
//!
//! 1. SessionStart, UserPromptSubmit, SessionEnd, PostToolUse (Edit, Write, MultiEdit) and PreCompact hooks in `settings.json` run
//!    `reprise hook …`,
//! 2. `PERMISSIONS` in `permissions.allow`: agents read the memory and write records without a prompt
//!    each time; everything else reprise does (choosing the vault and folders, connecting, the sweep and
//!    its schedule, running a verify command, putting a proposed rule in effect) still asks the user,
//! 3. `~/.local/bin/reprise`, a link to the CLI, so `reprise` works in any shell, unless `reprise` is already on PATH.
//!
//! The hooks run the `reprise` on PATH when that is the running program (Homebrew's `/opt/homebrew/bin/reprise`,
//! which stays put while upgrades move the file it points to), else the running program itself. There is
//! one reprise hook per event: connecting again from another reprise points the hooks at it in place, and so
//! does any reprise command once the reprise they ran is gone and PATH finds this one.
//!
//! Nothing is written to `CLAUDE.md`: what an agent needs to know arrives with the session-start hook,
//! which points at the vault's own `AGENTS.md`.
//!
//! What was actually added is recorded in a manifest next to reprise's settings; `disconnect` removes
//! exactly that. Things that already existed (the permission, an `reprise` on PATH) are left alone and
//! not recorded. Before the first change, each file that will change is copied to
//! `*.reprise-backup`. If a step fails, the steps before it are undone.

use std::path::{Path, PathBuf};

use reprise_core::Settings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::io;
use crate::{Change, Error, Refresh, Result};

/// What an agent may run without asking: reading the memory and writing records. Not setup, access,
/// connect, import, sweep, `item verify` (it runs the command a record names), `rule accept`, or
/// `engine set`: those change what every later session sees, and stay the user's call.
pub const PERMISSIONS: [&str; 31] = [
    "Bash(reprise project)",
    "Bash(reprise project *)",
    "Bash(reprise overview)",
    "Bash(reprise overview *)",
    "Bash(reprise items)",
    "Bash(reprise items *)",
    "Bash(reprise decisions)",
    "Bash(reprise decisions *)",
    "Bash(reprise sessions)",
    "Bash(reprise sessions *)",
    "Bash(reprise graph)",
    "Bash(reprise doctor)",
    "Bash(reprise doctor *)",
    "Bash(reprise clients)",
    "Bash(reprise engine)",
    "Bash(reprise item add *)",
    "Bash(reprise item set *)",
    "Bash(reprise decision add *)",
    "Bash(reprise decision set *)",
    "Bash(reprise session write *)",
    "Bash(reprise rule list)",
    "Bash(reprise fact set *)",
    "Bash(reprise fact end *)",
    "Bash(reprise fact list)",
    "Bash(reprise fact list *)",
    "Bash(reprise event add *)",
    "Bash(reprise event list)",
    "Bash(reprise event list *)",
    "Bash(reprise timeline)",
    "Bash(reprise timeline *)",
    "Bash(reprise search *)",
];
const HOOK_TIMEOUT: u64 = 10;
/// Event, `reprise hook` argument, and the tools it is limited to (Claude Code's `matcher`).
const EVENTS: [(&str, &str, Option<&str>); 5] = [
    ("SessionStart", "session-start", None),
    ("UserPromptSubmit", "prompt", None),
    ("SessionEnd", "session-end", None),
    ("PostToolUse", "tool-use", Some("Edit|Write|MultiEdit")),
    ("PreCompact", "pre-compact", None),
];

/// Where connecting writes. Tests point these at temporary folders.
#[derive(Debug, Clone)]
pub struct Targets {
    /// Claude Code's config folder: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude_dir: PathBuf,
    /// Folder for the `reprise` link: `$REPRISE_BIN_DIR`, else `~/.local/bin`.
    pub bin_dir: PathBuf,
    /// Record of what `connect` added, beside reprise's settings file.
    pub manifest: PathBuf,
}

impl Targets {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| Error::Conflict("no home folder".into()))?;
        let settings = Settings::default_path()?;
        let base = settings
            .parent()
            .map_or_else(|| home.clone(), Path::to_path_buf);
        Ok(Self {
            claude_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            bin_dir: reprise_core::env_var("BIN_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/bin")),
            manifest: base.join("connections/claude.json"),
        })
    }

    fn settings(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }

    fn link(&self) -> PathBuf {
        self.bin_dir
            .join(format!("reprise{}", std::env::consts::EXE_SUFFIX))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    /// The reprise the hooks run.
    reprise: PathBuf,
    /// Hook commands added to `settings.json`, by event.
    hooks: Vec<(String, String)>,
    /// The rules of `PERMISSIONS` reprise added.
    permissions: Vec<String>,
    /// The link created, if any.
    link: Option<PathBuf>,
}

fn read_manifest(t: &Targets) -> Result<Option<(Manifest, String)>> {
    let text = match std::fs::read_to_string(&t.manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(&t.manifest)(e)),
    };
    let manifest = serde_json::from_str(&text).map_err(|source| Error::Json {
        path: t.manifest.clone(),
        source,
    })?;
    Ok(Some((manifest, text)))
}

/// The reprise the connection's hooks run, from its manifest.
pub fn hooks_run(t: &Targets) -> Result<Option<PathBuf>> {
    Ok(read_manifest(t)?.map(|(m, _)| m.reprise))
}

pub fn hook_command(reprise: &Path, event: &str) -> String {
    format!(
        "{} hook {event}",
        shell_quote(&reprise.display().to_string())
    )
}

fn shell_quote(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+:@".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The reprise and argument of a command `hook_command` wrote, whichever reprise it runs; `None` for anything
/// else.
fn parse_hook(cmd: &str) -> Option<(PathBuf, &str)> {
    let (program, arg) = cmd.rsplit_once(" hook ")?;
    let program = match program
        .strip_prefix('\'')
        .and_then(|p| p.strip_suffix('\''))
    {
        Some(quoted) => quoted.replace(r"'\''", "'"),
        None => program.to_owned(),
    };
    let path = PathBuf::from(program);
    let named_reprise = path.file_stem().is_some_and(|s| s == "reprise");
    let one_word = !arg.is_empty() && arg.chars().all(|c| c.is_ascii_lowercase() || c == '-');
    (named_reprise && one_word && path.is_absolute() && hook_command(&path, arg) == cmd)
        .then_some((path, arg))
}

// --- which reprise -----------------------------------------------------------------------------

/// The reprise the hooks run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reprise {
    path: PathBuf,
    /// Found as `reprise` on PATH, so no link is needed.
    on_path: bool,
}

/// The path the hooks run for the running reprise `exe`: the first `reprise` that `which` finds (PATH order) that
/// is this same program, else `exe` itself. reprise's own link does not count, since disconnect removes it;
/// a program installed in its place does.
fn locate<I: IntoIterator<Item = PathBuf>>(
    t: &Targets,
    exe: &Path,
    which: impl FnOnce() -> I,
) -> Reprise {
    let real = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let link = t.link();
    let our_link = |p: &Path| {
        p == link
            && p.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
    };
    which()
        .into_iter()
        .find(|found| !our_link(found) && found.canonicalize().is_ok_and(|f| f == real))
        .map_or(
            Reprise {
                path: real,
                on_path: false,
            },
            |found| Reprise {
                path: found,
                on_path: true,
            },
        )
}

fn which_reprise() -> impl Iterator<Item = PathBuf> {
    crate::path::find_all("reprise")
}

/// The path to run the running reprise `exe` by from outside a shell (hooks, a scheduled job), and whether
/// PATH found it there, as `connect` chooses it.
pub fn runnable_reprise(exe: &Path) -> Result<(PathBuf, bool)> {
    let reprise = locate(&Targets::from_env()?, exe, which_reprise);
    Ok((reprise.path, reprise.on_path))
}

// --- working out a connection --------------------------------------------------------------

/// What connecting does to one event's hooks.
#[derive(Debug)]
struct HookChange {
    event: &'static str,
    cmd: String,
    /// `cmd` was already there.
    present: bool,
    /// reprise hooks for the same event that run another reprise: the first becomes `cmd` (unless `cmd` was
    /// there), the rest go, so the hook runs once.
    replaced: Vec<String>,
}

/// What happens to reprise's link on PATH.
#[derive(Debug, PartialEq, Eq)]
enum LinkPlan {
    Keep,
    Create,
    /// The link to the reprise the hooks ran before goes: PATH finds `reprise` without it.
    Remove {
        old: PathBuf,
    },
    /// The link to the reprise the hooks ran before points to the new one.
    Repoint {
        old: PathBuf,
    },
}

/// Everything `connect` writes, worked out before writing anything.
struct Work {
    reprise: Reprise,
    settings: Map<String, Value>,
    hooks: Vec<HookChange>,
    /// Rules of `PERMISSIONS` added now.
    permissions_added: Vec<String>,
    link: LinkPlan,
    manifest: Manifest,
    /// The connection as it was, and its manifest's text, to put back if a step fails.
    before: Option<(Manifest, String)>,
}

impl Work {
    fn settings_changed(&self) -> bool {
        !self.permissions_added.is_empty()
            || self
                .hooks
                .iter()
                .any(|h| !h.present || !h.replaced.is_empty())
    }
}

/// Works out connecting `reprise`, or pointing an existing connection at it. Without `links`, the link and
/// what the manifest says about it stay as they are.
fn prepare(t: &Targets, reprise: &Reprise, links: bool) -> Result<Work> {
    let before = read_manifest(t)?;
    let mut settings = read_json(&t.settings())?;
    let hooks = retarget_hooks(&mut settings, &reprise.path);
    let old = before.as_ref().map(|(m, _)| m);
    let mut permissions_added = Vec::new();
    for rule in PERMISSIONS {
        if !has_permission_named(&settings, rule) {
            allow_list(&mut settings).push(Value::String(rule.into()));
            permissions_added.push(rule.to_owned());
        }
    }

    let link = if links {
        link_plan(t, reprise, old)
    } else {
        LinkPlan::Keep
    };
    let mut manifest = old.cloned().unwrap_or(Manifest {
        reprise: reprise.path.clone(),
        hooks: Vec::new(),
        permissions: Vec::new(),
        link: None,
    });
    manifest.reprise = reprise.path.clone();
    record_hooks(&mut manifest, &hooks);
    manifest
        .permissions
        .retain(|p| has_permission_named(&settings, p));
    for p in &permissions_added {
        if !manifest.permissions.contains(p) {
            manifest.permissions.push(p.clone());
        }
    }
    if links {
        manifest.link = match &link {
            LinkPlan::Create | LinkPlan::Repoint { .. } => Some(t.link()),
            LinkPlan::Remove { .. } => None,
            LinkPlan::Keep => manifest
                .link
                .filter(|l| link_state(l, &reprise.path) == LinkState::Ours),
        };
    }
    Ok(Work {
        reprise: reprise.clone(),
        settings,
        hooks,
        permissions_added,
        link,
        manifest,
        before,
    })
}

/// Points each event's reprise hook at `reprise`. An event without one gets it.
fn retarget_hooks(settings: &mut Map<String, Value>, reprise: &Path) -> Vec<HookChange> {
    EVENTS
        .iter()
        .map(|&(event, arg, matcher)| {
            let cmd = hook_command(reprise, arg);
            let mut present = false;
            let mut replaced = Vec::new();
            for c in commands(settings, event) {
                if c == cmd {
                    present = true;
                } else if parse_hook(&c).is_some_and(|(_, a)| a == arg) {
                    replaced.push(c);
                }
            }
            if !replaced.is_empty() {
                replace_commands(settings, event, &replaced, (!present).then_some(&cmd));
            } else if !present {
                add_hook(settings, event, &cmd, matcher);
            }
            HookChange {
                event,
                cmd,
                present,
                replaced,
            }
        })
        .collect()
}

/// Records the hooks reprise now owns: one it added, or one it put in place of a hook it had added. A hook
/// that was there before, as it is, stays unrecorded.
fn record_hooks(manifest: &mut Manifest, changes: &[HookChange]) {
    for c in changes {
        let recorded = |cmd: &str| manifest.hooks.iter().any(|(e, h)| e == c.event && h == cmd);
        let ours = recorded(&c.cmd) || c.replaced.iter().any(|r| recorded(r));
        manifest
            .hooks
            .retain(|(e, h)| !(e == c.event && (*h == c.cmd || c.replaced.contains(h))));
        if !c.present || ours {
            manifest.hooks.push((c.event.to_owned(), c.cmd.clone()));
        }
    }
}

fn link_plan(t: &Targets, reprise: &Reprise, before: Option<&Manifest>) -> LinkPlan {
    let link = t.link();
    // The link reprise made to the reprise the hooks ran before, still pointing there.
    let ours_before = before
        .filter(|m| m.link.is_some() && link_state(&link, &m.reprise) == LinkState::Ours)
        .map(|m| m.reprise.clone());
    match (link_state(&link, &reprise.path), ours_before) {
        (LinkState::Ours, _) => LinkPlan::Keep,
        (_, Some(old)) if reprise.on_path => LinkPlan::Remove { old },
        (_, Some(old)) => LinkPlan::Repoint { old },
        (LinkState::Missing, None) if !reprise.on_path => LinkPlan::Create,
        _ => LinkPlan::Keep,
    }
}

/// Writes `work`; if a step fails, puts back what the steps before it changed.
fn apply(t: &Targets, work: &Work) -> Result<()> {
    let settings_path = t.settings();
    let original_settings = std::fs::read(&settings_path).ok();
    let link = t.link();
    let result = (|| -> Result<()> {
        if work.settings_changed() {
            backup(&settings_path)?;
            write_json(&settings_path, &work.settings, original_settings.as_deref())?;
        }
        match &work.link {
            LinkPlan::Keep => {}
            LinkPlan::Create => make_link(&work.reprise.path, &link)?,
            LinkPlan::Remove { .. } => std::fs::remove_file(&link).map_err(io(&link))?,
            LinkPlan::Repoint { .. } => {
                std::fs::remove_file(&link).map_err(io(&link))?;
                make_link(&work.reprise.path, &link)?;
            }
        }
        let text = serde_json::to_string_pretty(&work.manifest).expect("manifest serializes");
        write_atomic(&t.manifest, text.as_bytes())
    })();
    if result.is_err() {
        restore(&settings_path, original_settings.as_deref());
        match &work.link {
            LinkPlan::Keep => {}
            LinkPlan::Create => {
                let _ = std::fs::remove_file(&link);
            }
            LinkPlan::Remove { old } | LinkPlan::Repoint { old } => {
                let _ = std::fs::remove_file(&link);
                let _ = make_link(old, &link);
            }
        }
        restore(
            &t.manifest,
            work.before.as_ref().map(|(_, text)| text.as_bytes()),
        );
    }
    result
}

// --- connect, refresh, disconnect ----------------------------------------------------------

/// What `connect` would change right now, for the running reprise `exe`.
pub fn plan(t: &Targets, exe: &Path) -> Result<Vec<Change>> {
    plan_for(t, &locate(t, exe, which_reprise))
}

fn plan_for(t: &Targets, reprise: &Reprise) -> Result<Vec<Change>> {
    let work = prepare(t, reprise, true)?;
    let settings_path = tilde(&t.settings());
    let mut changes = Vec::new();
    if let Some((before, _)) = &work.before
        && before.reprise != reprise.path
    {
        changes.push(Change {
            title: "Move the connection".into(),
            detail: format!(
                "The hooks run {}; they will run {} instead",
                before.reprise.display(),
                reprise.path.display()
            ),
        });
    }
    changes.push(Change {
        title: "Add hooks".into(),
        detail: work
            .hooks
            .iter()
            .map(|h| {
                let note = match (h.present, h.replaced.is_empty()) {
                    (true, true) => " (already there)".to_owned(),
                    (false, true) => String::new(),
                    (true, false) => format!(" (already there; removes {})", h.replaced.join(", ")),
                    (false, false) => format!(" (replaces {})", h.replaced.join(", ")),
                };
                format!("{settings_path} · {}: {}{note}", h.event, h.cmd)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    });
    changes.push(Change {
        title: "Allow the reprise CLI".into(),
        detail: if work.permissions_added.is_empty() {
            format!("{settings_path} · reading the memory and writing records are already allowed")
        } else {
            format!(
                "{settings_path} · permissions.allow gets {}",
                work.permissions_added.join(", ")
            )
        },
    });
    let link = t.link();
    changes.push(Change {
        title: "Put reprise on PATH".into(),
        detail: match &work.link {
            LinkPlan::Create => {
                let mut d = format!("{} → {}", tilde(&link), reprise.path.display());
                if !crate::path::on_user_path(&t.bin_dir) {
                    d.push_str(&format!(" ({} is not on your PATH)", tilde(&t.bin_dir)));
                }
                d
            }
            LinkPlan::Remove { old } => format!(
                "{} (to {}) is removed: {} is on your PATH",
                tilde(&link),
                old.display(),
                reprise.path.display()
            ),
            LinkPlan::Repoint { old } => format!(
                "{} points to {} instead of {}",
                tilde(&link),
                reprise.path.display(),
                old.display()
            ),
            LinkPlan::Keep => match link_state(&link, &reprise.path) {
                LinkState::Ours => format!("{} already points to this reprise", tilde(&link)),
                _ if reprise.on_path => {
                    format!("{} is already on your PATH", reprise.path.display())
                }
                _ => format!("{} already exists and is left as it is", tilde(&link)),
            },
        },
    });
    changes.push(Change {
        title: "Backups".into(),
        detail: format!(
            "{} copied to *.reprise-backup before the first change",
            tilde(&t.settings())
        ),
    });
    Ok(changes)
}

/// Connects the running reprise `exe`, or points an existing connection at it.
pub fn connect(t: &Targets, exe: &Path) -> Result<()> {
    connect_as(t, &locate(t, exe, which_reprise))
}

fn connect_as(t: &Targets, reprise: &Reprise) -> Result<()> {
    apply(t, &prepare(t, reprise, true)?)
}

/// Keeps a connection working. While the reprise it runs is there, it gets the hooks of `EVENTS` it is
/// missing, recorded so `disconnect` removes them too, and nothing else changes. Once that reprise is
/// gone, the connection moves to the running reprise `exe`, but only when PATH finds it: a build run from
/// its folder does not take over the hooks. Without a connection it does nothing.
pub fn refresh(t: &Targets, exe: &Path) -> Result<Refresh> {
    refresh_with(t, exe, which_reprise)
}

fn refresh_with<I: IntoIterator<Item = PathBuf>>(
    t: &Targets,
    exe: &Path,
    which: impl FnOnce() -> I,
) -> Result<Refresh> {
    let Some((before, _)) = read_manifest(t)? else {
        return Ok(Refresh::Unchanged);
    };
    if before.reprise.exists() {
        let current = Reprise {
            path: before.reprise,
            on_path: false,
        };
        let work = prepare(t, &current, false)?;
        if !work.settings_changed() {
            return Ok(Refresh::Unchanged);
        }
        apply(t, &work)?;
        return Ok(Refresh::Updated);
    }
    let reprise = locate(t, exe, which);
    if !reprise.on_path {
        return Ok(Refresh::Broken {
            reprise: before.reprise,
        });
    }
    apply(t, &prepare(t, &reprise, true)?)?;
    Ok(Refresh::Moved {
        from: before.reprise,
        to: reprise.path,
    })
}

pub fn disconnect(t: &Targets) -> Result<()> {
    let Some((manifest, _)) = read_manifest(t)? else {
        return Ok(());
    };

    let settings_path = t.settings();
    if !manifest.hooks.is_empty() || !manifest.permissions.is_empty() {
        let original = std::fs::read(&settings_path).ok();
        let mut settings = read_json(&settings_path)?;
        for (event, cmd) in &manifest.hooks {
            remove_hook(&mut settings, event, cmd);
        }
        drop_empty_hooks(&mut settings);
        for p in &manifest.permissions {
            remove_permission(&mut settings, p);
        }
        write_json(&settings_path, &settings, original.as_deref())?;
    }

    if let Some(link) = &manifest.link
        && link_state(link, &manifest.reprise) == LinkState::Ours
    {
        std::fs::remove_file(link).map_err(io(link))?;
    }
    std::fs::remove_file(&t.manifest).map_err(io(&t.manifest))
}

// --- settings.json -------------------------------------------------------------------------

fn read_json(path: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(io(path)(e)),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(Error::Conflict(format!(
            "{} is not a JSON object",
            path.display()
        ))),
        Err(source) => Err(Error::Json {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Pretty JSON with two-space indent, keeping a trailing newline if the original had one.
fn write_json(path: &Path, map: &Map<String, Value>, original: Option<&[u8]>) -> Result<()> {
    let mut text = serde_json::to_string_pretty(map).expect("JSON serializes");
    if original.is_none_or(|o| o.ends_with(b"\n")) {
        text.push('\n');
    }
    write_atomic(path, text.as_bytes())
}

fn groups<'a>(settings: &'a Map<String, Value>, event: &str) -> impl Iterator<Item = &'a Value> {
    settings
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// Every hook command of `event`, in order.
fn commands(settings: &Map<String, Value>, event: &str) -> Vec<String> {
    groups(settings, event)
        .filter_map(|g| g.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
fn has_hook(settings: &Map<String, Value>, event: &str, cmd: &str) -> bool {
    commands(settings, event).iter().any(|c| c == cmd)
}

/// The first hook running one of `old` gets `new` as its command, in place, keeping its group, matcher
/// and timeout; the other hooks running one of `old` go, and a group left empty goes too. Without
/// `new`, they all go.
fn replace_commands(
    settings: &mut Map<String, Value>,
    event: &str,
    old: &[String],
    mut new: Option<&String>,
) {
    let Some(list) = settings
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .and_then(|h| h.get_mut(event))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for group in list.iter_mut() {
        if let Some(hs) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            hs.retain_mut(|h| {
                let stale = h
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| old.iter().any(|o| o == c));
                if !stale {
                    return true;
                }
                match new.take() {
                    Some(cmd) => {
                        h["command"] = json!(cmd);
                        true
                    }
                    None => false,
                }
            });
        }
    }
    list.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|hs| !hs.is_empty())
    });
}

fn add_hook(settings: &mut Map<String, Value>, event: &str, cmd: &str, matcher: Option<&str>) {
    let hooks = settings.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let list = hooks
        .as_object_mut()
        .expect("checked above")
        .entry(event)
        .or_insert_with(|| json!([]));
    if !list.is_array() {
        *list = json!([]);
    }
    let mut group = json!({
        "hooks": [{ "type": "command", "command": cmd, "timeout": HOOK_TIMEOUT }]
    });
    if let Some(m) = matcher {
        group["matcher"] = json!(m);
    }
    list.as_array_mut().expect("checked above").push(group);
}

/// Removes the hook with `cmd`; a group left empty goes, then an empty event, then empty `hooks`.
fn remove_hook(settings: &mut Map<String, Value>, event: &str, cmd: &str) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(list) = hooks.get_mut(event).and_then(Value::as_array_mut) {
        for group in list.iter_mut() {
            if let Some(hs) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                hs.retain(|h| h.get("command").and_then(Value::as_str) != Some(cmd));
            }
        }
        list.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|hs| !hs.is_empty())
        });
    }
}

/// Drops empty event lists, then an empty `hooks` object.
fn drop_empty_hooks(settings: &mut Map<String, Value>) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    hooks.retain(|_, list| list.as_array().is_none_or(|l| !l.is_empty()));
    if hooks.is_empty() {
        settings.remove("hooks");
    }
}

/// Every rule of `PERMISSIONS` is there.
#[cfg(test)]
fn has_permission(settings: &Map<String, Value>) -> bool {
    PERMISSIONS
        .iter()
        .all(|p| has_permission_named(settings, p))
}

fn has_permission_named(settings: &Map<String, Value>, permission: &str) -> bool {
    settings
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(permission)))
}

fn allow_list(settings: &mut Map<String, Value>) -> &mut Vec<Value> {
    let perms = settings.entry("permissions").or_insert_with(|| json!({}));
    if !perms.is_object() {
        *perms = json!({});
    }
    let allow = perms
        .as_object_mut()
        .expect("checked above")
        .entry("allow")
        .or_insert_with(|| json!([]));
    if !allow.is_array() {
        *allow = json!([]);
    }
    allow.as_array_mut().expect("checked above")
}

/// Removes the permission; an `allow` (and `permissions`) left empty goes too.
fn remove_permission(settings: &mut Map<String, Value>, permission: &str) {
    let Some(perms) = settings
        .get_mut("permissions")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if let Some(allow) = perms.get_mut("allow").and_then(Value::as_array_mut) {
        allow.retain(|v| v.as_str() != Some(permission));
        if allow.is_empty() {
            perms.remove("allow");
        }
    }
    if perms.is_empty() {
        settings.remove("permissions");
    }
}

// --- link, files ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum LinkState {
    Missing,
    Ours,
    Other,
}

fn link_state(link: &Path, reprise: &Path) -> LinkState {
    match std::fs::read_link(link) {
        Ok(target) if target == reprise => LinkState::Ours,
        Ok(_) => LinkState::Other,
        Err(_) if link.symlink_metadata().is_ok() => LinkState::Other,
        Err(_) => LinkState::Missing,
    }
}

#[cfg(unix)]
fn make_link(reprise: &Path, link: &Path) -> Result<()> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    std::os::unix::fs::symlink(reprise, link).map_err(io(link))
}

#[cfg(not(unix))]
fn make_link(_reprise: &Path, _link: &Path) -> Result<()> {
    Ok(())
}

/// Copies `path` to `<path>.reprise-backup` once; an existing backup is kept.
fn backup(path: &Path) -> Result<()> {
    let target = PathBuf::from(format!("{}.reprise-backup", path.display()));
    if path.exists() && !target.exists() {
        std::fs::copy(path, &target).map_err(io(&target))?;
    }
    Ok(())
}

fn restore(path: &Path, original: Option<&[u8]>) {
    let _ = match original {
        Some(bytes) => std::fs::write(path, bytes),
        None => std::fs::remove_file(path),
    };
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    let tmp = PathBuf::from(format!("{}.reprise-tmp", path.display()));
    std::fs::write(&tmp, bytes).map_err(io(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io(path))
}

fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Connecting never writes `CLAUDE.md`; the tests check it stays as it was.
    fn claude_md(t: &Targets) -> PathBuf {
        t.claude_dir.join("CLAUDE.md")
    }

    struct Env {
        _tmp: tempfile::TempDir,
        t: Targets,
        reprise: PathBuf,
    }

    impl Env {
        /// The reprise under test, run from its own folder (not on PATH).
        fn me(&self) -> Reprise {
            Reprise {
                path: self.reprise.clone(),
                on_path: false,
            }
        }
    }

    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let reprise = root.join("build/reprise");
        std::fs::create_dir_all(reprise.parent().unwrap()).unwrap();
        std::fs::write(&reprise, "#!/bin/sh\n").unwrap();
        let t = Targets {
            claude_dir: root.join(".claude"),
            bin_dir: root.join(".local/bin"),
            manifest: root.join("reprise/connections/claude.json"),
        };
        std::fs::create_dir_all(&t.claude_dir).unwrap();
        Env {
            _tmp: tmp,
            t,
            reprise,
        }
    }

    const SETTINGS: &str = r#"{
  "model": "opus",
  "permissions": {
    "additionalDirectories": [
      "/Users/x/notes"
    ]
  },
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "other-tool session-start",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
"#;

    #[test]
    fn connect_then_disconnect_restores_everything() {
        let e = env();
        std::fs::write(e.t.settings(), SETTINGS).unwrap();
        std::fs::write(claude_md(&e.t), "# Mine\n\nKeep this.\n").unwrap();

        connect_as(&e.t, &e.me()).unwrap();
        let s = read_json(&e.t.settings()).unwrap();
        let start = hook_command(&e.reprise, "session-start");
        assert!(has_hook(&s, "SessionStart", &start));
        assert!(
            has_hook(&s, "SessionStart", "other-tool session-start"),
            "other hooks are kept"
        );
        assert!(has_hook(
            &s,
            "UserPromptSubmit",
            &hook_command(&e.reprise, "prompt")
        ));
        let tool_use = hook_command(&e.reprise, "tool-use");
        assert!(has_hook(&s, "PostToolUse", &tool_use));
        let group = groups(&s, "PostToolUse").next().unwrap();
        assert_eq!(
            group["matcher"], "Edit|Write|MultiEdit",
            "only after the agent writes a file"
        );
        assert!(has_permission(&s));
        assert_eq!(
            s.keys().collect::<Vec<_>>(),
            ["model", "permissions", "hooks"],
            "key order kept"
        );
        assert_eq!(
            std::fs::read_to_string(claude_md(&e.t)).unwrap(),
            "# Mine\n\nKeep this.\n",
            "CLAUDE.md is not written"
        );
        assert!(
            !PathBuf::from(format!("{}.reprise-backup", claude_md(&e.t).display())).exists(),
            "an untouched file is not backed up"
        );
        assert_eq!(std::fs::read_link(e.t.link()).unwrap(), e.reprise);
        assert!(e.t.manifest.exists());
        assert_eq!(
            std::fs::read_to_string(format!("{}.reprise-backup", e.t.settings().display()))
                .unwrap(),
            SETTINGS
        );

        let settings = std::fs::read_to_string(e.t.settings()).unwrap();
        let manifest = std::fs::read_to_string(&e.t.manifest).unwrap();
        connect_as(&e.t, &e.me()).unwrap();
        assert_eq!(
            std::fs::read_to_string(e.t.settings()).unwrap(),
            settings,
            "connecting the same reprise again changes nothing"
        );
        assert_eq!(std::fs::read_to_string(&e.t.manifest).unwrap(), manifest);

        disconnect(&e.t).unwrap();
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), SETTINGS);
        assert_eq!(
            std::fs::read_to_string(claude_md(&e.t)).unwrap(),
            "# Mine\n\nKeep this.\n"
        );
        assert!(e.t.link().symlink_metadata().is_err());
        assert!(!e.t.manifest.exists());
        disconnect(&e.t).unwrap();
    }

    #[test]
    fn refresh_adds_missing_hooks() {
        let e = env();
        std::fs::write(e.t.settings(), SETTINGS).unwrap();
        assert_eq!(
            refresh_with(&e.t, &e.reprise, Vec::new).unwrap(),
            Refresh::Unchanged,
            "nothing to refresh without a connection"
        );
        connect_as(&e.t, &e.me()).unwrap();
        let end = hook_command(&e.reprise, "session-end");
        assert!(has_hook(
            &read_json(&e.t.settings()).unwrap(),
            "SessionEnd",
            &end
        ));

        // A connection missing its SessionEnd hook, in settings.json and in the manifest.
        let mut s = read_json(&e.t.settings()).unwrap();
        remove_hook(&mut s, "SessionEnd", &end);
        drop_empty_hooks(&mut s);
        write_json(&e.t.settings(), &s, None).unwrap();
        let mut m: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&e.t.manifest).unwrap()).unwrap();
        m.hooks.retain(|(event, _)| event != "SessionEnd");
        std::fs::write(&e.t.manifest, serde_json::to_string(&m).unwrap()).unwrap();

        assert_eq!(
            refresh_with(&e.t, &e.reprise, Vec::new).unwrap(),
            Refresh::Updated
        );
        assert!(has_hook(
            &read_json(&e.t.settings()).unwrap(),
            "SessionEnd",
            &end
        ));
        assert_eq!(
            refresh_with(&e.t, &e.reprise, Vec::new).unwrap(),
            Refresh::Unchanged,
            "up to date: nothing more to add"
        );

        disconnect(&e.t).unwrap();
        assert_eq!(
            std::fs::read_to_string(e.t.settings()).unwrap(),
            SETTINGS,
            "disconnect removes the refreshed hook too"
        );
    }

    #[test]
    fn existing_permission_and_link_are_left_alone() {
        let e = env();
        // The user's own rules: one of reprise's, and a broad one.
        std::fs::write(
            e.t.settings(),
            r#"{"permissions":{"allow":["Bash(reprise items)", "Bash(reprise *)"]}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&e.t.bin_dir).unwrap();
        std::os::unix::fs::symlink("/somewhere/else", e.t.link()).unwrap();

        let plan = plan_for(&e.t, &e.me()).unwrap();
        let detail = |title: &str| {
            plan.iter()
                .find(|c| c.title == title)
                .unwrap()
                .detail
                .clone()
        };
        let allow = detail("Allow the reprise CLI");
        assert!(
            !allow.contains("Bash(reprise items),"),
            "already there: {allow}"
        );
        assert!(detail("Put reprise on PATH").contains("left as it is"));

        connect_as(&e.t, &e.me()).unwrap();
        disconnect(&e.t).unwrap();
        let s = read_json(&e.t.settings()).unwrap();
        let allow: Vec<&str> = s["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            allow,
            ["Bash(reprise items)", "Bash(reprise *)"],
            "rules we did not add stay"
        );
        assert!(!s.contains_key("hooks"));
        assert_eq!(
            std::fs::read_link(e.t.link()).unwrap(),
            PathBuf::from("/somewhere/else")
        );
        assert!(!claude_md(&e.t).exists());
    }

    /// Setup, connecting, the sweep, `item verify` and `rule accept` are left to ask the user.
    #[test]
    fn permissions_leave_what_changes_later_sessions_to_the_user() {
        for asks in [
            "Bash(reprise *)",
            "Bash(reprise setup *)",
            "Bash(reprise connect *)",
            "Bash(reprise sweep *)",
            "Bash(reprise item verify *)",
            "Bash(reprise rule accept *)",
        ] {
            assert!(!PERMISSIONS.contains(&asks), "{asks}");
        }
    }

    #[test]
    fn missing_files_are_created_and_removed_cleanly() {
        let e = env();
        connect_as(&e.t, &e.me()).unwrap();
        assert!(!claude_md(&e.t).exists(), "no CLAUDE.md is created");
        disconnect(&e.t).unwrap();
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), "{}\n");
        assert!(!claude_md(&e.t).exists());
    }

    #[test]
    fn invalid_settings_writes_nothing() {
        let e = env();
        std::fs::write(e.t.settings(), "{broken").unwrap();
        std::fs::write(claude_md(&e.t), "x\n").unwrap();
        assert!(matches!(connect_as(&e.t, &e.me()), Err(Error::Json { .. })));
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), "{broken");
        assert_eq!(std::fs::read_to_string(claude_md(&e.t)).unwrap(), "x\n");
        assert!(e.t.link().symlink_metadata().is_err());
        assert!(!e.t.manifest.exists());
    }

    #[test]
    fn quoting_paths_with_spaces() {
        assert_eq!(
            hook_command(Path::new("/opt/reprise/bin/reprise"), "prompt"),
            "/opt/reprise/bin/reprise hook prompt"
        );
        assert_eq!(
            hook_command(Path::new("/My Apps/reprise"), "prompt"),
            "'/My Apps/reprise' hook prompt"
        );
    }

    /// A package manager's install: the real file under a versioned folder, and the link on PATH to it.
    fn brew(e: &Env) -> (PathBuf, PathBuf) {
        let root = e.t.claude_dir.parent().unwrap();
        let real = root.join("Cellar/reprise/0.2.0/bin/reprise");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "#!/bin/sh\n").unwrap();
        let linked = root.join("brew/bin/reprise");
        std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        (real, linked)
    }

    fn reprise_hooks(e: &Env, event: &str) -> Vec<String> {
        commands(&read_json(&e.t.settings()).unwrap(), event)
            .into_iter()
            .filter(|c| parse_hook(c).is_some())
            .collect()
    }

    #[test]
    fn hooks_run_the_reprise_on_path_when_it_is_this_one() {
        let e = env();
        let (real, linked) = brew(&e);
        let on_path = Reprise {
            path: linked.clone(),
            on_path: true,
        };
        assert_eq!(locate(&e.t, &real, || vec![linked.clone()]), on_path);
        assert_eq!(
            locate(&e.t, &linked, || vec![linked.clone()]),
            on_path,
            "started through the link"
        );
        assert_eq!(
            locate(&e.t, &real, || vec![e.reprise.clone()]),
            Reprise {
                path: real.clone(),
                on_path: false
            },
            "PATH finds another reprise"
        );
        assert_eq!(locate(&e.t, &real, Vec::new).path, real);

        // reprise's own link is not a place to run the hooks from: disconnect removes it. An reprise later on
        // PATH is.
        std::fs::create_dir_all(&e.t.bin_dir).unwrap();
        std::os::unix::fs::symlink(&real, e.t.link()).unwrap();
        assert_eq!(locate(&e.t, &real, || vec![e.t.link()]).path, real);
        assert_eq!(
            locate(&e.t, &real, || vec![
                e.t.link(),
                e.reprise.clone(),
                linked.clone()
            ]),
            on_path
        );

        // A program installed where reprise's link was (`install reprise ~/.local/bin/reprise`) counts.
        std::fs::remove_file(e.t.link()).unwrap();
        std::fs::copy(&real, e.t.link()).unwrap();
        let copy = e.t.link();
        assert_eq!(
            locate(&e.t, &copy, || vec![copy.clone()]),
            Reprise {
                path: copy.clone(),
                on_path: true
            }
        );
    }

    #[test]
    fn connecting_another_reprise_moves_the_hooks_in_place() {
        let e = env();
        std::fs::write(e.t.settings(), SETTINGS).unwrap();
        connect_as(&e.t, &e.me()).unwrap();
        assert_eq!(std::fs::read_link(e.t.link()).unwrap(), e.reprise);

        let (_, linked) = brew(&e);
        let new = Reprise {
            path: linked.clone(),
            on_path: true,
        };
        let plan = plan_for(&e.t, &new).unwrap();
        let detail = |title: &str| {
            plan.iter()
                .find(|c| c.title == title)
                .unwrap()
                .detail
                .clone()
        };
        assert!(detail("Move the connection").contains(&linked.display().to_string()));
        assert!(detail("Add hooks").contains(&format!(
            "(replaces {})",
            hook_command(&e.reprise, "prompt")
        )));
        assert!(detail("Put reprise on PATH").contains("is removed"));

        connect_as(&e.t, &new).unwrap();
        for (event, arg, _) in EVENTS {
            assert_eq!(
                reprise_hooks(&e, event),
                [hook_command(&linked, arg)],
                "{event}: one hook, running the new reprise"
            );
        }
        let s = read_json(&e.t.settings()).unwrap();
        assert!(has_hook(&s, "SessionStart", "other-tool session-start"));
        let group = groups(&s, "PostToolUse").next().unwrap();
        assert_eq!(
            group["matcher"], "Edit|Write|MultiEdit",
            "the group is kept"
        );
        assert!(
            e.t.link().symlink_metadata().is_err(),
            "no link needed: reprise is on PATH"
        );
        let (m, _) = read_manifest(&e.t).unwrap().unwrap();
        assert_eq!(m.reprise, linked);
        assert_eq!(m.link, None);
        assert_eq!(
            m.permissions.len(),
            PERMISSIONS.len(),
            "still ours to remove"
        );

        disconnect(&e.t).unwrap();
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), SETTINGS);
    }

    #[test]
    fn moving_to_an_reprise_off_path_repoints_the_link() {
        let e = env();
        connect_as(&e.t, &e.me()).unwrap();
        let (real, _) = brew(&e);
        connect_as(
            &e.t,
            &Reprise {
                path: real.clone(),
                on_path: false,
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_link(e.t.link()).unwrap(), real);
        assert_eq!(
            reprise_hooks(&e, "SessionEnd"),
            [hook_command(&real, "session-end")]
        );

        disconnect(&e.t).unwrap();
        assert!(e.t.link().symlink_metadata().is_err());
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), "{}\n");
    }

    #[test]
    fn reprise_hooks_left_without_a_manifest_are_replaced_once() {
        let e = env();
        std::fs::write(
            e.t.settings(),
            r#"{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "/opt/reprise/bin/reprise hook prompt", "timeout": 10 }] },
      { "hooks": [
        { "type": "command", "command": "'/Other Place/reprise' hook prompt", "timeout": 10 },
        { "type": "command", "command": "/usr/bin/reprisex hook prompt" },
        { "type": "command", "command": "/x/reprise hook session-start" },
        { "type": "command", "command": "reprise hook prompt" }
      ] }
    ]
  }
}
"#,
        )
        .unwrap();
        connect_as(&e.t, &e.me()).unwrap();
        let s = read_json(&e.t.settings()).unwrap();
        assert_eq!(
            commands(&s, "UserPromptSubmit"),
            [
                hook_command(&e.reprise, "prompt"),
                "/usr/bin/reprisex hook prompt".to_owned(),
                "/x/reprise hook session-start".to_owned(),
                "reprise hook prompt".to_owned(),
            ],
            "the first stale hook becomes this reprise in place; the duplicate goes; other commands stay"
        );

        disconnect(&e.t).unwrap();
        assert_eq!(
            commands(&read_json(&e.t.settings()).unwrap(), "UserPromptSubmit"),
            [
                "/usr/bin/reprisex hook prompt",
                "/x/reprise hook session-start",
                "reprise hook prompt"
            ],
            "the stale hooks are not put back"
        );
    }

    #[test]
    fn refresh_moves_a_connection_whose_reprise_is_gone() {
        let e = env();
        std::fs::write(e.t.settings(), SETTINGS).unwrap();
        connect_as(&e.t, &e.me()).unwrap();
        std::fs::remove_file(&e.reprise).unwrap(); // the reprise the hooks run is deleted
        let (real, linked) = brew(&e);

        assert_eq!(
            refresh_with(&e.t, &real, Vec::new).unwrap(),
            Refresh::Broken {
                reprise: e.reprise.clone()
            },
            "a build PATH does not find does not take over the hooks"
        );
        assert_eq!(
            reprise_hooks(&e, "UserPromptSubmit"),
            [hook_command(&e.reprise, "prompt")],
            "broken: nothing changed"
        );

        assert_eq!(
            refresh_with(&e.t, &real, || vec![linked.clone()]).unwrap(),
            Refresh::Moved {
                from: e.reprise.clone(),
                to: linked.clone()
            }
        );
        for (event, arg, _) in EVENTS {
            assert_eq!(reprise_hooks(&e, event), [hook_command(&linked, arg)]);
        }
        assert!(
            e.t.link().symlink_metadata().is_err(),
            "the dangling link to the deleted reprise goes"
        );
        assert_eq!(
            refresh_with(&e.t, &real, || vec![linked.clone()]).unwrap(),
            Refresh::Unchanged
        );

        disconnect(&e.t).unwrap();
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), SETTINGS);
    }

    /// An reprise installed in place of reprise's link in `~/.local/bin`, after the reprise the hooks ran is
    /// deleted, takes the connection.
    #[test]
    fn an_reprise_installed_over_the_link_takes_the_connection() {
        let e = env();
        std::fs::write(e.t.settings(), SETTINGS).unwrap();
        connect_as(&e.t, &e.me()).unwrap();
        assert_eq!(std::fs::read_link(e.t.link()).unwrap(), e.reprise);
        std::fs::remove_file(e.t.link()).unwrap();
        std::fs::write(e.t.link(), "#!/bin/sh\n").unwrap();
        std::fs::remove_file(&e.reprise).unwrap();

        let new = e.t.link();
        assert_eq!(
            refresh_with(&e.t, &new, || vec![new.clone()]).unwrap(),
            Refresh::Moved {
                from: e.reprise.clone(),
                to: new.clone()
            }
        );
        assert_eq!(
            reprise_hooks(&e, "SessionStart"),
            [hook_command(&new, "session-start")]
        );
        let (m, _) = read_manifest(&e.t).unwrap().unwrap();
        assert_eq!(
            m.link, None,
            "the file there is not reprise's link to remove"
        );

        disconnect(&e.t).unwrap();
        assert!(
            new.exists(),
            "disconnect leaves the installed reprise alone"
        );
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), SETTINGS);
    }

    #[test]
    fn a_failed_move_puts_everything_back() {
        use std::os::unix::fs::PermissionsExt;

        let e = env();
        std::fs::write(e.t.settings(), SETTINGS).unwrap();
        connect_as(&e.t, &e.me()).unwrap();
        let settings = std::fs::read_to_string(e.t.settings()).unwrap();
        let manifest = std::fs::read_to_string(&e.t.manifest).unwrap();

        // The manifest's folder takes no new files, so the last step fails.
        let dir = e.t.manifest.parent().unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let (real, _) = brew(&e);
        let result = connect_as(
            &e.t,
            &Reprise {
                path: real,
                on_path: false,
            },
        );
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(e.t.settings()).unwrap(), settings);
        assert_eq!(
            std::fs::read_link(e.t.link()).unwrap(),
            e.reprise,
            "link points back"
        );
        assert_eq!(std::fs::read_to_string(&e.t.manifest).unwrap(), manifest);
    }

    #[test]
    fn parsing_hook_commands() {
        let installed = Path::new("/opt/reprise/bin/reprise");
        assert_eq!(
            parse_hook(&hook_command(installed, "prompt")),
            Some((installed.to_path_buf(), "prompt"))
        );
        let spaced = Path::new("/My Apps/it's/reprise");
        assert_eq!(
            parse_hook(&hook_command(spaced, "tool-use")),
            Some((spaced.to_path_buf(), "tool-use"))
        );
        for other in [
            "reprise hook prompt",
            "/usr/bin/reprisex hook prompt",
            "/x/reprise hook prompt --verbose",
            "/x/reprise prompt",
            "other-tool session-start",
        ] {
            assert_eq!(parse_hook(other), None, "{other}");
        }
    }
}
