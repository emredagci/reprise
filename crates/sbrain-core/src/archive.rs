//! Moving a vault as one file: `export` writes a `.tar.gz` of the vault (records, profile, notes,
//! settings and the sweep's kept sessions, so their records can be replayed), and `restore` unpacks
//! one into an empty folder. The git history is left out unless asked for: a vault with a remote moves
//! by cloning. Restoring refuses an archive with a path that leaves the folder or a link in it.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::{Error, Result, Vault};

fn tar_error(what: &str, out: &std::process::Output) -> Error {
    Error::InvalidChange(format!(
        "tar could not {what}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The files of the vault an archive holds, relative to its root: everything but `.git` (unless
/// `history`) and the machine's cache, of which only the kept sessions go along.
pub fn files(vault: &Vault, history: bool) -> Vec<PathBuf> {
    let root = vault.root();
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            let skip = (rel.starts_with(".git") && !history)
                || (rel.starts_with(".sbrain/cache")
                    && !rel.starts_with(".sbrain/cache/sessions")
                    && rel != Path::new(".sbrain/cache"));
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if skip || kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

/// Writes the vault to `archive` (`.tar.gz`); returns how many files went in.
pub fn export(vault: &Vault, archive: &Path, history: bool) -> Result<usize> {
    let files = files(vault, history);
    let list = std::env::temp_dir().join(format!("sbrain-export-{}.txt", std::process::id()));
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    std::fs::write(&list, names.join("\n") + "\n").map_err(|source| Error::Io {
        path: list.clone(),
        source,
    })?;
    let out = Command::new("tar")
        .arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(vault.root())
        .arg("-T")
        .arg(&list)
        .output();
    let _ = std::fs::remove_file(&list);
    let out = out.map_err(|source| Error::Io {
        path: PathBuf::from("tar"),
        source,
    })?;
    if !out.status.success() {
        return Err(tar_error("write the archive", &out));
    }
    Ok(files.len())
}

/// Whether an archive member's name stays inside the folder it is unpacked into.
fn safe_name(name: &str) -> bool {
    let p = Path::new(name);
    !name.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Unpacks `archive` into `folder`, which must be empty or missing, and checks that it is a vault.
/// Returns the vault.
pub fn restore(archive: &Path, folder: &Path) -> Result<Vault> {
    if folder.exists()
        && std::fs::read_dir(folder)
            .map_err(|source| Error::Io {
                path: folder.to_path_buf(),
                source,
            })?
            .next()
            .is_some()
    {
        return Err(Error::InvalidChange(format!(
            "{} is not empty; restore into an empty folder",
            folder.display()
        )));
    }
    let list = Command::new("tar")
        .arg("-tvzf")
        .arg(archive)
        .output()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tar"),
            source,
        })?;
    if !list.status.success() {
        return Err(tar_error("read the archive", &list));
    }
    let names = Command::new("tar")
        .arg("-tzf")
        .arg(archive)
        .output()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tar"),
            source,
        })?;
    let names = String::from_utf8_lossy(&names.stdout).into_owned();
    if let Some(bad) = names.lines().find(|n| !safe_name(n)) {
        return Err(Error::InvalidChange(format!(
            "the archive has {bad:?}, which would land outside the folder; nothing was unpacked"
        )));
    }
    // A link (`l`) or a device in the long listing: the vault never holds one.
    if String::from_utf8_lossy(&list.stdout)
        .lines()
        .any(|l| !l.starts_with('-') && !l.starts_with('d'))
    {
        return Err(Error::InvalidChange(
            "the archive holds a link or a special file; nothing was unpacked".into(),
        ));
    }
    crate::safe::create_dir_all(folder)?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(folder)
        .output()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tar"),
            source,
        })?;
    if !out.status.success() {
        return Err(tar_error("unpack the archive", &out));
    }
    Vault::open(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vault_moves_as_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault_without_git(&tmp.path().join("v"));
        let write = |rel: &str, text: &str| {
            let p = vault.root().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("projects/app/state.md", "# app\n");
        write(".sbrain/cache/sessions/s-1.json", "{}");
        write(".sbrain/cache/doctor.json", "{}");
        write(".git/HEAD", "ref");
        let names: Vec<String> = files(&vault, false)
            .iter()
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"projects/app/state.md".to_owned()));
        assert!(names.contains(&".sbrain/vault.toml".to_owned()));
        assert!(names.contains(&".sbrain/cache/sessions/s-1.json".to_owned()));
        assert!(
            !names
                .iter()
                .any(|n| n.contains("doctor.json") || n.starts_with(".git/"))
        );

        let archive = tmp.path().join("v.tar.gz");
        assert_eq!(export(&vault, &archive, false).unwrap(), names.len());
        let back = restore(&archive, &tmp.path().join("w")).unwrap();
        assert_eq!(
            std::fs::read_to_string(back.root().join("projects/app/state.md")).unwrap(),
            "# app\n"
        );
        assert!(
            restore(&archive, &tmp.path().join("w")).is_err(),
            "not empty"
        );
    }

    #[test]
    fn unsafe_names_are_refused() {
        assert!(safe_name("projects/app/state.md"));
        assert!(safe_name("./a"));
        assert!(!safe_name("../x"));
        assert!(!safe_name("/etc/passwd"));
        assert!(!safe_name("a/../../x"));
        assert!(!safe_name(""));
    }
}
