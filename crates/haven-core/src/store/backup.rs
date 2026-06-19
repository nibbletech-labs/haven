//! Local self-backups (HV-41): corruption-safe snapshots, rotation, restore.
//!
//! `~/.haven` is the single copy of the work-graph (`haven.db` + per-project
//! `items/` trees). A snapshot is a SQLite **online backup** of the live WAL
//! database — never a raw file copy, which under WAL would miss committed pages
//! still in `haven.db-wal` — plus a `tar.gz` of each project's content tree,
//! written under `<content_root>/backups/<UTC-ts>/`.
//!
//! Integrity is gated by `PRAGMA quick_check`: a snapshot of a database that
//! fails the check is quarantined to `<ts>-SUSPECT/`, which **freezes** rotation
//! of the good snapshots (so a corruption can't silently age out the last clean
//! copy) until the operator removes it. Backups are opportunistic — at most one
//! per day, gated by a `last_backup` meta marker, fired from the CLI/MCP command
//! paths (no cron/launchd).
//!
//! All snapshot logic is here in `haven-core` so the CLI and MCP share one
//! implementation (SPEC §7); the clients are thin wrappers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use super::Store;
use crate::error::{HavenError, Result};
use crate::time;

/// Quarantine marker suffix on a snapshot directory name.
const SUSPECT_SUFFIX: &str = "-SUSPECT";
/// Retention: keep the newest snapshot of each of the 7 most-recent days …
const KEEP_DAILY: usize = 7;
/// … and the newest snapshot of each of the 4 most-recent ISO weeks.
const KEEP_WEEKLY: usize = 4;

/// Integrity verdict for a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Integrity {
    Ok,
    Suspect,
}

/// One project's content archive inside a snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectArchive {
    pub key: String,
    pub archive: String,
    pub bytes: u64,
}

/// Outcome of taking a snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct BackupReport {
    /// The snapshot directory name (`<UTC-ts>` or `<UTC-ts>-SUSPECT`).
    pub id: String,
    pub db_bytes: u64,
    pub projects: Vec<ProjectArchive>,
    pub integrity: Integrity,
    pub quarantined: bool,
}

/// A snapshot as listed by `haven backup list`.
#[derive(Debug, Clone, Serialize)]
pub struct BackupEntry {
    pub id: String,
    /// The UTC timestamp portion of the id (the `-SUSPECT` suffix stripped).
    pub created_at: String,
    pub size_bytes: u64,
    pub integrity: Integrity,
}

/// Outcome of a restore.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub restored: String,
    /// The safety snapshot of pre-restore state (empty only if it could not be
    /// captured at all).
    pub safety_snapshot: String,
    pub files_restored: usize,
}

impl Store {
    /// Take a snapshot now: `quick_check` the live DB, online-backup it, and
    /// `tar.gz` every project content tree under `backups_root/<UTC-ts>/`. A
    /// failing check quarantines the snapshot (`<ts>-SUSPECT/`) and skips both
    /// rotation (freeze) and the `last_backup` marker — so a corrupt day leaves
    /// the daily gate open for a later command to still capture a clean copy.
    pub fn backup_now(&self, backups_root: &Path) -> Result<BackupReport> {
        std::fs::create_dir_all(backups_root)?;
        let (id, integrity) = snapshot_with_conn(&self.conn, self.content_root(), backups_root)?;
        if integrity == Integrity::Ok {
            self.meta_set("last_backup", &time::ymd_string(time::today_ymd()))?;
            rotate(backups_root)?;
        }
        build_report(backups_root, &id, integrity)
    }

