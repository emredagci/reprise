//! `sbrain sweep`'s bookkeeping. `.sbrain/cache/sweep.json` keeps since when sessions are swept and the
//! last run; the job ledger (`crate::ledger`) keeps each session's distilling, retried with backoff when
//! it fails, and the sessions agents reported as ended. `.sbrain/cache/sweep.lock` keeps two runs apart.
//! Reading transcripts and running the engine belong to the client; writing records to `distill`.

use std::fs::File;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ledger::{Ledger, State as JobState};
use crate::{Error, Result, Vault};

/// Ledger kinds of the sweep: a session's distilling (key: the session id), and a session an agent
/// reported as ended, due without waiting to settle.
pub const DISTILL: &str = "distill";
pub const ENDED: &str = "ended";

/// Since when sessions are swept, and the last run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Unix seconds of the first run. Transcripts last written before it are history and are never
    /// distilled, so turning the sweep on does not distill every past session at once.
    pub baseline: Option<i64>,
    /// The latest run that checked for due sessions; `sbrain sweep --due` schedules from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<Run>,
}

/// One run of `sbrain sweep`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// Unix seconds.
    pub started: i64,
    pub finished: i64,
    /// The engine's name, and the model when one was chosen.
    pub client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub sessions: Vec<RunSession>,
    /// Due sessions left for a later run: past the run's limit, or stopped while offline.
    #[serde(default)]
    pub remaining: usize,
    /// The run stopped because there was no connection.
    #[serde(default)]
    pub offline: bool,
}

/// What a run did with one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSession {
    pub session: String,
    /// ok, skipped or failed.
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// The card written, relative to the vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    #[serde(default)]
    pub items_added: usize,
    #[serde(default)]
    pub items_updated: usize,
    #[serde(default)]
    pub decisions_added: usize,
    /// Rules proposed from the session.
    #[serde(default)]
    pub rules_proposed: usize,
    #[serde(default)]
    pub state_filled: bool,
}

impl Run {
    pub fn count(&self, status: &str) -> usize {
        self.sessions.iter().filter(|s| s.status == status).count()
    }
}

impl State {
    fn path(vault: &Vault) -> PathBuf {
        vault.cache_dir().join("sweep.json")
    }

    /// The saved state; a missing or unreadable file is a first run.
    pub fn load(vault: &Vault) -> Self {
        crate::safe::read_to_string(Self::path(vault))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, vault: &Vault) -> Result<()> {
        let json = serde_json::to_string_pretty(self).expect("the state serializes");
        crate::scaffold::write(&Self::path(vault), &json)
    }
}

/// A distill job's payload: the transcript's size when it was handled (also after a failure, so a
/// retry comes early only when the session went on), how it went, and when (`YYYY-MM-DD HH:MM`).
pub fn distilled(size: Option<u64>, status: &str, reason: &str, project: &str, at: &str) -> Value {
    json!({"size": size, "status": status, "reason": reason, "project": project, "at": at})
}

/// Whether a transcript is due: written after the baseline, left alone for `settle_seconds` (or
/// reported as ended), and not yet handled at this size. A failed session waits out its backoff and a
/// given-up one is left alone, unless its transcript has grown since; `force` (one session asked for by
/// name) skips the wait.
#[allow(clippy::too_many_arguments)]
pub fn is_due(
    state: &State,
    ledger: &Ledger,
    session: &str,
    size: u64,
    modified: i64,
    now: i64,
    settle_seconds: i64,
    force: bool,
) -> Result<bool> {
    let Some(baseline) = state.baseline else {
        return Ok(false);
    };
    let ended = ledger
        .get(ENDED, session)?
        .is_some_and(|j| j.state == JobState::Pending);
    let settled = now - modified >= settle_seconds || ended;
    if modified <= baseline || !settled {
        return Ok(false);
    }
    let Some(job) = ledger.get(DISTILL, session)? else {
        return Ok(true);
    };
    let grew = job.payload["size"].as_u64() != Some(size);
    Ok(match job.state {
        JobState::Done => grew,
        JobState::Pending => grew || force || job.next_at <= now,
        JobState::GaveUp => grew || force,
    })
}

/// When the ledger next has sweep work: an ended session, a distilling to try again, a commit to retry.
pub fn waiting(ledger: &Ledger) -> Result<Option<i64>> {
    let mut times = Vec::new();
    for kind in [ENDED, DISTILL, crate::git::COMMIT] {
        times.extend(ledger.next_due(kind)?);
    }
    Ok(times.into_iter().min())
}

