//! Reading and writing inside the vault without following symlinks. A vault can be synced from a git
//! remote, and a remote can place a symlink anywhere in it: `projects/app/state.md` pointing at a key
//! file would put that file in an agent's context; `gotchas.md` or a temporary file pointing outside
//! the vault would let a write overwrite it. So below the vault's root (the folder holding `.sbrain/`)
//! no part of a path may be a symlink, temporary files are created only if nothing is there yet, and
//! appends refuse a symlink. Paths outside any vault (sbrain's settings, which only the user writes) are
//! not checked.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The vault root a path lies in: the nearest folder above it holding `.sbrain`.
fn vault_root(path: &Path) -> Option<&Path> {
    path.ancestors()
        .skip(1)
        .find(|a| a.join(".sbrain").symlink_metadata().is_ok())
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

fn refused(path: &Path) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other("refused: a symlink inside the vault"),
    }
}

/// Fails when `path`, or any folder between the vault root and it, is a symlink.
pub fn check(path: &Path) -> Result<()> {
    let Some(root) = vault_root(path) else {
        return Ok(());
    };
    let rest = path.strip_prefix(root).unwrap_or(path);
    let mut at = root.to_path_buf();
    for part in rest.components() {
        at.push(part);
        if is_symlink(&at) {
            return Err(refused(&at));
        }
    }
    Ok(())
}

/// `std::fs::read_to_string`, refusing a symlink on the way (as an `io::Error`), for code that reads
/// vault files and handles a missing one itself.
pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    let path = path.as_ref();
    check(path).map_err(|_| std::io::Error::other("refused: a symlink inside the vault"))?;
    std::fs::read_to_string(path)
}

/// The file's text, unless a symlink is on the way to it.
pub fn read_text(path: &Path) -> Result<String> {
    check(path)?;
    std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Creates the folder and the ones above it, refusing a symlink on the way.
pub fn create_dir_all(path: &Path) -> Result<()> {
    check(path)?;
    std::fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    // A symlink put in place while the folders were made.
    check(path)
}

/// Writes through a new temporary file beside `path` and a rename, so a half-written file never
/// remains. The temporary file is created only if nothing has that name, so a symlink planted there
/// is never written through; `path` itself, and every folder on the way, must not be a symlink.
pub fn write(path: &Path, text: &str) -> Result<()> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    check(path)?;
    if let Some(dir) = path.parent() {
        create_dir_all(dir)?;
    }
    let tmp = temporary(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(io)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    Ok(())
}

/// `<name>.sbrain-tmp-<pid>-<n>` beside `path`: a name no other writer uses at the same time.
fn temporary(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(
        ".{name}.sbrain-tmp-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Appends `line` (one line: control characters become spaces) to the file, creating it; refuses a
/// symlink at `path` or on the way.
pub fn append_line(path: &Path, line: &str) -> Result<()> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        create_dir_all(dir)?;
    }
    check(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Fails on a symlink put at `path` after the check.
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(io)?;
    let clean: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    writeln!(file, "{clean}").map_err(io)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// A vault root with `.sbrain/`, and a folder outside it.
    fn env() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let vault = root.join("vault");
        let outside = root.join("outside");
        std::fs::create_dir_all(vault.join(".sbrain/cache")).unwrap();
        std::fs::create_dir_all(vault.join("projects/app")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "key").unwrap();
        (tmp, vault, outside)
    }

    #[test]
    fn symlinks_in_the_vault_are_neither_read_nor_written() {
        let (_t, vault, outside) = env();
        let state = vault.join("projects/app/state.md");
        symlink(outside.join("secret"), &state).unwrap();
        assert!(
            read_text(&state).is_err(),
            "a key file is not read into context"
        );
        assert!(write(&state, "x").is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("secret")).unwrap(),
            "key"
        );

        // A symlinked folder on the way counts too.
        symlink(&outside, vault.join("projects/linked")).unwrap();
        assert!(read_text(&vault.join("projects/linked/secret")).is_err());
        assert!(write(&vault.join("projects/linked/new.md"), "x").is_err());
        assert!(!outside.join("new.md").exists());

        let plain = vault.join("projects/app/gotchas.md");
        write(&plain, "ok\n").unwrap();
        assert_eq!(read_text(&plain).unwrap(), "ok\n");
    }

    #[test]
    fn a_planted_temporary_file_is_not_written_through() {
        let (_t, vault, outside) = env();
        let target = vault.join("projects/app/state.md");
        // A symlink planted beside the file under a temporary-looking name is never written through.
        symlink(outside.join("secret"), target.with_extension("sbrain-tmp")).unwrap();
        write(&target, "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
        assert_eq!(
            std::fs::read_to_string(outside.join("secret")).unwrap(),
            "key"
        );
        let left: Vec<_> = std::fs::read_dir(vault.join("projects/app"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("sbrain-tmp-"))
            .collect();
        assert!(left.is_empty(), "no temporary file is left: {left:?}");
    }

    #[test]
    fn appends_refuse_a_symlink_and_keep_one_line() {
        let (_t, vault, outside) = env();
        let log = vault.join(".sbrain/cache/hook-errors.log");
        append_line(&log, "first\nforged line").unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "first forged line\n"
        );

        let rc = outside.join("rc");
        std::fs::write(&rc, "").unwrap();
        std::fs::remove_file(&log).unwrap();
        symlink(&rc, &log).unwrap();
        assert!(append_line(&log, "echo pwned").is_err());
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "");
    }

    #[test]
    fn outside_a_vault_nothing_is_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        symlink(&real, root.join("via")).unwrap();
        // A settings folder reached through a symlinked parent, or a settings file kept elsewhere.
        write(&root.join("via/settings.json"), "{}").unwrap();
        assert_eq!(
            std::fs::read_to_string(real.join("settings.json")).unwrap(),
            "{}"
        );
        symlink(real.join("settings.json"), root.join("linked.json")).unwrap();
        assert_eq!(read_text(&root.join("linked.json")).unwrap(), "{}");
    }
}