    /// Opportunistic daily backup: a no-op if a clean backup already ran today
    /// (`last_backup` marker), or if a snapshot was already quarantined today
    /// (avoid re-quarantining on every command of a corrupt day — the freeze +
    /// warning already persist). Otherwise [`Store::backup_now`]. The marker
    /// check is a single indexed `meta` read, so callers can invoke this on
    /// every command cheaply. Best-effort by contract — callers must not let its
    /// result fail the user's actual command.
    pub fn maybe_daily_backup(&self, backups_root: &Path) -> Result<Option<BackupReport>> {
        let (y, m, d) = time::today_ymd();
        let today = time::ymd_string((y, m, d));
        if self.meta_get("last_backup")?.as_deref() == Some(today.as_str()) {
            return Ok(None);
        }
        let today_compact = format!("{y:04}{m:02}{d:02}");
        if Self::backups_frozen(backups_root)?
            .iter()
            .any(|name| name.starts_with(&today_compact))
        {
            return Ok(None);
        }
        Ok(Some(self.backup_now(backups_root)?))
    }

    /// List snapshots under `backups_root`, newest first. Filesystem-only — no
    /// DB needed — so it works even when the live store is unopenable.
    pub fn list_backups(backups_root: &Path) -> Result<Vec<BackupEntry>> {
        let mut out = Vec::new();
        if !backups_root.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(backups_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            let integrity = if id.ends_with(SUSPECT_SUFFIX) {
                Integrity::Suspect
            } else {
                Integrity::Ok
            };
            let created_at = id.strip_suffix(SUSPECT_SUFFIX).unwrap_or(&id).to_string();
            out.push(BackupEntry {
                id,
                created_at,
                size_bytes: dir_size(&entry.path()),
                integrity,
            });
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    /// Verify a snapshot by `PRAGMA integrity_check` against its `haven.db`
    /// opened read-only.
    pub fn verify_backup(backups_root: &Path, id: &str) -> Result<Integrity> {
        let db = backups_root.join(id).join("haven.db");
        if !db.exists() {
            return Err(HavenError::NotFound(format!("backup {id:?}")));
        }
        Ok(if integrity_check_path(&db)? {
            Integrity::Ok
        } else {
            Integrity::Suspect
        })
    }

    /// The quarantined (`*-SUSPECT/`) snapshot dirs, if any. Non-empty means
    /// rotation is frozen and every command should warn until they are removed.
    pub fn backups_frozen(backups_root: &Path) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if !backups_root.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(backups_root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(SUSPECT_SUFFIX) && entry.file_type()?.is_dir() {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Restore a snapshot. Ordered so nothing on disk is destroyed until the
    /// restore is committed to succeed:
    ///   1. integrity-check the snapshot DB (refuse a corrupt source);
    ///   2. **stage** every content archive into a temp area — this fully
    ///      decompresses each `tar.gz`, so a truncated/corrupt member archive
    ///      fails the restore here, before any live data is touched;
    ///   3. safety-snapshot the current state (refuse if it can't be captured —
    ///      the restore would otherwise be unrecoverable);
    ///   4. refuse if another process holds the write lock (checked immediately
    ///      before the swap to minimize the window);
    ///   5. commit: atomically rename each staged tree into place, then swap the
    ///      DB file via a temp-write + rename.
    ///
    /// Operates on the files directly — it does **not** require a healthy live
    /// store, which is the whole point (you restore *because* the live DB is bad).
    ///
    /// Durability note: steps are crash-consistent at rename granularity but not
    /// `fsync`-barriered, so a power loss mid-commit can still leave a partial
    /// state recoverable from the safety snapshot. Restore is additive on the
    /// file side: a project created *after* the snapshot keeps its content tree.
    ///
    /// TODO(HV-13): once cloud sync lands, restore must reconcile sync metadata
    /// (`revision` / `sync_state`) against the remote — restored rows carry the
    /// snapshot's sync state, which the push pass would treat as already-synced.
    /// Until sync ships this is a plain copy-back; no blocking dependency.
    pub fn restore_backup(
        db_path: &Path,
        content_root: &Path,
        backups_root: &Path,
        id: &str,
    ) -> Result<RestoreReport> {
        let snap_dir = backups_root.join(id);
        let snap_db = snap_dir.join("haven.db");
        if !snap_db.exists() {
            return Err(HavenError::NotFound(format!("backup {id:?}")));
        }
        if !integrity_check_path(&snap_db)? {
            return Err(HavenError::Invalid(format!(
                "backup {id:?} fails an integrity check; refusing to restore from a corrupt snapshot"
            )));
        }

        // 2. Stage all content trees (validates every archive; no destruction yet).
        let staged = match stage_project_trees(&snap_dir, content_root) {
            Ok(staged) => staged,
            Err(e) => {
                cleanup_staging(content_root);
                return Err(e);
            }
        };

        // 3. Safety-snapshot current state, so the whole op is undoable. Refuse if
        //    we couldn't capture it rather than do an unrecoverable restore.
        let safety = safety_snapshot(db_path, content_root, backups_root);
        if safety.is_empty() {
            cleanup_staging(content_root);
            return Err(HavenError::Invalid(
                "could not capture a safety snapshot of the current state; \
                 refusing to restore (it would be unrecoverable)"
                    .into(),
            ));
        }

        // 4. Refuse if another process holds the write lock (just before the swap).
        if let Err(e) = require_unlocked(db_path) {
            cleanup_staging(content_root);
            return Err(e);
        }

        // 5. Commit. Content trees first (atomic rename per project), DB file last
        //    via temp-write + rename so a failed copy never leaves a half-written
        //    live DB. The snapshot DB is a standalone non-WAL file, so dropping
        //    the live DB's now-stale -wal/-shm sidecars is consistent.
        let files_restored = commit_staged_trees(staged, content_root)?;
        let db_tmp = sidecar(db_path, ".restore-tmp");
        std::fs::copy(&snap_db, &db_tmp)?;
        std::fs::rename(&db_tmp, db_path)?;
        let _ = std::fs::remove_file(sidecar(db_path, "-wal"));
        let _ = std::fs::remove_file(sidecar(db_path, "-shm"));

        Ok(RestoreReport {
            restored: id.to_string(),
            safety_snapshot: safety,
            files_restored,
        })
    }
}

// ---- snapshot internals ---------------------------------------------------

/// Online-backup `src` and archive `content_root`'s project trees into a new
/// `<UTC-ts>` dir under `backups_root`. Returns the dir name + its integrity. A
/// `quick_check` failure (before or after the copy) lands the snapshot in
/// `<ts>-SUSPECT/`. Touches neither the `last_backup` marker nor rotation —
/// callers layer those on.
fn snapshot_with_conn(
    src: &Connection,
    content_root: &Path,
    backups_root: &Path,
) -> Result<(String, Integrity)> {
    std::fs::create_dir_all(backups_root)?;
    let live_ok = quick_check_conn(src)?;
    let ts = time::utc_stamp(time::now_secs());
    let dir_name = if live_ok {
        ts.clone()
    } else {
        format!("{ts}{SUSPECT_SUFFIX}")
    };
    let dir = backups_root.join(&dir_name);
    std::fs::create_dir_all(&dir)?;
    backup_conn_to_file(src, &dir.join("haven.db"))?;
    archive_project_trees(content_root, &dir)?;

    if !live_ok {
        return Ok((dir_name, Integrity::Suspect));
    }
    // Re-check the COPY: a torn page that survived the page-by-page copy is
    // caught here, before the snapshot is trusted.
    if quick_check_path(&dir.join("haven.db"))? {
        Ok((dir_name, Integrity::Ok))
    } else {
        let suspect = format!("{ts}{SUSPECT_SUFFIX}");
        std::fs::rename(&dir, backups_root.join(&suspect))?;
        Ok((suspect, Integrity::Suspect))
    }
}

/// Online-backup the live connection into a fresh destination file. Uses the
/// SQLite backup API (consistent across main + WAL), never a raw copy.
fn backup_conn_to_file(src: &Connection, dest: &Path) -> Result<()> {
    let mut dst = Connection::open(dest)?;
    {
        let backup = rusqlite::backup::Backup::new(src, &mut dst)?;
        // Small pause between page chunks so a contended backup yields to a
        // concurrent writer instead of spinning (negligible for a local DB).
        backup.run_to_completion(100, Duration::from_millis(5), None)?;
    }
    // The page copy carries the live DB's WAL file-format flag, which would make
    // the snapshot spawn -wal/-shm sidecars whenever it is opened (e.g. by
    // verify). Convert to a rollback journal so each snapshot is a single,
    // self-contained haven.db.
    dst.pragma_update(None, "journal_mode", "DELETE")?;
    dst.close().map_err(|(_, e)| e)?;
    Ok(())
}

/// `tar.gz` each project's `items/` tree (+ `backlog.md`) found under
/// `content_root` into `dir/<key>.tar.gz`. Project dirs are discovered on disk
/// (robust even when the DB is unreadable); the reserved `backups/` dir and
/// non-project dirs are skipped.
fn archive_project_trees(content_root: &Path, dir: &Path) -> Result<()> {
    if !content_root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(content_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        // Skip the backups dir and any internal dot-dir (e.g. restore staging).
        if key == "backups" || key.starts_with('.') {
            continue;
        }
        let proj_dir = entry.path();
        let items_dir = proj_dir.join("items");
        let backlog = proj_dir.join("backlog.md");
        if !items_dir.is_dir() && !backlog.is_file() {
            continue; // not a project content dir
        }
        let file = std::fs::File::create(dir.join(format!("{key}.tar.gz")))?;
        let enc = GzEncoder::new(file, Compression::fast());
        let mut builder = tar::Builder::new(enc);
        if items_dir.is_dir() {
            builder.append_dir_all("items", &items_dir)?;
        }
        if backlog.is_file() {
            let mut f = std::fs::File::open(&backlog)?;
            builder.append_file("backlog.md", &mut f)?;
        }
        builder.into_inner()?.finish()?;
    }
    Ok(())
}

/// A content archive unpacked into the staging area, ready to be swapped into
/// its live location.
struct StagedTree {
    staged_items: Option<PathBuf>,
    items_target: PathBuf,
    staged_backlog: Option<PathBuf>,
    backlog_target: PathBuf,
}

/// `<content_root>/.restore-staging` — a temp area on the same filesystem as the
/// content trees (so the commit rename is atomic, not a cross-device copy).
fn staging_root(content_root: &Path) -> PathBuf {
    content_root.join(".restore-staging")
}

fn cleanup_staging(content_root: &Path) {
    let _ = std::fs::remove_dir_all(staging_root(content_root));
}

/// Decompress every `<key>.tar.gz` in `snap_dir` into the staging area. This
/// fully validates each archive (a truncated/corrupt member fails here) and
/// destroys **nothing** live — so the caller can still abort cleanly.
fn stage_project_trees(snap_dir: &Path, content_root: &Path) -> Result<Vec<StagedTree>> {
    let staging = staging_root(content_root);
    // Clear any leftovers from a prior aborted restore.
    let _ = std::fs::remove_dir_all(&staging);
    let mut staged = Vec::new();
    for entry in std::fs::read_dir(snap_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(key) = name.strip_suffix(".tar.gz") else {
            continue;
        };
        let stage_dir = staging.join(key);
        std::fs::create_dir_all(&stage_dir)?;
        let file = std::fs::File::open(entry.path())?;
        tar::Archive::new(GzDecoder::new(file)).unpack(&stage_dir)?;
        let proj_dir = content_root.join(key);
        staged.push(StagedTree {
            staged_items: Some(stage_dir.join("items")).filter(|p| p.is_dir()),
            items_target: proj_dir.join("items"),
            staged_backlog: Some(stage_dir.join("backlog.md")).filter(|p| p.is_file()),
            backlog_target: proj_dir.join("backlog.md"),
        });
    }
    Ok(staged)
}

/// Swap each staged tree into its live location with atomic renames (the live
/// `items/` is removed then the staged one renamed in; `backlog.md` is renamed
/// over). Returns the count of projects restored.
fn commit_staged_trees(staged: Vec<StagedTree>, content_root: &Path) -> Result<usize> {
    let mut count = 0;
    for t in &staged {
        if let Some(src) = &t.staged_items {
            if let Some(parent) = t.items_target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if t.items_target.exists() {
                std::fs::remove_dir_all(&t.items_target)?;
            }
            std::fs::rename(src, &t.items_target)?;
        }
        if let Some(src) = &t.staged_backlog {
            std::fs::rename(src, &t.backlog_target)?;
        }
        count += 1;
    }
    cleanup_staging(content_root);
    Ok(count)
}

/// Best-effort snapshot of pre-restore state. Tries a clean online snapshot of
/// the current DB; on failure (e.g. the live DB is already corrupt) falls back
/// to a raw forensic copy into `<ts>-SUSPECT/`. Returns the id, or empty if even
/// that failed (restore must not be blocked by an unsnapshot-able prior state).
fn safety_snapshot(db_path: &Path, content_root: &Path, backups_root: &Path) -> String {
    if let Ok(conn) = Connection::open(db_path) {
        if let Ok((id, _)) = snapshot_with_conn(&conn, content_root, backups_root) {
            return id;
        }
    }
    raw_forensic_copy(db_path, content_root, backups_root).unwrap_or_default()
}

/// Raw copy of the (presumed-corrupt) live DB + content trees into a quarantined
/// `<ts>-SUSPECT/` dir — a forensic record when the online backup can't read it.
fn raw_forensic_copy(db_path: &Path, content_root: &Path, backups_root: &Path) -> Result<String> {
    let id = format!("{}{SUSPECT_SUFFIX}", time::utc_stamp(time::now_secs()));
    let dir = backups_root.join(&id);
    std::fs::create_dir_all(&dir)?;
    if db_path.exists() {
        std::fs::copy(db_path, dir.join("haven.db"))?;
    }
    archive_project_trees(content_root, &dir)?;
    Ok(id)
}

// ---- integrity + locking --------------------------------------------------

/// `PRAGMA quick_check` on a live connection. A corruption error from the
/// pragma itself is treated as a failed check (the signal we want), not an
/// error to bubble.
fn quick_check_conn(conn: &Connection) -> Result<bool> {
    pragma_is_ok(conn, "quick_check")
}

fn quick_check_path(db: &Path) -> Result<bool> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    pragma_is_ok(&conn, "quick_check")
}

fn integrity_check_path(db: &Path) -> Result<bool> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    pragma_is_ok(&conn, "integrity_check")
}

/// Run an integrity pragma and report whether the result is a single `ok` row.
/// A `SQLITE_CORRUPT` / `SQLITE_NOTADB` failure of the pragma means the DB is
/// bad — reported as `false`, not propagated.
fn pragma_is_ok(conn: &Connection, pragma: &str) -> Result<bool> {
    let mut rows: Vec<String> = Vec::new();
    let res = conn.pragma_query(None, pragma, |r| {
        rows.push(r.get::<_, String>(0)?);
        Ok(())
    });
    match res {
        Ok(()) => Ok(rows.len() == 1 && rows[0].eq_ignore_ascii_case("ok")),
        Err(rusqlite::Error::SqliteFailure(f, _))
            if f.code == rusqlite::ErrorCode::DatabaseCorrupt
                || f.code == rusqlite::ErrorCode::NotADatabase =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// Refuse if another process holds the write lock. Probes with a zero-timeout
/// `BEGIN EXCLUSIVE` (the global busy_timeout is 5s, which we override on this
/// throwaway connection so we fail fast rather than waiting). Only `BUSY`/`LOCKED`
/// means a concurrent writer — refuse. Any other outcome (success, or a corrupt
/// DB throwing an unpredictable code, since we restore *because* the DB is bad)
/// means no one holds the lock, so proceed; a genuinely broken filesystem is then
/// surfaced loudly by the atomic copy/rename that follows, not silently ignored.
fn require_unlocked(db_path: &Path) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(Duration::ZERO)?;
    match conn.execute_batch("BEGIN EXCLUSIVE") {
        Err(e) if is_busy(&e) => Err(HavenError::Conflict(
            "another process holds the Haven write lock; close other haven sessions and retry"
                .into(),
        )),
        _ => {
            let _ = conn.execute_batch("COMMIT");
            Ok(())
        }
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::DatabaseBusy
                || f.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

// ---- rotation -------------------------------------------------------------

/// Prune snapshots under `backups_root` to [`KEEP_DAILY`] + [`KEEP_WEEKLY`].
/// A no-op while any `*-SUSPECT/` exists (freeze): a corruption must not age out
/// the last good copy.
fn rotate(backups_root: &Path) -> Result<()> {
    if !Store::backups_frozen(backups_root)?.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = std::fs::read_dir(backups_root)?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for name in plan_rotation(&names, KEEP_DAILY, KEEP_WEEKLY) {
        std::fs::remove_dir_all(backups_root.join(name))?;
    }
    Ok(())
}

/// Pure rotation policy over snapshot dir names: keep the newest snapshot of
/// each of the `keep_daily` most-recent calendar days and of each of the
/// `keep_weekly` most-recent ISO weeks; return the names to delete. Quarantined
/// (`-SUSPECT`) and unparseable names are never deleted. Time-free so it is
/// deterministically testable (the codebase has no injectable clock).
fn plan_rotation(dirs: &[String], keep_daily: usize, keep_weekly: usize) -> Vec<String> {
    // Parse the parseable, non-suspect dirs; sort newest-first (lexical ==
    // chronological for the YYYYMMDDTHHMMSSZ stamp).
    let mut snaps: Vec<(&String, (i64, u32, u32))> = dirs
        .iter()
        .filter(|n| !n.ends_with(SUSPECT_SUFFIX))
        .filter_map(|n| parse_stamp(n).map(|ymd| (n, ymd)))
        .collect();
    snaps.sort_by(|a, b| b.0.cmp(a.0));

    let mut keep: HashSet<&String> = HashSet::new();

    let mut seen_days: Vec<(i64, u32, u32)> = Vec::new();
    for (name, ymd) in &snaps {
        if seen_days.contains(ymd) {
            continue;
        }
        seen_days.push(*ymd);
        if seen_days.len() <= keep_daily {
            keep.insert(*name);
        }
    }

    let mut seen_weeks: Vec<(i64, u32)> = Vec::new();
    for (name, ymd) in &snaps {
        let w = time::iso_week(ymd.0, ymd.1, ymd.2);
        if seen_weeks.contains(&w) {
            continue;
        }
        seen_weeks.push(w);
        if seen_weeks.len() <= keep_weekly {
            keep.insert(*name);
        }
    }

    snaps
        .iter()
        .filter(|(n, _)| !keep.contains(n))
        .map(|(n, _)| (*n).clone())
        .collect()
}

/// Parse a `YYYYMMDDTHHMMSSZ` snapshot dir name to `(year, month, day)`.
fn parse_stamp(name: &str) -> Option<(i64, u32, u32)> {
    let b = name.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    if !b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit) {
        return None;
    }
    let y: i64 = name[0..4].parse().ok()?;
    let m: u32 = name[4..6].parse().ok()?;
    let d: u32 = name[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Reject impossible calendar days (e.g. Feb 30) so rotation day/week buckets
    // aren't skewed — a round-trip through the date helpers normalizes overflow.
    if time::civil_from_days(time::days_from_civil(y, m, d)) != (y, m, d) {
        return None;
    }
    Some((y, m, d))
}

// ---- reporting helpers ----------------------------------------------------

fn build_report(backups_root: &Path, id: &str, integrity: Integrity) -> Result<BackupReport> {
    let dir = backups_root.join(id);
    let db_bytes = std::fs::metadata(dir.join("haven.db"))
        .map(|m| m.len())
        .unwrap_or(0);
    let mut projects = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(key) = name.strip_suffix(".tar.gz") {
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            projects.push(ProjectArchive {
                key: key.to_string(),
                archive: name,
                bytes,
            });
        }
    }
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(BackupReport {
        id: id.to_string(),
        db_bytes,
        projects,
        integrity,
        quarantined: integrity == Integrity::Suspect,
    })
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => total += dir_size(&entry.path()),
                Ok(_) => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => {}
            }
        }
    }
    total
}

/// `haven.db` + `"-wal"` -> `haven.db-wal` (the SQLite sidecar naming).
fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeType;
    use crate::store::NewItem;