/// Where the engine runs to distill: beside sbrain's settings, outside the vault and outside any
/// repository. Run inside the vault, the engine would take the vault's project settings (hooks,
/// `CLAUDE.md`), which a git remote can supply, and run them during an unattended sweep.
pub fn distill_dir() -> Result<PathBuf> {
    Ok(crate::Settings::default_path()?.with_file_name("distill"))
}

/// When a run leaves sessions for later, the next one starts this soon instead of on schedule.
pub const FOLLOW_UP_SECONDS: i64 = 5 * 60;

impl State {
    /// Unix seconds of the next scheduled run, `every_hours` apart: when the ledger next has work
    /// (`waiting`: an ended session, a retry), now when sweeping never ran, soon after a run that left
    /// sessions for later, else the schedule counted from the last run. `None` when it runs only by hand
    /// (`every_hours` 0).
    pub fn next_run(&self, every_hours: u32, now: i64, waiting: Option<i64>) -> Option<i64> {
        if every_hours == 0 {
            return None;
        }
        let follow_up = self
            .last_run
            .as_ref()
            .filter(|r| r.remaining > 0 && !r.offline)
            .map(|r| r.finished + FOLLOW_UP_SECONDS);
        let scheduled = self
            .last_run
            .as_ref()
            .map(|r| r.finished)
            .or(self.baseline)
            .map_or(now, |last| last + i64::from(every_hours) * 3600);
        [waiting, follow_up, Some(scheduled)]
            .into_iter()
            .flatten()
            .min()
    }
}

/// Notes that a session ended, so the next run distills it without waiting for it to settle. Before
/// the first run there is nothing to note: sessions before the baseline are never distilled.
pub fn mark_ended(vault: &Vault, session: &str, now: i64) -> Result<()> {
    if State::load(vault).baseline.is_none() || session.trim().is_empty() {
        return Ok(());
    }
    Ledger::open(vault)?.put(ENDED, session, &json!({}), now)
}

/// What `sbrain sweep schedule` installed on this computer, kept beside the settings (`schedule.json`), so
/// `unschedule` removes exactly that and the doctor can tell whether the sweep runs by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// `launchd` or `systemd`.
    pub service: String,
    /// The job: launchd's label, or the systemd timer.
    pub name: String,
    /// The sbrain it runs.
    pub sbrain: PathBuf,
    /// Variables the job sets for sbrain, such as `SBRAIN_SETTINGS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// Files written for it.
    pub files: Vec<PathBuf>,
    /// Where its output goes, when that is a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
}

impl Schedule {
    pub fn path() -> Result<PathBuf> {
        Ok(crate::Settings::default_path()?.with_file_name("schedule.json"))
    }

    /// The schedule on this computer, if one is installed.
    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        let text = match crate::safe::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path, source }),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| Error::SettingsInvalid { path, source })
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(self).expect("the schedule serializes");
        crate::scaffold::write(&Self::path()?, &json)
    }

    /// Forgets the schedule; there being none is fine.
    pub fn forget() -> Result<()> {
        let path = Self::path()?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::Io { path, source: e })
            }
            _ => Ok(()),
        }
    }
}

