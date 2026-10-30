//! The job ledger: work that has to happen and may fail, kept in
//! `.reprise/cache/ledger.sqlite` so nothing is lost and a failure is tried again later. The records stay
//! in Markdown; the ledger is machine-local, gitignored, and losing it costs only the retry history.
//!
//! A job is named by its kind and key (a session id, a commit's id). It is pending until it succeeds
//! (done) or has failed `MAX_ATTEMPTS` times (gave up). After a failure it waits longer each time
//! (`BACKOFF`) before it is due again. Every change is one SQLite transaction, so a hook and a sweep
//! can write at the same time.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::{Error, Result, Vault};

/// Seconds to wait after the 1st, 2nd, … failure; the last applies from then on.
pub const BACKOFF: [i64; 6] = [60, 5 * 60, 30 * 60, 2 * 3600, 12 * 3600, 24 * 3600];
/// Failures after which a job is given up; `requeue` brings it back.
pub const MAX_ATTEMPTS: u32 = 8;
/// The schema this version writes (`PRAGMA user_version`).
const SCHEMA: i32 = 1;
const FILE: &str = "ledger.sqlite";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Pending,
    Done,
    GaveUp,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::GaveUp => "gave-up",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "done" => Self::Done,
            "gave-up" => Self::GaveUp,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub kind: String,
    pub key: String,
    pub state: State,
    /// Failures so far.
    pub attempts: u32,
    pub last_error: Option<String>,
    /// Unix seconds from which a pending job is due.
    pub next_at: i64,
    /// What the job needs, or what it left behind.
    pub payload: Value,
    /// Unix seconds of the last change.
    pub updated: i64,
}

impl Job {
    pub fn is_due(&self, now: i64) -> bool {
        self.state == State::Pending && self.next_at <= now
    }
}

pub struct Ledger {
    conn: Connection,
    path: PathBuf,
}

/// Seconds to wait after the `attempts`-th failure.
pub fn backoff(attempts: u32) -> i64 {
    let i = (attempts.max(1) as usize - 1).min(BACKOFF.len() - 1);
    BACKOFF[i]
}

impl Ledger {
    /// The vault's ledger, created on first use.
    pub fn open(vault: &Vault) -> Result<Self> {
        Self::open_at(&vault.cache_dir().join(FILE))
    }