    fn stamp(y: i64, m: u32, d: u32) -> String {
        time::utc_stamp(time::days_from_civil(y, m, d) * 86_400 + 12 * 3600)
    }

    #[test]
    fn plan_rotation_keeps_7_daily_and_4_weekly() {
        // 40 consecutive daily snapshots ending 2026-02-09 (a Monday).
        let start = time::days_from_civil(2026, 1, 1);
        let end = time::days_from_civil(2026, 2, 9);
        let real: Vec<String> = (start..=end)
            .map(|z| time::utc_stamp(z * 86_400 + 12 * 3600))
            .collect();
        assert_eq!(real.len(), 40);

        let mut names = real.clone();
        names.push("20260210T120000Z-SUSPECT".into()); // quarantined: never pruned
        names.push("not-a-timestamp".into()); // unparseable: never pruned

        let deleted: HashSet<String> = plan_rotation(&names, 7, 4).into_iter().collect();
        assert!(!deleted.contains("20260210T120000Z-SUSPECT"));
        assert!(!deleted.contains("not-a-timestamp"));

        // Daily keeps Feb 3-9; weekly reach-back adds Feb 1 (wk5) + Jan 25 (wk4).
        let kept: Vec<&String> = real.iter().filter(|n| !deleted.contains(*n)).collect();
        assert_eq!(kept.len(), 9, "7 daily + 2 distinct older weekly");
        for (m, d) in [
            (2, 9),
            (2, 8),
            (2, 7),
            (2, 6),
            (2, 5),
            (2, 4),
            (2, 3),
            (2, 1),
            (1, 25),
        ] {
            assert!(!deleted.contains(&stamp(2026, m, d)), "should keep {m}-{d}");
        }
        for (m, d) in [(2, 2), (1, 24), (1, 1)] {
            assert!(
                deleted.contains(&stamp(2026, m, d)),
                "should delete {m}-{d}"
            );
        }
    }

