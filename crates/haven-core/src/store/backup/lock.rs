//! Cross-process lock for the backup store's mutating critical sections.
//!
//! The daily trigger fires from BOTH the CLI and the long-lived MCP server, so
//! two processes can race a snapshot against a GC — and a mark-and-sweep that
//! reaps an object a concurrent snapshot is about to reference is silent data
//! loss. This lock serializes (write objects + manifest) against (rotate + GC) so
//! they can never overlap. It is an O_EXCL lockfile (`backups/.lock`) — no new
//! dependency; `create_new` is an atomic exclusive create. A crashed holder's
//! lock is stolen once stale (mtime older than [`LOCK_STALE`]).
//!
//! This is NOT the SQLite `BEGIN EXCLUSIVE` probe (`require_unlocked`): that
//! guards the live DB file during a restore swap and is unavailable exactly when
//! we restore *because* the DB is unreadable. Different resource, different lock.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;
use crate::time;
use crate::util::older_than;

const LOCK_NAME: &str = ".lock";
/// A held lock older than this is presumed abandoned by a crash and may be stolen.
/// Generous (far under the once-a-day cadence) so a long snapshot is never
/// preempted, yet a crash self-heals within the hour.
const LOCK_STALE: Duration = Duration::from_secs(3600);

/// An acquired backup lock; releases (removes the lockfile) on drop.
pub(super) struct BackupLock {
    path: PathBuf,
    held: bool,
}

impl BackupLock {
    /// Try to acquire the lock. `Ok(Some(_))` = acquired; `Ok(None)` = another
    /// live process holds it (caller no-ops — a missed opportunistic backup must
    /// never fail the user's command). Steals a stale lock left by a crash.
    pub(super) fn try_acquire(backups_root: &Path) -> Result<Option<BackupLock>> {
        Self::try_acquire_with_stale(backups_root, LOCK_STALE)
    }

    fn try_acquire_with_stale(backups_root: &Path, stale: Duration) -> Result<Option<BackupLock>> {
        std::fs::create_dir_all(backups_root)?;
        let path = backups_root.join(LOCK_NAME);
        match Self::create(&path) {
            Ok(lock) => Ok(Some(lock)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if older_than(&path, stale) {
                    // Stale: a crashed holder. Steal it (remove + retry once).
                    let _ = std::fs::remove_file(&path);
                    match Self::create(&path) {
                        Ok(lock) => Ok(Some(lock)),
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
                        Err(e) => Err(e.into()),
                    }
                } else {
                    Ok(None)
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    fn create(path: &Path) -> std::io::Result<BackupLock> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        // Forensic body (pid + epoch secs); staleness uses the file mtime, not this.
        let _ = writeln!(f, "{}\n{}", std::process::id(), time::now_secs());
        let _ = f.sync_all();
        Ok(BackupLock {
            path: path.to_path_buf(),
            held: true,
        })
    }
}

impl Drop for BackupLock {
    fn drop(&mut self) {
        if self.held {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_then_released() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let lock = BackupLock::try_acquire(root).unwrap();
        assert!(lock.is_some(), "first acquire succeeds");
        assert!(
            BackupLock::try_acquire(root).unwrap().is_none(),
            "a fresh held lock blocks a second acquire"
        );
        drop(lock);
        assert!(
            BackupLock::try_acquire(root).unwrap().is_some(),
            "release lets the next acquirer in"
        );
    }

    #[test]
    fn steals_a_stale_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root).unwrap();
        // A leftover lockfile from a crashed process (no live guard).
        std::fs::write(root.join(LOCK_NAME), b"99999\n0\n").unwrap();
        // Fresh threshold: not stolen.
        assert!(BackupLock::try_acquire(root).unwrap().is_none());
        // Zero threshold: any existing lock counts as stale → stolen.
        let lock = BackupLock::try_acquire_with_stale(root, Duration::ZERO).unwrap();
        assert!(lock.is_some(), "a stale lock is stolen");
    }
}
