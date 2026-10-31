//! `reprise uninstall`: removes what reprise set up on this computer, except the vault and the program
//! itself. Connections are undone with each client's own `disconnect` and the scheduled sweep with its
//! `unschedule`, before the settings folder goes, because the records of what they installed live
//! there. Then reprise's files in that folder go, and the copies connecting made of Claude Code's files
//! (`*.reprise-backup`). Without `--yes` it only lists what it would do. Last, it says how to remove the
//! program: the way it was installed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use reprise::Settings;
use reprise::sweep::Schedule;

/// What reprise keeps beside its settings file.
const SETTINGS_ENTRIES: [&str; 4] = ["connections", "schedule.json", "distill", "sweep.log"];

pub fn run(yes: bool) -> Result<()> {
    let settings_path = Settings::default_path()?;
    let settings = Settings::load_from(&settings_path).unwrap_or_default();
    let vault = settings.vault.clone();
    let connected: Vec<_> = reprise::clients::all()
        .into_iter()
        .filter(|c| {
            c.connector()
                .is_some_and(|k| k.is_connected().unwrap_or(false))
        })
        .collect();
    let scheduled = Schedule::load().ok().flatten();
    let claude_dir = reprise::clients::claude::install::Targets::from_env()
        .ok()
        .map(|t| t.claude_dir);
    let files = files_to_remove(&settings_path, claude_dir.as_deref(), vault.as_deref());

    let exe = std::env::current_exe()?;
    let remove_program = how_to_remove(&exe);
    if connected.is_empty() && scheduled.is_none() && files.is_empty() {
        println!("Nothing of reprise's is set up on this computer.");
        println!("To remove the program: {remove_program}");
        return Ok(());
    }

    println!(
        "{} reprise from this computer:",
        if yes { "Removing" } else { "This removes" }
    );
    for c in &connected {
        println!(
            "  disconnect {} (what connecting added, nothing else)",
            c.display_name()
        );
    }
    if let Some(s) = &scheduled {
        println!("  unschedule the sweep ({} job {})", s.service, s.name);
    }
    for f in &files {
        println!("  delete {}", shown(f));
    }
    match &vault {
        Some(v) => println!("The vault stays: {}", shown(v)),
        None => println!("No vault is set; nothing outside reprise's own files is touched."),
    }
    if !yes {
        println!("\nRun `reprise uninstall --yes` to do it.");
        return Ok(());
    }

    for c in &connected {
        if let Some(k) = c.connector() {
            k.disconnect()
                .with_context(|| format!("disconnecting {}", c.display_name()))?;
        }
    }
    if let Some(installed) = &scheduled {
        crate::schedule::Host::current()?.uninstall(installed)?;
        Schedule::forget()?;
    }
    for f in files_to_remove(&settings_path, claude_dir.as_deref(), vault.as_deref()) {
        remove(&f)?;
    }
    if let Some(dir) = settings_path.parent() {
        // Only when nothing else is left in it.
        let _ = std::fs::remove_dir(dir);
    }
    println!("\nDone. To remove the program: {remove_program}");
    Ok(())
}

/// reprise's files beside its settings file and the copies connecting made in `claude_dir`, that exist;
/// never the vault or anything inside it.
fn files_to_remove(
    settings_path: &Path,
    claude_dir: Option<&Path>,
    vault: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = vec![settings_path.to_path_buf()];
    if let Some(dir) = settings_path.parent() {
        out.extend(SETTINGS_ENTRIES.iter().map(|e| dir.join(e)));
    }
    if let Some(dir) = claude_dir {
        out.extend(connect_backups(dir));
    }
    let vault = vault.map(|v| v.canonicalize().unwrap_or_else(|_| v.to_path_buf()));
    out.retain(|p| {
        let real = p.canonicalize().unwrap_or_else(|_| p.clone());
        p.symlink_metadata().is_ok()
            && vault
                .as_deref()
                .is_none_or(|v| !real.starts_with(v) && !v.starts_with(&real))
    });
    out
}

/// The copies connecting made of Claude Code's files before changing them (`*.reprise-backup`).
fn connect_backups(claude_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(claude_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".reprise-backup"))
        .collect()
}

fn remove(path: &Path) -> Result<()> {
    let is_dir = path.symlink_metadata().is_ok_and(|m| m.is_dir());
    if is_dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("deleting {}", path.display()))
}

/// The command that removes the program, from where it runs: Homebrew's Cellar, cargo's bin folder, or
/// any other place, where the file itself goes.
fn how_to_remove(exe: &Path) -> String {
    let real = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let text = real.to_string_lossy();
    if text.contains("/Cellar/reprise/") {
        "`brew uninstall reprise`".to_owned()
    } else if text.contains("/.cargo/bin/") {
        "`cargo uninstall reprise`".to_owned()
    } else {
        format!("delete {}", shown(&real))
    }
}

/// `path` with the home folder written as `~`.
fn shown(path: &Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_removed_the_way_it_was_installed() {
        assert_eq!(
            how_to_remove(Path::new("/opt/homebrew/Cellar/reprise/0.2.0/bin/reprise")),
            "`brew uninstall reprise`"
        );
        assert_eq!(
            how_to_remove(Path::new("/home/me/.cargo/bin/reprise")),
            "`cargo uninstall reprise`"
        );
        assert_eq!(
            how_to_remove(Path::new("/usr/local/bin/reprise")),
            "delete /usr/local/bin/reprise"
        );
    }

    #[test]
    fn the_vault_is_never_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap().join("reprise");
        std::fs::create_dir_all(dir.join("connections")).unwrap();
        std::fs::write(dir.join("settings.json"), "{}").unwrap();
        std::fs::write(dir.join("sweep.log"), "").unwrap();
        let settings = dir.join("settings.json");
        let claude = tmp.path().canonicalize().unwrap().join("claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("settings.json.reprise-backup"), "{}").unwrap();
        std::fs::write(claude.join("settings.json"), "{}").unwrap();

        let found = files_to_remove(&settings, Some(&claude), None);
        assert!(found.contains(&settings) && found.contains(&dir.join("connections")));
        assert!(found.contains(&claude.join("settings.json.reprise-backup")));
        assert!(
            !found.contains(&claude.join("settings.json")),
            "Claude Code's own file stays"
        );
        assert!(!found.contains(&dir.join("distill")), "only what exists");

        // A vault kept inside the settings folder, or holding it, is left alone.
        let vault = dir.join("connections");
        assert!(!files_to_remove(&settings, None, Some(&vault)).contains(&vault));
        assert!(files_to_remove(&settings, None, Some(&dir)).is_empty());
    }
}