    #[test]
    fn corruption_round_trip_restores_identical_graph() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("haven.db");
        let backups = root.join("backups");

        // Seed: a small graph (items + a decomposition edge) and an items/ file.
        {
            let s = Store::open(&db_path, root).unwrap();
            s.add_project("haven", Some("HV"), "Haven", None).unwrap();
            s.use_project("haven").unwrap();
            let _root_item = s
                .add_item(
                    None,
                    NewItem {
                        title: "Parent".into(),
                        node_type: Some(NodeType::Anchor),
                        ..Default::default()
                    },
                )
                .unwrap();
            s.add_item(
                None,
                NewItem {
                    title: "Child".into(),
                    parent: Some("HV-1".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            // A content file under the items/ tree, to prove the tar round-trips.
            let item_dir = root.join("haven/items/HV-1");
            std::fs::create_dir_all(&item_dir).unwrap();
            std::fs::write(item_dir.join("spec.md"), b"the spec body").unwrap();

            let report = s.backup_now(&backups).unwrap();
            assert_eq!(report.integrity, Integrity::Ok);
            assert!(backups.join(&report.id).join("haven.db").exists());
            assert!(backups.join(&report.id).join("haven.tar.gz").exists());
        }

        // Capture the canonical graph dump, then close the store (checkpoints WAL).
        let before = {
            let s = Store::open(&db_path, root).unwrap();
            serde_json::to_string(&s.project_graph(Some("haven"), true).unwrap()).unwrap()
        };
        let snapshot_id = Store::list_backups(&backups).unwrap()[0].id.clone();

        // Corrupt: truncate the live DB mid-file, and delete the content file.
        let len = std::fs::metadata(&db_path).unwrap().len();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .unwrap();
        f.set_len(len / 2).unwrap();
        drop(f);
        std::fs::remove_file(root.join("haven/items/HV-1/spec.md")).unwrap();

        // Restore.
        let restore = Store::restore_backup(&db_path, root, &backups, &snapshot_id).unwrap();
        assert!(!restore.safety_snapshot.is_empty());
        assert_eq!(restore.files_restored, 1);

        // The graph dump is byte-identical and the content file is back.
        let after = {
            let s = Store::open(&db_path, root).unwrap();
            serde_json::to_string(&s.project_graph(Some("haven"), true).unwrap()).unwrap()
        };
        assert_eq!(before, after, "restored graph must be byte-identical");
        assert_eq!(
            std::fs::read(root.join("haven/items/HV-1/spec.md")).unwrap(),
            b"the spec body"
        );
    }

    #[test]
    fn restore_aborts_without_destroying_content_when_an_archive_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("haven.db");
        let backups = root.join("backups");

        {
            let s = Store::open(&db_path, root).unwrap();
            s.add_project("haven", Some("HV"), "Haven", None).unwrap();
            s.use_project("haven").unwrap();
            s.add_item(
                None,
                NewItem {
                    title: "Item".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            let item_dir = root.join("haven/items/HV-1");
            std::fs::create_dir_all(&item_dir).unwrap();
            std::fs::write(item_dir.join("spec.md"), b"live content").unwrap();
            s.backup_now(&backups).unwrap();
        }

        let id = Store::list_backups(&backups).unwrap()[0].id.clone();
        // Corrupt the content archive (the DB stays valid, so the integrity gate
        // passes and we reach the staging step).
        std::fs::write(backups.join(&id).join("haven.tar.gz"), b"not a gzip").unwrap();

        // Restore must fail at staging — before any live data is touched.
        let err = Store::restore_backup(&db_path, root, &backups, &id).unwrap_err();
        assert!(matches!(err, HavenError::Io(_)), "got {err:?}");

        // The live content tree is intact, and no staging litter was left behind.
        assert_eq!(
            std::fs::read(root.join("haven/items/HV-1/spec.md")).unwrap(),
            b"live content"
        );
        assert!(!staging_root(root).exists());
        // The store still opens and reads.
        let s = Store::open(&db_path, root).unwrap();
        assert_eq!(s.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn suspect_freezes_rotation_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();

        // 10 good daily snapshots (well past the 7+4 budget) + a quarantine dir.
        let start = time::days_from_civil(2026, 3, 1);
        for z in start..start + 10 {
            let id = time::utc_stamp(z * 86_400 + 12 * 3600);
            let d = backups.join(&id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("haven.db"), b"x").unwrap();
        }
        let suspect = backups.join("20260311T120000Z-SUSPECT");
        std::fs::create_dir_all(&suspect).unwrap();

        // Frozen: rotate is a no-op, nothing pruned.
        assert_eq!(Store::backups_frozen(&backups).unwrap().len(), 1);
        rotate(&backups).unwrap();
        let after_frozen = std::fs::read_dir(&backups).unwrap().count();
        assert_eq!(after_frozen, 11, "freeze must keep every good snapshot");

        // Clear the quarantine: rotation resumes and prunes to the budget.
        std::fs::remove_dir_all(&suspect).unwrap();
        assert!(Store::backups_frozen(&backups).unwrap().is_empty());
        rotate(&backups).unwrap();
        let kept = std::fs::read_dir(&backups).unwrap().count();
        assert!(
            kept < 10,
            "rotation should prune once unfrozen (kept {kept})"
        );
    }

    #[test]
    fn quick_check_flags_a_corrupt_database() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("garbage.db");
        std::fs::write(&junk, b"this is definitely not a sqlite database").unwrap();
        assert!(!quick_check_path(&junk).unwrap(), "garbage must fail check");
    }
}
