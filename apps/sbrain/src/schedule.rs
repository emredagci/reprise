//! `sbrain sweep schedule`: has the OS run `sbrain sweep --due` every few minutes, launchd on macOS and a systemd
//! user timer on Linux. Whether a run is due is decided by `--due` from the settings and the vault's
//! sweep record, so changing how often needs no reinstall, and a run missed while the computer slept
//! happens once it wakes. What was installed is recorded (`sbrain_core::sweep::Schedule`) so `unschedule`
//! removes exactly that.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sbrain_core::sweep::Schedule;

/// How often the OS asks whether a run is due.
pub const TICK_MINUTES: u32 = 15;
/// launchd's label for the job.
const LABEL: &str = "com.emredagci.sbrain.sweep";
/// The systemd units, `<UNIT>.service` and `<UNIT>.timer`.
const UNIT: &str = "sbrain-sweep";
/// Variables that point sbrain somewhere other than its defaults; the job keeps the ones set here.
const PASSED_ENV: [&str; 3] = ["SETTINGS", "VAULT", "CLAUDE_CONFIG_DIR"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Launchd,
    Systemd,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
        }
    }
}

/// What the job runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub sbrain: PathBuf,
    pub env: Vec<(String, String)>,
}

impl Job {
    pub fn new(sbrain: PathBuf) -> Self {
        let env = PASSED_ENV
            .iter()
            .filter_map(|k| {
                let (name, value) = if k.starts_with("CLAUDE_") {
                    ((*k).to_owned(), std::env::var_os(k)?)
                } else {
                    (format!("SBRAIN_{k}"), sbrain_core::env_var(k)?)
                };
                Some((name, value.into_string().ok()?))
            })
            .collect();
        Self { sbrain, env }
    }
}

/// Runs the service manager (`launchctl` or `systemctl`) with these arguments.
pub type Control = Box<dyn Fn(&[&str]) -> Result<()>>;

/// Where the job's files go and how the service manager is told. Tests point these elsewhere.
pub struct Host {
    pub kind: Kind,
    /// `~/Library/LaunchAgents`, or `~/.config/systemd/user`.
    pub dir: PathBuf,
    /// Folder for launchd's log, beside sbrain's settings.
    pub log_dir: PathBuf,
    pub control: Control,
}

impl Host {
    /// This computer's service manager.
    pub fn current() -> Result<Self> {
        let home = dirs::home_dir().context("no home folder")?;
        let log_dir = sbrain_core::Settings::default_path()?
            .parent()
            .map_or_else(|| home.clone(), Path::to_path_buf);
        let kind = if cfg!(target_os = "macos") {
            Kind::Launchd
        } else if cfg!(target_os = "linux") {
            Kind::Systemd
        } else {
            bail!(
                "scheduling the sweep is supported on macOS and Linux; run `sbrain sweep` by hand"
            )
        };
        let (dir, program) = match kind {
            Kind::Launchd => (home.join("Library/LaunchAgents"), "launchctl"),
            Kind::Systemd => (
                std::env::var_os("XDG_CONFIG_HOME")
                    .map_or_else(|| home.join(".config"), PathBuf::from)
                    .join("systemd/user"),
                "systemctl",
            ),
        };
        Ok(Self {
            kind,
            dir,
            log_dir,
            control: Box::new(move |args| run(program, args)),
        })
    }

