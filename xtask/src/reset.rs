//! `cargo xtask reset`: puts this machine back to before reprise was first run, to try the first run again.
//!
//! Lists what it will do and asks first. Files go to the Trash unless `--delete` is given. A Claude Code
//! connection is undone with reprise's own `disconnect`, and the scheduled sweep with `reprise sweep
//! unschedule`, before reprise's settings folder goes, because the records of what they installed live
//! there; the copies connecting made of Claude Code's files (`*.reprise-backup`) go too. The vault is left
//! alone unless asked for, and only a reprise vault is ever removed.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use reprise::settings::Settings;
use reprise::sweep::Schedule;
use reprise::vault::Vault;

pub const USAGE: &str =
    "  reset   removes reprise's settings and caches, undoes connections and the scheduled sweep, to try
          the first run again. Lists what it will do and asks first; files go to the Trash.
            --vault        also the vault in the settings (only a reprise vault)
            --delete       delete instead of moving to the Trash
            --yes          do not ask";

#[derive(Default)]
struct Options {
    vault: bool,
    delete: bool,
    yes: bool,
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let mut opts = Options::default();
    for arg in args {
        match arg.as_str() {
            "--vault" => opts.vault = true,
            "--delete" => opts.delete = true,
            "--yes" => opts.yes = true,
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    let settings_path = Settings::default_path()?;
    let settings = Settings::load_from(&settings_path).unwrap_or_default();
    let connected: Vec<_> = reprise::clients::all()
        .into_iter()
        .filter(|c| {
            c.connector()
                .is_some_and(|k| k.is_connected().unwrap_or(false))
        })
        .collect();

    let mut paths: Vec<PathBuf> = settings_path
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    let scheduled = Schedule::load().ok().flatten();
    paths.extend(connect_backups());
    let mut notes = Vec::new();
    if opts.vault {
        match settings.vault.as_deref() {
            Some(vault) if Vault::open(vault).is_ok() => paths.push(vault.to_path_buf()),
            Some(vault) if vault.exists() => {
                notes.push(format!(
                    "{} is not a reprise vault; it is left alone",
                    shown(vault)
                ));
            }
            _ => notes.push("no vault in the settings".to_owned()),
        }
    }
    paths.retain(|p| p.symlink_metadata().is_ok());
    paths.dedup();

    if connected.is_empty() && scheduled.is_none() && paths.is_empty() {
        println!("Nothing of reprise's is left on this machine.");
        notes.iter().for_each(|n| println!("  {n}"));
        return Ok(());
    }
    println!("This will:");
    for c in &connected {
        println!(
            "  disconnect {} (what connecting added, nothing else)",
            c.display_name()
        );
    }
    if let Some(s) = &scheduled {
        println!("  unschedule the sweep ({} job {})", s.service, s.name);
    }
    let verb = if opts.delete {
        "delete"
    } else {
        "move to the Trash"
    };
    for p in &paths {
        println!("  {verb} {}", shown(p));
    }
    notes.iter().for_each(|n| println!("  ({n})"));
    if !opts.yes && !confirm()? {
        println!("Nothing changed.");
        return Ok(());
    }

    for c in &connected {
        if let Some(k) = c.connector() {
            k.disconnect()
                .with_context(|| format!("disconnecting {}", c.display_name()))?;
        }
    }
    if scheduled.is_some() {
        unschedule(root)?;
    }
    let trash = if opts.delete {
        None
    } else {
        Some(trash_dir()?)
    };
    for p in &paths {
        match &trash {
            Some(dir) => to_trash(p, dir)?,
            None if p.is_dir() && !p.is_symlink() => {
                std::fs::remove_dir_all(p).with_context(|| format!("deleting {}", p.display()))?
            }
            None => std::fs::remove_file(p).with_context(|| format!("deleting {}", p.display()))?,
        }
    }
    match trash {
        Some(dir) => println!("Done. What was removed is in {}.", shown(&dir)),
        None => println!("Done."),
    }
    Ok(())
}

/// `reprise sweep unschedule`, built from this checkout: it removes exactly the job it installed.
fn unschedule(root: &Path) -> Result<()> {
    let status = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "--package",
            "reprise",
            "--",
            "sweep",
            "unschedule",
        ])
        .current_dir(root)
        .status()
        .context("cargo did not run")?;
    if !status.success() {
        bail!("`reprise sweep unschedule` failed");
    }
    Ok(())
}

fn confirm() -> Result<bool> {
    print!("Go ahead? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// A new folder in the Trash for this run, so what one reset removed stays together.
fn trash_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home folder")?;
    let base = if cfg!(target_os = "macos") {
        home.join(".Trash")
    } else {
        dirs::data_dir()
            .context("no data folder")?
            .join("Trash/files")
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let dir = base.join(format!("reprise-reset-{stamp}"));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// `path` with the home folder written as `~`.
fn shown(path: &Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The copies connecting made of Claude Code's files before changing them (`*.reprise-backup`). A later
/// connect keeps an existing copy, so a stale one would outlive the reset.
fn connect_backups() -> Vec<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    dir.and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".reprise-backup"))
        .collect()
}

/// Moves `path` into `dir`, named after where it was (`Library__Application Support__reprise`).
fn to_trash(path: &Path, dir: &Path) -> Result<()> {
    let home = dirs::home_dir().unwrap_or_default();
    let shown = path.strip_prefix(&home).unwrap_or(path);
    let name = shown
        .to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "__");
    std::fs::rename(path, dir.join(name)).with_context(|| {
        format!(
            "moving {} to the Trash (on another disk? use --delete)",
            path.display()
        )
    })
}