    /// The ledger of the vault rooted at `root`, without opening the vault.
    pub fn open_root(root: &Path) -> Result<Self> {
        Self::open_at(&root.join(".reprise/cache").join(FILE))
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        let err = |e: &dyn std::fmt::Display| Error::Ledger {
            path: path.to_path_buf(),
            message: e.to_string(),
        };
        if let Some(dir) = path.parent() {
            crate::safe::create_dir_all(dir)?;
        }
        crate::safe::check(path)?;
        let conn = Connection::open(path).map_err(|e| err(&e))?;
        // A hook and a sweep may write at once: wait for the other rather than fail.
        conn.busy_timeout(Duration::from_secs(10))
            .map_err(|e| err(&e))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| err(&e))?;
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|e| err(&e))?;
        if version > SCHEMA {
            return Err(err(&format!(
                "written by a newer reprise (schema {version}); update reprise"
            )));
        }
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS jobs (
                    kind TEXT NOT NULL,
                    key TEXT NOT NULL,
                    state TEXT NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    next_at INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    updated INTEGER NOT NULL,
                    PRIMARY KEY (kind, key)
                );
                CREATE INDEX IF NOT EXISTS jobs_due ON jobs (kind, state, next_at);
                PRAGMA user_version = 1;",
            )
            .map_err(|e| err(&e))?;
        }
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    fn err(&self, e: impl std::fmt::Display) -> Error {
        Error::Ledger {
            path: self.path.clone(),
            message: e.to_string(),
        }
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
        let payload: String = r.get(6)?;
        Ok(Job {
            kind: r.get(0)?,
            key: r.get(1)?,
            state: State::parse(&r.get::<_, String>(2)?),
            attempts: r.get(3)?,
            last_error: r.get(4)?,
            next_at: r.get(5)?,
            payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
            updated: r.get(7)?,
        })
    }

    const COLUMNS: &str = "kind, key, state, attempts, last_error, next_at, payload, updated";

    pub fn get(&self, kind: &str, key: &str) -> Result<Option<Job>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {} FROM jobs WHERE kind = ?1 AND key = ?2",
                    Self::COLUMNS
                ),
                params![kind, key],
                Self::row,
            )
            .optional()
            .map_err(|e| self.err(e))
    }

    /// Adds the job, or makes an existing one pending again from scratch: due at `now`, no failures.
    pub fn put(&self, kind: &str, key: &str, payload: &Value, now: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO jobs (kind, key, state, attempts, last_error, next_at, payload, updated)
                 VALUES (?1, ?2, 'pending', 0, NULL, ?3, ?4, ?3)
                 ON CONFLICT (kind, key) DO UPDATE SET
                    state = 'pending', attempts = 0, last_error = NULL, next_at = ?3,
                    payload = ?4, updated = ?3",
                params![kind, key, now, payload.to_string()],
            )
            .map(|_| ())
            .map_err(|e| self.err(e))
    }

    /// The job is done; `payload` records what it left behind. A job not in the ledger is added done.
    pub fn succeed(&self, kind: &str, key: &str, payload: &Value, now: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO jobs (kind, key, state, attempts, last_error, next_at, payload, updated)
                 VALUES (?1, ?2, 'done', 0, NULL, ?3, ?4, ?3)
                 ON CONFLICT (kind, key) DO UPDATE SET
                    state = 'done', last_error = NULL, payload = ?4, updated = ?3",
                params![kind, key, now, payload.to_string()],
            )
            .map(|_| ())
            .map_err(|e| self.err(e))
    }

    /// Counts a failure: the job waits `backoff(attempts)` before it is due again, or is given up after
    /// `MAX_ATTEMPTS`. A job not in the ledger is added with `payload`. Returns the state it is left in.
    pub fn fail(
        &self,
        kind: &str,
        key: &str,
        payload: &Value,
        error: &str,
        now: i64,
    ) -> Result<State> {
        let tx = self.conn.unchecked_transaction().map_err(|e| self.err(e))?;
        let attempts: u32 = tx
            .query_row(
                "SELECT attempts FROM jobs WHERE kind = ?1 AND key = ?2 AND state = 'pending'",
                params![kind, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| self.err(e))?
            .unwrap_or(0)
            + 1;
        let state = if attempts >= MAX_ATTEMPTS {
            State::GaveUp
        } else {
            State::Pending
        };
        tx.execute(
            "INSERT INTO jobs (kind, key, state, attempts, last_error, next_at, payload, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (kind, key) DO UPDATE SET
                state = ?3, attempts = ?4, last_error = ?5, next_at = ?6, payload = ?7, updated = ?8",
            params![
                kind,
                key,
                state.as_str(),
                attempts,
                error,
                now + backoff(attempts),
                payload.to_string(),
                now
            ],
        )
        .map_err(|e| self.err(e))?;
        tx.commit().map_err(|e| self.err(e))?;
        Ok(state)
    }

    /// Pending jobs of `kind` due at `now`, the longest waiting first.
    pub fn due(&self, kind: &str, now: i64) -> Result<Vec<Job>> {
        self.select(
            "WHERE kind = ?1 AND state = 'pending' AND next_at <= ?2 ORDER BY next_at, key",
            params![kind, now],
        )
    }

    /// Every job of `kind` in `state`.
    pub fn list(&self, kind: &str, state: State) -> Result<Vec<Job>> {
        self.select(
            "WHERE kind = ?1 AND state = ?2 ORDER BY updated DESC, key",
            params![kind, state.as_str()],
        )
    }

    /// When the earliest pending job of `kind` is due, if any is pending.
    pub fn next_due(&self, kind: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT MIN(next_at) FROM jobs WHERE kind = ?1 AND state = 'pending'",
                params![kind],
                |r| r.get(0),
            )
            .map_err(|e| self.err(e))
    }

    /// Makes given-up jobs of `kind` (only `key`, when given) pending and due again; returns how many.
    pub fn requeue(&self, kind: &str, key: Option<&str>, now: i64) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE jobs SET state = 'pending', attempts = 0, next_at = ?3, updated = ?3
                 WHERE kind = ?1 AND state = 'gave-up' AND (?2 IS NULL OR key = ?2)",
                params![kind, key, now],
            )
            .map_err(|e| self.err(e))
    }

    pub fn remove(&self, kind: &str, key: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM jobs WHERE kind = ?1 AND key = ?2",
                params![kind, key],
            )
            .map(|_| ())
            .map_err(|e| self.err(e))
    }

    fn select(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<Job>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {} FROM jobs {filter}", Self::COLUMNS))
            .map_err(|e| self.err(e))?;
        stmt.query_map(args, Self::row)
            .map_err(|e| self.err(e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| self.err(e))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::open_at(&tmp.path().join("cache/ledger.sqlite")).unwrap();
        (tmp, l)
    }

    #[test]
    fn failures_back_off_then_give_up() {
        let (_t, l) = ledger();
        let p = json!({"size": 10});
        assert_eq!(
            l.fail("distill", "s1", &p, "boom", 1000).unwrap(),
            State::Pending
        );
        let job = l.get("distill", "s1").unwrap().unwrap();
        assert_eq!((job.attempts, job.next_at), (1, 1000 + 60));
        assert_eq!(job.last_error.as_deref(), Some("boom"));
        assert!(l.due("distill", 1059).unwrap().is_empty(), "waits a minute");
        assert_eq!(l.due("distill", 1060).unwrap().len(), 1);

        let mut now = 1060;
        for n in 2..MAX_ATTEMPTS {
            assert_eq!(
                l.fail("distill", "s1", &p, "boom", now).unwrap(),
                State::Pending
            );
            assert_eq!(
                l.get("distill", "s1").unwrap().unwrap().next_at,
                now + backoff(n)
            );
            now += backoff(n);
        }
        assert_eq!(
            l.fail("distill", "s1", &p, "last", now).unwrap(),
            State::GaveUp
        );
        assert!(
            l.due("distill", i64::MAX).unwrap().is_empty(),
            "given up: never due"
        );
        let gave_up = l.list("distill", State::GaveUp).unwrap();
        assert_eq!(gave_up[0].last_error.as_deref(), Some("last"));
        assert_eq!(gave_up[0].attempts, MAX_ATTEMPTS);

        assert_eq!(l.requeue("distill", None, now).unwrap(), 1);
        let job = l.get("distill", "s1").unwrap().unwrap();
        assert!(job.is_due(now) && job.attempts == 0, "{job:?}");
    }

    #[test]
    fn backoff_grows_and_stops_growing() {
        assert_eq!(backoff(1), 60);
        assert_eq!(backoff(2), 300);
        assert_eq!(backoff(6), 86400);
        assert_eq!(backoff(50), 86400);
    }

    #[test]
    fn put_succeed_and_remove() {
        let (_t, l) = ledger();
        l.fail("commit", "c1", &json!({}), "index.lock", 5).unwrap();
        l.put("commit", "c1", &json!({"files": ["a.md"]}), 10)
            .unwrap();
        let job = l.get("commit", "c1").unwrap().unwrap();
        assert_eq!(
            (job.state, job.attempts, job.next_at),
            (State::Pending, 0, 10)
        );
        assert_eq!(job.last_error, None, "put starts over");
        assert_eq!(l.next_due("commit").unwrap(), Some(10));

        l.succeed("commit", "c1", &json!({"sha": "abc"}), 11)
            .unwrap();
        let job = l.get("commit", "c1").unwrap().unwrap();
        assert_eq!(job.state, State::Done);
        assert_eq!(job.payload["sha"], "abc");
        assert_eq!(l.next_due("commit").unwrap(), None);
        // A failure after success starts counting again.
        l.fail("commit", "c1", &json!({}), "x", 12).unwrap();
        assert_eq!(l.get("commit", "c1").unwrap().unwrap().attempts, 1);

        l.remove("commit", "c1").unwrap();
        assert!(l.get("commit", "c1").unwrap().is_none());
    }

    #[test]
    fn two_connections_write_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ledger.sqlite");
        Ledger::open_at(&path).unwrap();
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let l = Ledger::open_at(&path).unwrap();
                    for i in 0..25 {
                        l.put("ended", &format!("{w}-{i}"), &json!({}), i).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let l = Ledger::open_at(&path).unwrap();
        assert_eq!(l.list("ended", State::Pending).unwrap().len(), 100);
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ledger.sqlite");
        Ledger::open_at(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 9)
            .unwrap();
        let err = Ledger::open_at(&path).err().unwrap().to_string();
        assert!(err.contains("newer reprise"), "{err}");
    }
}