    /// Installs the job, replacing one installed before, and returns what to record.
    pub fn install(&self, job: &Job) -> Result<Schedule> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        match self.kind {
            Kind::Launchd => {
                let plist = self.dir.join(format!("{LABEL}.plist"));
                let log = self.log_dir.join("sweep.log");
                std::fs::create_dir_all(&self.log_dir)?;
                write(&plist, &launchd_plist(job, &log))?;
                let loaded = gui_domain(&plist).and_then(|domain| {
                    // A job loaded before is replaced; there being none is fine.
                    let _ = (self.control)(&["bootout", &format!("{domain}/{LABEL}")]);
                    (self.control)(&["bootstrap", &domain, &plist.to_string_lossy()])
                });
                if let Err(e) = loaded {
                    let _ = std::fs::remove_file(&plist);
                    return Err(e);
                }
                Ok(Schedule {
                    service: self.kind.name().into(),
                    name: LABEL.into(),
                    sbrain: job.sbrain.clone(),
                    env: job.env.clone(),
                    files: vec![plist],
                    log: Some(log),
                })
            }
            Kind::Systemd => {
                let service = self.dir.join(format!("{UNIT}.service"));
                let timer = self.dir.join(format!("{UNIT}.timer"));
                let enabled = write(&service, &systemd_service(job))
                    .and_then(|()| write(&timer, &systemd_timer()))
                    .and_then(|()| (self.control)(&["--user", "daemon-reload"]))
                    .and_then(|()| {
                        (self.control)(&["--user", "enable", "--now", &format!("{UNIT}.timer")])
                    });
                if let Err(e) = enabled {
                    let _ = std::fs::remove_file(&service);
                    let _ = std::fs::remove_file(&timer);
                    let _ = (self.control)(&["--user", "daemon-reload"]);
                    return Err(e);
                }
                Ok(Schedule {
                    service: self.kind.name().into(),
                    name: format!("{UNIT}.timer"),
                    sbrain: job.sbrain.clone(),
                    env: job.env.clone(),
                    files: vec![service, timer],
                    log: None,
                })
            }
        }
    }

    /// Stops the job and removes its files. A job the service manager no longer knows is fine.
    pub fn uninstall(&self, schedule: &Schedule) -> Result<()> {
        match schedule.service.as_str() {
            "launchd" => {
                if let Some(plist) = schedule.files.first() {
                    let domain = gui_domain(plist).or_else(|_| gui_domain(&self.dir))?;
                    let _ = (self.control)(&["bootout", &format!("{domain}/{}", schedule.name)]);
                }
            }
            "systemd" => {
                let _ = (self.control)(&["--user", "disable", "--now", &schedule.name]);
            }
            other => bail!(
                "unknown scheduler {other} in {}",
                Schedule::path()?.display()
            ),
        }
        for file in &schedule.files {
            match std::fs::remove_file(file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("removing {}", file.display()));
                }
                _ => {}
            }
        }
        if schedule.service == "systemd" {
            let _ = (self.control)(&["--user", "daemon-reload"]);
        }
        Ok(())
    }
}

/// `gui/<uid>`: launchd's domain for the user who owns `path` (a file just written, so the current one).
#[cfg(unix)]
fn gui_domain(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("gui/{}", meta.uid()))
}