/// Held while a sweep runs; dropping it lets the next one in.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// Takes the sweep lock; `None` when another run holds it.
pub fn lock(vault: &Vault) -> Result<Option<Lock>> {
    let dir = vault.cache_dir();
    crate::safe::create_dir_all(&dir)?;
    let path = dir.join("sweep.lock");
    crate::safe::check(&path)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lock { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(Error::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::open_at(&tmp.path().join("ledger.sqlite")).unwrap();
        (tmp, l)
    }

    #[test]
    fn due_only_after_the_baseline_and_once_settled() {
        let (_t, l) = ledger();
        let mut state = State::default();
        let due = |state: &State, s: &str, size, modified, force| {
            is_due(state, &l, s, size, modified, 10_000, 60, force).unwrap()
        };
        assert!(
            !due(&state, "s", 10, 500, false),
            "no baseline yet: nothing is due"
        );
        state.baseline = Some(1_000);
        assert!(
            !due(&state, "s", 10, 900, false),
            "history before the baseline"
        );
        assert!(!due(&state, "s", 10, 9_990, false), "still being written");
        assert!(due(&state, "s", 10, 5_000, false));

        l.succeed(
            DISTILL,
            "s",
            &distilled(Some(10), "ok", "", "app", ""),
            9_000,
        )
        .unwrap();
        assert!(!due(&state, "s", 10, 5_000, false), "handled at this size");
        assert!(due(&state, "s", 12, 5_000, false), "grew since");

        // A failure waits out its backoff, unless asked for by name or grown.
        l.fail(
            DISTILL,
            "f",
            &distilled(Some(10), "failed", "x", "", ""),
            "x",
            9_990,
        )
        .unwrap();
        assert!(!due(&state, "f", 10, 5_000, false), "backing off");
        assert!(due(&state, "f", 10, 5_000, true), "asked for by name");
        assert!(due(&state, "f", 11, 5_000, false), "grew since the failure");

        assert!(!due(&state, "e", 10, 9_990, false), "still settling");
        l.put(ENDED, "e", &json!({}), 9_995).unwrap();
        assert!(due(&state, "e", 10, 9_990, false), "ended: due at once");
    }

    #[test]
    fn a_given_up_session_waits_for_more_transcript() {
        let (_t, l) = ledger();
        let state = State {
            baseline: Some(1),
            ..State::default()
        };
        let p = distilled(Some(10), "failed", "x", "", "");
        let mut now = 100;
        while l.fail(DISTILL, "g", &p, "x", now).unwrap() != JobState::GaveUp {
            now += 1;
        }
        let due = |size| is_due(&state, &l, "g", size, 50, i64::MAX / 2, 0, false).unwrap();
        assert!(!due(10), "given up");
        assert!(due(20), "the session went on: one more try");
    }

    #[test]
    fn ended_sessions_are_noted_only_after_the_first_run() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        mark_ended(&vault, "abc", 5).unwrap();
        let l = Ledger::open(&vault).unwrap();
        assert!(l.get(ENDED, "abc").unwrap().is_none(), "no baseline yet");
        State {
            baseline: Some(1),
            ..State::default()
        }
        .save(&vault)
        .unwrap();
        mark_ended(&vault, "abc", 5).unwrap();
        assert_eq!(l.get(ENDED, "abc").unwrap().unwrap().next_at, 5);
        assert_eq!(waiting(&l).unwrap(), Some(5));
    }

    #[test]
    fn state_round_trips_and_the_lock_is_exclusive() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = crate::vault::tests::new_vault(tmp.path());
        assert_eq!(State::load(&vault), State::default());
        let mut state = State {
            baseline: Some(7),
            ..State::default()
        };
        state.save(&vault).unwrap();
        assert_eq!(State::load(&vault), state);
        state.last_run = Some(Run {
            started: 1,
            finished: 2,
            client: "Claude Code".into(),
            sessions: vec![RunSession {
                session: "abc".into(),
                status: "failed".into(),
                ..RunSession::default()
            }],
            ..Run::default()
        });
        state.save(&vault).unwrap();
        let loaded = State::load(&vault);
        assert_eq!(loaded.last_run.as_ref().unwrap().count("failed"), 1);
        assert_eq!(loaded, state);

        let held = lock(&vault).unwrap();
        assert!(held.is_some());
        assert!(
            lock(&vault).unwrap().is_none(),
            "a second run is turned away"
        );
        drop(held);
        // A process another test starts at that moment inherits the lock file until it execs, so the
        // lock can stay taken for a few milliseconds after it is dropped.
        let free = (0..50).any(|_| {
            lock(&vault).unwrap().is_some() || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        });
        assert!(free, "the lock is free once its holder is gone");
    }

    #[test]
    fn next_run_follows_the_schedule_ended_sessions_and_left_over_work() {
        let hour = 3600;
        let now = 100 * hour;
        let mut state = State::default();
        assert_eq!(state.next_run(1, now, None), Some(now), "never run: now");
        assert_eq!(state.next_run(0, now, None), None, "by hand only");

        state.baseline = Some(now - 10 * hour);
        assert_eq!(
            state.next_run(3, now, None),
            Some(now - 7 * hour),
            "overdue"
        );
        state.last_run = Some(Run {
            finished: now - hour,
            ..Run::default()
        });
        assert_eq!(state.next_run(3, now, None), Some(now + 2 * hour));
        assert_eq!(state.next_run(0, now, None), None);

        state.last_run.as_mut().unwrap().remaining = 2;
        assert_eq!(
            state.next_run(3, now, None),
            Some(now - hour + FOLLOW_UP_SECONDS),
            "sessions left for later follow soon"
        );
        state.last_run.as_mut().unwrap().offline = true;
        assert_eq!(
            state.next_run(3, now, None),
            Some(now + 2 * hour),
            "offline: back on schedule"
        );

        assert_eq!(
            state.next_run(3, now, Some(now - 60)),
            Some(now - 60),
            "the ledger has work (an ended session): due now"
        );
        assert_eq!(
            state.next_run(3, now, Some(now + 600)),
            Some(now + 600),
            "a retry in ten minutes comes first"
        );
        assert_eq!(state.next_run(0, now, Some(now)), None, "by hand only");
    }
}
