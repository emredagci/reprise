use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Finds a program the user has installed. A program started by launchd or systemd gets a short `PATH` (`/usr/bin:/bin:…`) without the folders the user's shell adds, such as
/// `~/.local/bin`, where Claude Code installs itself. So after this process's `PATH` come the usual
/// install folders and, last, the `PATH` of the user's login shell. In a terminal or a hook the first
/// step finds it, and the shell is never started.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let env = std::env::var_os("PATH").unwrap_or_default();
    find_executable(name, &env)
        .or_else(|| {
            let extra = std::env::join_paths(install_dirs()).ok()?;
            find_executable(name, &extra)
        })
        .or_else(|| find_executable(name, login_shell_path()?))
}

/// Every `name` the user could run, in the order `find_program` looks. The login shell is started only
/// once the folders before it are used up.
pub fn find_all(name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    let env = std::env::var_os("PATH").unwrap_or_default();
    let near: Vec<PathBuf> = std::env::split_paths(&env).chain(install_dirs()).collect();
    let shell = std::iter::once_with(|| {
        login_shell_path().map_or_else(Vec::new, |p| std::env::split_paths(p).collect())
    })
    .flatten();
    let mut seen = std::collections::HashSet::new();
    near.into_iter()
        .chain(shell)
        .filter(move |d| !d.as_os_str().is_empty() && seen.insert(d.clone()))
        .flat_map(move |d| candidates(&d, name))
        .filter(|p| is_executable(p))
}

/// This process's `PATH` with the install folders and the login shell's `PATH` added, for the programs
/// reprise starts: what they run in turn is found as it would be in the user's terminal.
pub fn full_path() -> OsString {
    let env = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&env).collect();
    if let Some(shell) = login_shell_path() {
        dirs.extend(std::env::split_paths(shell));
    }
    dirs.extend(install_dirs());
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
    std::env::join_paths(dirs).unwrap_or(env)
}

/// Whether `dir` is on the `PATH` of the user's terminal: this process's, or the login shell's when it
/// is not on this one (a process started outside a shell).
pub fn on_user_path(dir: &Path) -> bool {
    let has = |p: &OsStr| std::env::split_paths(p).any(|d| d == dir);
    std::env::var_os("PATH").is_some_and(|p| has(&p)) || login_shell_path().is_some_and(has)
}

/// Where installers put command-line tools when they do not use the system's own folders.
fn install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// The `PATH` a login shell of the user's sets up, read once per process. `None` on Windows, when the
/// shell fails, or when it takes longer than a few seconds.
fn login_shell_path() -> Option<&'static OsStr> {
    static PATH: OnceLock<Option<OsString>> = OnceLock::new();
    PATH.get_or_init(read_login_shell_path).as_deref()
}

#[cfg(unix)]
fn read_login_shell_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/sh"
            }
            .into()
        });
    shell_path(&shell, Duration::from_secs(5))
}

/// Starts `shell` as a login and interactive shell and returns the `PATH` it prints.
#[cfg(unix)]
fn shell_path(shell: &OsStr, timeout: Duration) -> Option<OsString> {
    use std::io::Read;
    use std::os::unix::ffi::OsStringExt;

    const MARK: &str = "__REPRISE_PATH__";
    // Interactive as well as login, so what `.zshrc` or `.bashrc` adds is there too. The markers keep
    // whatever the shell's startup files print out of the answer.
    let mut child = Command::new(shell)
        .args([
            "-l",
            "-i",
            "-c",
            &format!("printf '{MARK}%s{MARK}' \"$PATH\""),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    let text = String::from_utf8_lossy(&out);
    let path = text.split(MARK).nth(1).filter(|p| !p.is_empty())?;
    Some(OsString::from_vec(path.as_bytes().to_vec()))
}

#[cfg(not(unix))]
fn read_login_shell_path() -> Option<OsString> {
    None
}

/// Finds an executable in a `PATH`-like list. On Windows it also tries the `PATHEXT` extensions.
pub fn find_executable(name: &str, search_path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .flat_map(|dir| candidates(&dir, name))
        .find(|p| is_executable(p))
}

fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = vec![dir.join(name)];
    if cfg!(windows) {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
        out.extend(
            exts.split(';')
                .filter(|e| !e.is_empty())
                .map(|e| dir.join(format!("{name}{e}"))),
        );
    }
    out
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn finds_first_executable_and_skips_plain_files() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("tool"), "").unwrap(); // not executable
        let exe = b.path().join("tool");
        std::fs::write(&exe, "").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        assert_eq!(find_executable("tool", &path), Some(exe));
        assert_eq!(find_executable("missing", &path), None);
    }

    fn script(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("fake-shell");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn shell_path_ignores_what_startup_files_print() {
        let dir = tempfile::tempdir().unwrap();
        // A shell whose startup prints a banner before running the command it was given ("$4" after -l -i -c).
        let shell = script(
            dir.path(),
            "echo 'Welcome back'; PATH=/custom/bin:/usr/bin; eval \"$4\"; echo bye",
        );
        assert_eq!(
            shell_path(shell.as_os_str(), Duration::from_secs(5)),
            Some(OsString::from("/custom/bin:/usr/bin"))
        );
    }

    #[test]
    fn shell_path_gives_up_on_a_shell_that_hangs_or_fails() {
        let dir = tempfile::tempdir().unwrap();
        let hangs = script(dir.path(), "sleep 10");
        let started = Instant::now();
        assert_eq!(
            shell_path(hangs.as_os_str(), Duration::from_millis(300)),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            shell_path(OsStr::new("/no/such/shell"), Duration::from_secs(1)),
            None
        );
    }
}