#[cfg(not(unix))]
fn gui_domain(_path: &Path) -> Result<String> {
    bail!("launchd runs only on macOS")
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    bail!("{program} {} failed: {}", args.join(" "), said.trim())
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn launchd_plist(job: &Job, log: &Path) -> String {
    let string = |s: &str| format!("<string>{}</string>", xml(s));
    let env = if job.env.is_empty() {
        String::new()
    } else {
        let pairs: String = job
            .env
            .iter()
            .map(|(k, v)| format!("\n    <key>{}</key>{}", xml(k), string(v)))
            .collect();
        format!("  <key>EnvironmentVariables</key>\n  <dict>{pairs}\n  </dict>\n")
    };
    let log = string(&log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `sbrain sweep schedule`; `sbrain sweep unschedule` removes it. -->
<plist version="1.0">
<dict>
  <key>Label</key>{label}
  <key>ProgramArguments</key>
  <array>{sbrain}<string>sweep</string><string>--due</string></array>
{env}  <key>StartInterval</key><integer>{interval}</integer>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key>{log}
  <key>StandardErrorPath</key>{log}
</dict>
</plist>
"#,
        label = string(LABEL),
        sbrain = string(&job.sbrain.to_string_lossy()),
        interval = TICK_MINUTES * 60,
    )
}

/// A value for a systemd unit file: double-quoted, with `\`, `"` and the `%` of specifiers escaped.
fn unit_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', r"\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

pub fn systemd_service(job: &Job) -> String {
    let env: String = job
        .env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", unit_quote(&format!("{k}={v}"))))
        .collect();
    format!(
        "# Written by `sbrain sweep schedule`; `sbrain sweep unschedule` removes it.\n\
         [Unit]\n\
         Description=sbrain sweep: distill coding sessions into the vault when due\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} sweep --due\n\
         {env}",
        unit_quote(&job.sbrain.to_string_lossy())
    )
}

pub fn systemd_timer() -> String {
    format!(
        "# Written by `sbrain sweep schedule`; `sbrain sweep unschedule` removes it.\n\
         [Unit]\n\
         Description=Ask sbrain whether a sweep is due\n\
         \n\
         [Timer]\n\
         OnCalendar=*:0/{TICK_MINUTES}\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    fn job() -> Job {
        Job {
            sbrain: PathBuf::from("/opt/homebrew/bin/sbrain"),
            env: vec![(
                "SBRAIN_SETTINGS".into(),
                "/tmp/a&b \"x\" 100%/settings.json".into(),
            )],
        }
    }

    fn host(kind: Kind, root: &Path) -> (Host, Rc<RefCell<Vec<String>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seen = calls.clone();
        let host = Host {
            kind,
            dir: root.join("agents"),
            log_dir: root.join("sbrain"),
            control: Box::new(move |args| {
                seen.borrow_mut().push(args.join(" "));
                Ok(())
            }),
        };
        (host, calls)
    }

    #[test]
    fn plist_runs_due_sweeps_every_tick_and_escapes_values() {
        let plist = launchd_plist(
            &job(),
            Path::new("/Users/x/Library/Application Support/sbrain/sweep.log"),
        );
        assert!(plist.contains(
            "<string>/opt/homebrew/bin/sbrain</string><string>sweep</string><string>--due</string>"
        ));
        assert!(plist.contains("<key>StartInterval</key><integer>900</integer>"));
        assert!(plist.contains(
            "<key>SBRAIN_SETTINGS</key><string>/tmp/a&amp;b &quot;x&quot; 100%/settings.json</string>"
        ));
        assert!(
            plist
                .contains("<string>/Users/x/Library/Application Support/sbrain/sweep.log</string>")
        );
        let bare = Job {
            sbrain: PathBuf::from("/x/sbrain"),
            env: Vec::new(),
        };
        assert!(!launchd_plist(&bare, Path::new("/l")).contains("EnvironmentVariables"));
    }

    #[test]
    fn systemd_units_quote_paths_and_specifiers() {
        let mut j = job();
        j.sbrain = PathBuf::from("/home/x/my apps/sbrain");
        let service = systemd_service(&j);
        assert!(service.contains("ExecStart=\"/home/x/my apps/sbrain\" sweep --due\n"));
        assert!(
            service.contains(
                "Environment=\"SBRAIN_SETTINGS=/tmp/a&b \\\"x\\\" 100%%/settings.json\"\n"
            )
        );
        assert!(service.contains("Type=oneshot"));
        let timer = systemd_timer();
        assert!(timer.contains("OnCalendar=*:0/15\n"));
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains("WantedBy=timers.target"));
    }

    #[test]
    fn launchd_install_and_uninstall() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, calls) = host(Kind::Launchd, tmp.path());
        let schedule = host.install(&job()).unwrap();
        let plist = tmp.path().join(format!("agents/{LABEL}.plist"));
        assert_eq!(schedule.files, std::slice::from_ref(&plist));
        assert_eq!(schedule.log, Some(tmp.path().join("sbrain/sweep.log")));
        assert_eq!(schedule.sbrain, PathBuf::from("/opt/homebrew/bin/sbrain"));
        assert!(plist.exists());
        let domain = gui_domain(&plist).unwrap();
        assert_eq!(
            *calls.borrow(),
            [
                format!("bootout {domain}/{LABEL}"),
                format!("bootstrap {domain} {}", plist.display())
            ]
        );

        calls.borrow_mut().clear();
        host.uninstall(&schedule).unwrap();
        assert!(!plist.exists());
        assert_eq!(*calls.borrow(), [format!("bootout {domain}/{LABEL}")]);
        host.uninstall(&schedule).unwrap();
    }

    #[test]
    fn systemd_install_and_uninstall() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, calls) = host(Kind::Systemd, tmp.path());
        let schedule = host.install(&job()).unwrap();
        let dir = tmp.path().join("agents");
        assert_eq!(
            schedule.files,
            [
                dir.join("sbrain-sweep.service"),
                dir.join("sbrain-sweep.timer")
            ]
        );
        assert_eq!(schedule.name, "sbrain-sweep.timer");
        assert_eq!(schedule.log, None, "the journal keeps the output");
        assert_eq!(
            *calls.borrow(),
            [
                "--user daemon-reload",
                "--user enable --now sbrain-sweep.timer"
            ]
        );

        calls.borrow_mut().clear();
        host.uninstall(&schedule).unwrap();
        assert!(schedule.files.iter().all(|f| !f.exists()));
        assert_eq!(
            *calls.borrow(),
            [
                "--user disable --now sbrain-sweep.timer",
                "--user daemon-reload"
            ]
        );
    }

    #[test]
    fn a_failing_service_manager_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut host, _) = host(Kind::Systemd, tmp.path());
        host.control = Box::new(|args| bail!("systemctl {} failed: no user bus", args.join(" ")));
        let err = host.install(&job()).unwrap_err().to_string();
        assert!(err.contains("no user bus"), "{err}");
        assert!(
            !tmp.path().join("agents/sbrain-sweep.service").exists(),
            "nothing is left behind"
        );
    }
}
