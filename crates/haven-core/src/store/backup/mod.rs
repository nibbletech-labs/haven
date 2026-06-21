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
use serde::{Deserialize, Serialize};

use super::Store;
use crate::error::{HavenError, Result};
use crate::time;

mod lock;
mod manifest;
mod objects;

/// Quarantine marker suffix on a snapshot directory name.
const SUSPECT_SUFFIX: &str = "-SUSPECT";
/// Where per-snapshot manifests live (content-addressed format).
const MANIFESTS_DIR: &str = "manifests";
/// Names under `backups/` that are store internals, not snapshots — skipped by
/// every `read_dir(backups_root)` scan so the two formats can't trample each other.
const RESERVED_BACKUP_NAMES: &[&str] = &[
    "objects",
    "manifests",
    ".lock",
    ".quarantine",
    ".restore-staging",
];
/// Retention: keep the newest snapshot of each of the 7 most-recent days …
const KEEP_DAILY: usize = 7;
/// … and the newest snapshot of each of the 4 most-recent ISO weeks.
const KEEP_WEEKLY: usize = 4;

/// Integrity verdict for a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The snapshot id — the manifest stem (`<UTC-ts>` or `<UTC-ts>-SUSPECT`).
    pub id: String,
    /// On-disk (compressed) size of the snapshot's DB object.
    pub db_bytes: u64,
    pub projects: Vec<ProjectArchive>,
    pub integrity: Integrity,
    pub quarantined: bool,
    /// Objects newly written by this snapshot (0 on a no-change re-snapshot).
    pub new_objects: usize,
    /// Total objects this snapshot references (DB object + every unique file).
    pub total_objects: usize,
}

/// The on-disk format of a listed snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupFormat {
    /// HV-90 content-addressed: a `manifests/<ts>.json` + shared `objects/`.
    Manifest,
    /// HV-41 legacy: a self-contained `<ts>/` dir (`haven.db` + `<key>.tar.gz`).
    Legacy,
}

/// A snapshot as listed by `haven backup list`.
#[derive(Debug, Clone, Serialize)]
pub struct BackupEntry {
    pub id: String,
    /// The UTC timestamp portion of the id (the `-SUSPECT` suffix stripped).
    pub created_at: String,
    /// Logical size: summed on-disk bytes of the objects this snapshot references
    /// (objects dedup across snapshots, so this is per-snapshot, not physical).
    pub size_bytes: u64,
    pub integrity: Integrity,
    pub format: BackupFormat,
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
        // Serialize snapshot vs GC across processes (CLI + the long-lived MCP
        // server both fire the daily trigger). An explicit `backup now` surfaces
        // contention; the opportunistic path below skips instead.
        let _lock = match lock::BackupLock::try_acquire(backups_root)? {
            Some(l) => l,
            None => {
                return Err(HavenError::Conflict(
                    "another haven process is taking a backup; retry shortly".into(),
                ))
            }
        };
        self.backup_now_locked(backups_root)
    }

    /// The snapshot body, assuming the backup lock is held. Shared by the explicit
    /// `backup_now` and the opportunistic `maybe_daily_backup`.
    fn backup_now_locked(&self, backups_root: &Path) -> Result<BackupReport> {
        let report = snapshot_to_objects(&self.conn, self.content_root(), backups_root)?;
        if report.integrity == Integrity::Ok {
            self.meta_set("last_backup", &time::ymd_string(time::today_ymd()))?;
            rotate(backups_root)?;
        }
        Ok(report)
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
        // Due: acquire the lock lazily. If another process holds it, it is already
        // handling today's backup — skip rather than fail the caller's command.
        let Some(_lock) = lock::BackupLock::try_acquire(backups_root)? else {
            return Ok(None);
        };
        // Re-check the marker under the lock (another process may have just run).
        if self.meta_get("last_backup")?.as_deref() == Some(today.as_str()) {
            return Ok(None);
        }
        Ok(Some(self.backup_now_locked(backups_root)?))
    }

    /// List snapshots under `backups_root`, newest first. Filesystem-only — no
    /// DB needed — so it works even when the live store is unopenable.
    pub fn list_backups(backups_root: &Path) -> Result<Vec<BackupEntry>> {
        let mut out = Vec::new();
        if !backups_root.exists() {
            return Ok(out);
        }
        // New (content-addressed) format: manifests/<ts>[-SUSPECT].json.
        let manifests_dir = backups_root.join(MANIFESTS_DIR);
        let objects_root = backups_root.join(objects::OBJECTS_DIR);
        if manifests_dir.is_dir() {
            for entry in std::fs::read_dir(&manifests_dir)? {
                let entry = entry?;
                let fname = entry.file_name().to_string_lossy().into_owned();
                let Some(id) = fname.strip_suffix(".json") else {
                    continue; // not a manifest (e.g. a .tmp-* in flight)
                };
                if id.starts_with('.') {
                    continue;
                }
                let integrity = suspect_of(id);
                let size_bytes = manifest::read(&entry.path())
                    .map(|m| manifest_object_bytes(&objects_root, &m))
                    .unwrap_or(0);
                out.push(BackupEntry {
                    id: id.to_string(),
                    created_at: strip_suspect(id).to_string(),
                    size_bytes,
                    integrity,
                    format: BackupFormat::Manifest,
                });
            }
        }
        // Legacy (HV-41) format: self-contained <ts>[-SUSPECT]/ dirs — coexist and
        // age out. Reserved internals (objects/, manifests/, .lock) are skipped.
        for entry in std::fs::read_dir(backups_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if RESERVED_BACKUP_NAMES.contains(&id.as_str()) || id.starts_with('.') {
                continue;
            }
            if !entry.path().join("haven.db").exists() {
                continue; // not a legacy snapshot dir
            }
            out.push(BackupEntry {
                created_at: strip_suspect(&id).to_string(),
                integrity: suspect_of(&id),
                size_bytes: dir_size(&entry.path()),
                format: BackupFormat::Legacy,
                id,
            });
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    /// Verify a snapshot. New format: re-hash every referenced object (decompress
    /// → sha256 → compare name) and quarantine any that is missing/mismatching,
    /// marking the snapshot SUSPECT (which freezes GC). Legacy format: a
    /// `PRAGMA integrity_check` of the snapshot's `haven.db`.
    pub fn verify_backup(backups_root: &Path, id: &str) -> Result<Integrity> {
        let manifest_path = backups_root.join(MANIFESTS_DIR).join(format!("{id}.json"));
        if manifest_path.exists() {
            return verify_manifest(backups_root, &manifest_path);
        }
        let legacy_db = backups_root.join(id).join("haven.db");
        if legacy_db.exists() && !RESERVED_BACKUP_NAMES.contains(&id) {
            return Ok(if integrity_check_path(&legacy_db)? {
                Integrity::Ok
            } else {
                Integrity::Suspect
            });
        }
        Err(HavenError::NotFound(format!("backup {id:?}")))
    }

    /// The quarantined snapshots, if any — the union of legacy `*-SUSPECT/` dirs
    /// and new `manifests/*-SUSPECT.json` manifests (returned as bare `<ts>` stems
    /// so a `starts_with(YYYYMMDD)` debounce works). Non-empty means rotation AND
    /// object GC are frozen and every command should warn until they are cleared.
    pub fn backups_frozen(backups_root: &Path) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if !backups_root.exists() {
            return Ok(out);
        }
        // Legacy: <ts>-SUSPECT/ dirs.
        for entry in std::fs::read_dir(backups_root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(SUSPECT_SUFFIX) && entry.file_type()?.is_dir() {
                out.push(name);
            }
        }
        // New: manifests/<ts>-SUSPECT.json.
        let manifests_dir = backups_root.join(MANIFESTS_DIR);
        if manifests_dir.is_dir() {
            for entry in std::fs::read_dir(&manifests_dir)? {
                let entry = entry?;
                let fname = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = fname.strip_suffix(".json") {
                    if stem.ends_with(SUSPECT_SUFFIX) {
                        out.push(stem.to_string());
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Remove a quarantined (`*-SUSPECT`) snapshot — the new-format manifest or the
    /// legacy dir — which un-freezes rotation and object GC. Filesystem-only.
    pub fn clear_quarantine(backups_root: &Path, id: &str) -> Result<()> {
        if !id.ends_with(SUSPECT_SUFFIX) {
            return Err(HavenError::Invalid(format!(
                "{id:?} is not a quarantined snapshot (expected a `*-SUSPECT` id)"
            )));
        }
        let manifest = backups_root.join(MANIFESTS_DIR).join(format!("{id}.json"));
        if manifest.exists() {
            std::fs::remove_file(manifest)?;
            return Ok(());
        }
        let dir = backups_root.join(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(dir)?;
            return Ok(());
        }
        Err(HavenError::NotFound(format!("quarantined backup {id:?}")))
    }

    /// Restore a snapshot — new content-addressed (`manifests/<id>.json`) or legacy
    /// HV-41 (`<id>/` dir). Ordered so nothing on disk is destroyed until the
    /// restore is committed to succeed: validate the source, **stage everything**
    /// (materialize from objects / untar) into a same-filesystem temp area
    /// re-hashing as it lands — a missing/mis-hashing object or corrupt archive
    /// fails here, before any live data is touched — then safety-snapshot, refuse
    /// if another process holds the write lock, and only then atomically rename
    /// each staged tree into place and swap the DB via temp-write + rename.
    ///
    /// Operates on the files directly — it does **not** require a healthy live
    /// store, which is the whole point (you restore *because* the live DB is bad).
    /// Holds the backup lock for the whole op so a concurrent GC cannot reap an
    /// object being materialized. Additive on the file side: a project created
    /// *after* the snapshot keeps its content tree.
    ///
    /// Durability note: steps are crash-consistent at rename granularity but not
    /// `fsync`-barriered, so a power loss mid-commit can still leave a partial
    /// state recoverable from the safety snapshot.
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
        // Hold the backup lock for the whole restore so a concurrent GC can't reap
        // an object we are materializing from.
        let _lock = lock::BackupLock::try_acquire(backups_root)?.ok_or_else(|| {
            HavenError::Conflict(
                "another haven process is using the backup store; retry shortly".into(),
            )
        })?;
        let manifest_path = backups_root.join(MANIFESTS_DIR).join(format!("{id}.json"));
        if manifest_path.exists() {
            return restore_from_manifest(db_path, content_root, backups_root, id, &manifest_path);
        }
        // Legacy (HV-41) self-contained <ts>/ dir.
        if !RESERVED_BACKUP_NAMES.contains(&id) && backups_root.join(id).join("haven.db").exists() {
            return restore_legacy(db_path, content_root, backups_root, id);
        }
        Err(HavenError::NotFound(format!("backup {id:?}")))
    }
}

// ---- restore internals ----------------------------------------------------

/// Restore from a content-addressed manifest (see [`Store::restore_backup`] for
/// the ordering contract). Caller holds the backup lock.
fn restore_from_manifest(
    db_path: &Path,
    content_root: &Path,
    backups_root: &Path,
    id: &str,
    manifest_path: &Path,
) -> Result<RestoreReport> {
    let objects_root = backups_root.join(objects::OBJECTS_DIR);
    // 1. Validate source: parse + self-validate the manifest (refuse a torn one).
    let m = manifest::read(manifest_path)?;

    // 2. Stage everything (destroy nothing): materialize the DB object (+ integrity
    //    check) and every file object (re-hashed). Any miss/mismatch fails here.
    let staged_db = match materialize_and_stage(&m, &objects_root, content_root) {
        Ok(db) => db,
        Err(e) => {
            cleanup_staging(content_root);
            return Err(e);
        }
    };
    let staged = build_staged_trees(&m, content_root);

    // 3. Safety-snapshot current state (refuse if it can't be captured).
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

    // 5. Commit. Copy the staged DB out to a temp beside the live DB FIRST (the
    //    staged DB lives under .restore-staging, which committing the trees wipes),
    //    commit the trees, then swap the DB in via an atomic rename (last).
    let db_tmp = sidecar(db_path, ".restore-tmp");
    std::fs::copy(&staged_db, &db_tmp)?;
    let files_restored = commit_staged_trees(staged, content_root)?;
    std::fs::rename(&db_tmp, db_path)?;
    let _ = std::fs::remove_file(sidecar(db_path, "-wal"));
    let _ = std::fs::remove_file(sidecar(db_path, "-shm"));
    cleanup_staging(content_root);

    Ok(RestoreReport {
        restored: id.to_string(),
        safety_snapshot: safety,
        files_restored,
    })
}

/// Materialize a manifest's objects into `.restore-staging`, re-hashing each file
/// object against its manifest entry and integrity-checking the DB object.
/// Destroys nothing live; any missing/mis-hashing object errors here. Returns the
/// staged DB file path.
fn materialize_and_stage(
    m: &manifest::Manifest,
    objects_root: &Path,
    content_root: &Path,
) -> Result<PathBuf> {
    let staging = staging_root(content_root);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    // DB object → a staging file (dot-prefixed so it is never mistaken for a
    // project key), then integrity-check it before trusting it.
    let staged_db = staging.join(".haven.db.restore");
    objects::read_object_to(objects_root, &m.db.object, &staged_db)?;
    if !integrity_check_path(&staged_db)? {
        return Err(HavenError::Invalid(format!(
            "backup {} DB object fails an integrity check; refusing to restore",
            m.ts
        )));
    }

    for p in &m.projects {
        let proj_stage = staging.join(&p.key);
        for e in &p.paths {
            match e {
                manifest::PathEntry::File { path, hash, .. } => {
                    let dest = proj_stage.join(path);
                    objects::read_object_to(objects_root, hash, &dest)?;
                    let (got, _) = objects::hash_file(&dest)?;
                    if &got != hash {
                        return Err(HavenError::Invalid(format!(
                            "backup object {hash} materialized to a mismatching hash; \
                             refusing to restore from a corrupt snapshot"
                        )));
                    }
                }
                manifest::PathEntry::Symlink { path, target } => {
                    let dest = proj_stage.join(path);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(target, &dest)?;
                    #[cfg(not(unix))]
                    let _ = target;
                }
            }
        }
    }
    Ok(staged_db)
}

/// Build the staged-tree handles for [`commit_staged_trees`] from a manifest:
/// each project's materialized `items/` tree + `backlog.md` under staging.
fn build_staged_trees(m: &manifest::Manifest, content_root: &Path) -> Vec<StagedTree> {
    let staging = staging_root(content_root);
    m.projects
        .iter()
        .map(|p| {
            let stage_dir = staging.join(&p.key);
            let proj_dir = content_root.join(&p.key);
            StagedTree {
                staged_items: Some(stage_dir.join("items")).filter(|x| x.is_dir()),
                items_target: proj_dir.join("items"),
                staged_backlog: Some(stage_dir.join("backlog.md")).filter(|x| x.is_file()),
                backlog_target: proj_dir.join("backlog.md"),
            }
        })
        .collect()
}

/// Restore a legacy HV-41 `<id>/` snapshot (self-contained `haven.db` +
/// `<key>.tar.gz`). Same staged-atomic ordering; kept so backups taken before the
/// content-addressed format still restore during the transition. Caller holds the
/// backup lock.
fn restore_legacy(
    db_path: &Path,
    content_root: &Path,
    backups_root: &Path,
    id: &str,
) -> Result<RestoreReport> {
    let snap_dir = backups_root.join(id);
    let snap_db = snap_dir.join("haven.db");
    if !integrity_check_path(&snap_db)? {
        return Err(HavenError::Invalid(format!(
            "backup {id:?} fails an integrity check; refusing to restore from a corrupt snapshot"
        )));
    }
    let staged = match stage_project_trees(&snap_dir, content_root) {
        Ok(staged) => staged,
        Err(e) => {
            cleanup_staging(content_root);
            return Err(e);
        }
    };
    let safety = safety_snapshot(db_path, content_root, backups_root);
    if safety.is_empty() {
        cleanup_staging(content_root);
        return Err(HavenError::Invalid(
            "could not capture a safety snapshot of the current state; \
             refusing to restore (it would be unrecoverable)"
                .into(),
        ));
    }
    if let Err(e) = require_unlocked(db_path) {
        cleanup_staging(content_root);
        return Err(e);
    }
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

// ---- verify ---------------------------------------------------------------

/// Verify a content-addressed snapshot by re-hashing every referenced object
/// (decompress → sha256 → compare to its name). A missing/mismatching object is a
/// poisoned cache entry: quarantine it (so no future write-skip trusts the name)
/// and re-mark the manifest SUSPECT — which freezes both rotation and object GC so
/// the corruption can't age out a clean copy. Returns the resulting verdict.
fn verify_manifest(backups_root: &Path, manifest_path: &Path) -> Result<Integrity> {
    let objects_root = backups_root.join(objects::OBJECTS_DIR);
    let manifests_dir = backups_root.join(MANIFESTS_DIR);
    let m = match manifest::read(manifest_path) {
        Ok(m) => m,
        Err(_) => return Ok(Integrity::Suspect), // torn/unreadable manifest
    };
    if m.integrity == Integrity::Suspect {
        return Ok(Integrity::Suspect);
    }
    let mut bad = false;
    for hash in m.referenced_objects() {
        if !objects::verify_object(&objects_root, &hash)? {
            let _ = objects::quarantine_object(&objects_root, &hash);
            bad = true;
        }
    }
    if bad {
        // Re-mark SUSPECT (body + `-SUSPECT.json` name) and drop the old OK file.
        let suspect = manifest::Manifest::new(
            m.ts.clone(),
            Integrity::Suspect,
            m.db.clone(),
            m.projects.clone(),
        );
        let _ = manifest::write(&manifests_dir, &suspect);
        let _ = std::fs::remove_file(manifest_path);
        return Ok(Integrity::Suspect);
    }
    Ok(Integrity::Ok)
}

// ---- snapshot internals (content-addressed) -------------------------------

/// Take a content-addressed snapshot from a live connection: `quick_check` the
/// DB, online-backup it to a consolidated single file and store *that* as an
/// object, walk each project tree writing one object per unique file, then write
/// the manifest. A `quick_check` failure (live DB, or the copied DB) marks the
/// snapshot `Suspect` (a `-SUSPECT` manifest). Lock-free: callers (`backup_now`,
/// restore's safety snapshot) hold the backup lock. Touches neither the
/// `last_backup` marker nor rotation — callers layer those on.
fn snapshot_to_objects(
    src: &Connection,
    content_root: &Path,
    backups_root: &Path,
) -> Result<BackupReport> {
    let objects_root = backups_root.join(objects::OBJECTS_DIR);
    let manifests_dir = backups_root.join(MANIFESTS_DIR);
    std::fs::create_dir_all(&objects_root)?;
    std::fs::create_dir_all(&manifests_dir)?;

    let live_ok = quick_check_conn(src)?;
    let mut secs = time::now_secs();
    let ts = time::utc_stamp(secs);

    // DB → consolidated non-WAL single file → content-addressed object (never a
    // raw WAL copy). The temp lives under objects/ so a crash leaves it for
    // `temp_sweep`, and its `quick_check` re-validates the copy.
    let tmp_db = objects_root.join(format!(".tmp-db-{}-{ts}", std::process::id()));
    backup_conn_to_file(src, &tmp_db)?;
    let copy_ok = quick_check_path(&tmp_db)?;
    let (db_sha, _db_size, db_new) = objects::write_object(&objects_root, &tmp_db)?;
    let _ = std::fs::remove_file(&tmp_db);

    let integrity = if live_ok && copy_ok {
        Integrity::Ok
    } else {
        Integrity::Suspect
    };

    let mut projects = Vec::new();
    let mut new_objects = usize::from(db_new);
    let mut total_objects = 1usize; // the DB object
    for key in discover_project_keys(content_root)? {
        let (paths, n_new, n_total) = walk_project_tree(&content_root.join(&key), &objects_root)?;
        new_objects += n_new;
        total_objects += n_total;
        if !paths.is_empty() {
            projects.push(manifest::ProjectEntry { key, paths });
        }
    }

    let mut m = manifest::Manifest::new(
        ts,
        integrity,
        manifest::DbHandle {
            repr: "gz-full".into(),
            object: db_sha,
        },
        projects,
    );
    // Same-second collision (rapid `backup now`, or a safety snapshot beside a
    // daily): bump the stamp by a second and retry. The lock already prevents
    // cross-process races; this is the within-process belt.
    let name = loop {
        match manifest::write(&manifests_dir, &m) {
            Ok(name) => break name,
            Err(HavenError::Conflict(_)) => {
                secs += 1;
                m.restamp(time::utc_stamp(secs));
            }
            Err(e) => return Err(e),
        }
    };
    let id = name.strip_suffix(".json").unwrap_or(&name).to_string();
    Ok(build_report_from_manifest(
        backups_root,
        &id,
        &m,
        new_objects,
        total_objects,
    ))
}

/// Discover project content dirs on disk under `content_root` (robust even when
/// the DB is unreadable): top-level dirs holding an `items/` tree or a
/// `backlog.md`, excluding the reserved `backups` dir and any dot-dir. The walk
/// is rooted here (never at `content_root` itself recursively) so the object
/// store under `backups/` can never be hashed into itself.
fn discover_project_keys(content_root: &Path) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    if !content_root.exists() {
        return Ok(keys);
    }
    for entry in std::fs::read_dir(content_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        if key == "backups" || key.starts_with('.') {
            continue;
        }
        let proj_dir = entry.path();
        if proj_dir.join("items").is_dir() || proj_dir.join("backlog.md").is_file() {
            keys.push(key);
        }
    }
    keys.sort();
    Ok(keys)
}

/// Walk one project's content (`items/` tree + `backlog.md`), writing one object
/// per unique file and returning the manifest path entries (relative to the
/// project dir) plus `(new, total)` object counts. Symlinks are recorded, never
/// followed (no tree-escape / cycles); editor/OS junk is filtered.
fn walk_project_tree(
    proj_dir: &Path,
    objects_root: &Path,
) -> Result<(Vec<manifest::PathEntry>, usize, usize)> {
    let mut paths = Vec::new();
    let mut counts = (0usize, 0usize); // (new, total)
    let items = proj_dir.join("items");
    if items.is_dir() {
        walk_into(&items, proj_dir, objects_root, &mut paths, &mut counts)?;
    }
    let backlog = proj_dir.join("backlog.md");
    if backlog.is_file() {
        ingest_file(&backlog, proj_dir, objects_root, &mut paths, &mut counts)?;
    }
    paths.sort_by(|a, b| entry_path(a).cmp(entry_path(b)));
    Ok((paths, counts.0, counts.1))
}

fn entry_path(e: &manifest::PathEntry) -> &str {
    match e {
        manifest::PathEntry::File { path, .. } | manifest::PathEntry::Symlink { path, .. } => path,
    }
}

fn walk_into(
    dir: &Path,
    proj_dir: &Path,
    objects_root: &Path,
    paths: &mut Vec<manifest::PathEntry>,
    counts: &mut (usize, usize),
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let p = entry.path();
        if ft.is_symlink() {
            let target = std::fs::read_link(&p)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default();
            paths.push(manifest::PathEntry::Symlink {
                path: rel(&p, proj_dir),
                target,
            });
        } else if ft.is_dir() {
            walk_into(&p, proj_dir, objects_root, paths, counts)?;
        } else if ft.is_file() {
            ingest_file(&p, proj_dir, objects_root, paths, counts)?;
        }
    }
    Ok(())
}

fn ingest_file(
    file: &Path,
    proj_dir: &Path,
    objects_root: &Path,
    paths: &mut Vec<manifest::PathEntry>,
    counts: &mut (usize, usize),
) -> Result<()> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if is_junk(&name) {
        return Ok(());
    }
    let (sha, size, new) = objects::write_object(objects_root, file)?;
    counts.0 += usize::from(new);
    counts.1 += 1;
    paths.push(manifest::PathEntry::File {
        path: rel(file, proj_dir),
        hash: sha,
        size,
    });
    Ok(())
}

/// Path of `p` relative to `proj_dir`, as a forward-slash string.
fn rel(p: &Path, proj_dir: &Path) -> String {
    p.strip_prefix(proj_dir)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Editor/OS junk that shouldn't accrue churny per-file objects. An intentional
/// divergence from HV-41 (whose whole-tree tar swept these in); the dedup win
/// matters more than byte-parity with junk.
fn is_junk(name: &str) -> bool {
    name == ".DS_Store"
        || name == "Thumbs.db"
        || name.ends_with(".swp")
        || name.ends_with(".swo")
        || name.ends_with('~')
}

/// Assemble a [`BackupReport`] from a freshly-written manifest: per-project
/// logical sizes (summed on-disk object bytes) + the DB object's size.
fn build_report_from_manifest(
    backups_root: &Path,
    id: &str,
    m: &manifest::Manifest,
    new_objects: usize,
    total_objects: usize,
) -> BackupReport {
    let objects_root = backups_root.join(objects::OBJECTS_DIR);
    let obj_size = |sha: &str| {
        std::fs::metadata(objects::object_path(&objects_root, sha))
            .map(|md| md.len())
            .unwrap_or(0)
    };
    let archive = format!("{id}.json");
    let mut projects: Vec<ProjectArchive> = m
        .projects
        .iter()
        .map(|p| ProjectArchive {
            key: p.key.clone(),
            archive: archive.clone(),
            bytes: p
                .paths
                .iter()
                .filter_map(|e| match e {
                    manifest::PathEntry::File { hash, .. } => Some(obj_size(hash)),
                    manifest::PathEntry::Symlink { .. } => None,
                })
                .sum(),
        })
        .collect();
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    BackupReport {
        id: id.to_string(),
        db_bytes: obj_size(&m.db.object),
        projects,
        integrity: m.integrity,
        quarantined: m.integrity == Integrity::Suspect,
        new_objects,
        total_objects,
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
/// `content_root` into `dir/<key>.tar.gz`. Used only by the forensic fallback
/// (`raw_forensic_copy`) — forensics want raw bytes in a self-contained dir, not
/// dedup'd objects. The normal snapshot path is `snapshot_to_objects`.
fn archive_project_trees(content_root: &Path, dir: &Path) -> Result<()> {
    for key in discover_project_keys(content_root)? {
        let proj_dir = content_root.join(&key);
        let items_dir = proj_dir.join("items");
        let backlog = proj_dir.join("backlog.md");
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
        if let Ok(report) = snapshot_to_objects(&conn, content_root, backups_root) {
            return report.id;
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

/// Prune snapshots to [`KEEP_DAILY`] + [`KEEP_WEEKLY`] across both formats (one
/// retention timeline over manifests + legacy dirs), then mark-and-sweep GC the
/// objects no surviving manifest references. A **full** no-op while any SUSPECT
/// snapshot exists (legacy `-SUSPECT/` dir or `-SUSPECT.json` manifest): a
/// corruption must not age out the last clean copy, and that pin extends to
/// object GC, not just manifest/dir pruning.
fn rotate(backups_root: &Path) -> Result<()> {
    if !Store::backups_frozen(backups_root)?.is_empty() {
        return Ok(());
    }
    let manifests_dir = backups_root.join(MANIFESTS_DIR);
    let objects_root = backups_root.join(objects::OBJECTS_DIR);

    // Collect snapshot ids from both formats: manifest stems + legacy <ts>/ dirs.
    let mut ids: Vec<String> = Vec::new();
    if manifests_dir.is_dir() {
        for e in std::fs::read_dir(&manifests_dir)?.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            if let Some(stem) = fname.strip_suffix(".json") {
                if !stem.starts_with('.') {
                    ids.push(stem.to_string());
                }
            }
        }
    }
    for e in std::fs::read_dir(backups_root)?.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !RESERVED_BACKUP_NAMES.contains(&name.as_str()) && !name.starts_with('.') {
            ids.push(name);
        }
    }

    // One retention timeline over the union; delete the manifest OR the legacy dir.
    for id in plan_rotation(&ids, KEEP_DAILY, KEEP_WEEKLY) {
        let manifest = manifests_dir.join(format!("{id}.json"));
        if manifest.exists() {
            let _ = std::fs::remove_file(manifest);
        } else {
            let _ = std::fs::remove_dir_all(backups_root.join(&id));
        }
    }

    // GC objects unreferenced by any RETAINED manifest. Skip if the live set can't
    // be fully determined (a corrupt manifest) — the grace window + a future
    // verify-driven freeze are the backstops.
    if let Some(live) = live_object_set(&manifests_dir)? {
        let _ = objects::gc(&objects_root, &live);
    }
    let _ = objects::temp_sweep(&objects_root);
    Ok(())
}

/// The union of objects referenced by every retained manifest — the GC live set.
/// `None` if any manifest fails to read/validate (be conservative: skip GC rather
/// than reap objects a manifest we can't parse might reference).
fn live_object_set(manifests_dir: &Path) -> Result<Option<HashSet<String>>> {
    let mut live = HashSet::new();
    if !manifests_dir.is_dir() {
        return Ok(Some(live));
    }
    for e in std::fs::read_dir(manifests_dir)?.flatten() {
        let fname = e.file_name().to_string_lossy().into_owned();
        if !fname.ends_with(".json") || fname.starts_with('.') {
            continue;
        }
        match manifest::read(&e.path()) {
            Ok(m) => live.extend(m.referenced_objects()),
            Err(_) => return Ok(None), // unreadable manifest → don't risk GC this round
        }
    }
    Ok(Some(live))
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

/// Integrity verdict implied by an id's `-SUSPECT` suffix.
fn suspect_of(id: &str) -> Integrity {
    if id.ends_with(SUSPECT_SUFFIX) {
        Integrity::Suspect
    } else {
        Integrity::Ok
    }
}

/// The `<ts>` part of an id, with any `-SUSPECT` suffix stripped.
fn strip_suspect(id: &str) -> &str {
    id.strip_suffix(SUSPECT_SUFFIX).unwrap_or(id)
}

/// Summed on-disk (compressed) bytes of every object a manifest references.
fn manifest_object_bytes(objects_root: &Path, m: &manifest::Manifest) -> u64 {
    let size = |sha: &str| {
        std::fs::metadata(objects::object_path(objects_root, sha))
            .map(|md| md.len())
            .unwrap_or(0)
    };
    let mut total = size(&m.db.object);
    for p in &m.projects {
        for e in &p.paths {
            if let manifest::PathEntry::File { hash, .. } = e {
                total += size(hash);
            }
        }
    }
    total
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

    fn put_object(scratch: &Path, objects_root: &Path, bytes: &[u8], name: &str) -> String {
        let src = scratch.join(name);
        std::fs::write(&src, bytes).unwrap();
        objects::write_object(objects_root, &src).unwrap().0
    }

    /// Age a file's mtime past the GC grace window so a sweep may reap it.
    fn age(path: &Path) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
            .unwrap();
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
    fn re_snapshot_dedups_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("haven.db");
        let backups = root.join("backups");

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
        std::fs::write(item_dir.join("spec.md"), b"the spec body").unwrap();

        let r1 = s.backup_now(&backups).unwrap();
        assert_eq!(r1.integrity, Integrity::Ok);
        assert!(backups
            .join("manifests")
            .join(format!("{}.json", r1.id))
            .exists());
        assert!(backups.join("objects").is_dir());
        assert!(r1.new_objects >= 2, "DB object + at least one file object");

        // Re-snapshot with NO file change: only the DB object is new (Option A —
        // the DB is a full copy per snapshot); every item-file object dedups to
        // zero. Same wall-clock second → the manifest stamp auto-bumps.
        let r2 = s.backup_now(&backups).unwrap();
        assert_eq!(r2.integrity, Integrity::Ok);
        assert_eq!(
            r2.total_objects, r1.total_objects,
            "same logical object set"
        );
        assert_eq!(r2.new_objects, 1, "only the per-snapshot DB object is new");

        // Change one file AND mutate the store (so the DB genuinely changes) →
        // exactly two new objects: the changed file + the new DB object.
        std::fs::write(item_dir.join("spec.md"), b"the spec body, revised").unwrap();
        s.add_item(
            None,
            NewItem {
                title: "Another".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let r3 = s.backup_now(&backups).unwrap();
        assert_eq!(r3.new_objects, 2, "changed file + changed DB object");
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
            assert!(backups
                .join("manifests")
                .join(format!("{}.json", report.id))
                .exists());
            assert!(backups.join("objects").is_dir());
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
        // Break a referenced object: delete the first stored object so a manifest
        // entry can't be materialized. Restore must fail at staging.
        let objects = backups.join("objects");
        let aa = std::fs::read_dir(&objects)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| {
                e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    && !e.file_name().to_string_lossy().starts_with('.')
            })
            .unwrap()
            .path();
        let obj = std::fs::read_dir(&aa)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::remove_file(&obj).unwrap();

        // Restore must fail at staging — before any live data is touched.
        let err = Store::restore_backup(&db_path, root, &backups, &id).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("refus")
                || matches!(err, HavenError::Invalid(_) | HavenError::Io(_)),
            "got {err:?}"
        );

        // The live content tree is intact, and no staging litter was left behind.
        assert_eq!(
            std::fs::read(root.join("haven/items/HV-1/spec.md")).unwrap(),
            b"live content"
        );
        assert!(!staging_root(root).exists());
        // The store still opens and reads.
        let s = Store::open(&db_path, root).unwrap();
        assert_eq!(s.list_projects(false).unwrap().len(), 1);
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
    fn rotate_gcs_unreferenced_objects_and_suspect_freezes_gc() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let backups = dir.path().join("backups");
        let objects_root = backups.join("objects");
        let manifests_dir = backups.join("manifests");
        std::fs::create_dir_all(&objects_root).unwrap();

        let db_sha = put_object(&scratch, &objects_root, b"db bytes", "db");
        let keep_sha = put_object(&scratch, &objects_root, b"kept file", "keep");
        let orphan_sha = put_object(&scratch, &objects_root, b"orphan file", "orphan");

        // A manifest referencing db + keep (NOT orphan).
        let m = manifest::Manifest::new(
            "20260601T120000Z".into(),
            Integrity::Ok,
            manifest::DbHandle {
                repr: "gz-full".into(),
                object: db_sha.clone(),
            },
            vec![manifest::ProjectEntry {
                key: "haven".into(),
                paths: vec![manifest::PathEntry::File {
                    path: "items/HV-1/spec.md".into(),
                    hash: keep_sha.clone(),
                    size: 9,
                }],
            }],
        );
        manifest::write(&manifests_dir, &m).unwrap();
        age(&objects::object_path(&objects_root, &orphan_sha));

        rotate(&backups).unwrap();
        assert!(objects::object_path(&objects_root, &keep_sha).exists());
        assert!(objects::object_path(&objects_root, &db_sha).exists());
        assert!(
            !objects::object_path(&objects_root, &orphan_sha).exists(),
            "an unreferenced, past-grace object is reaped"
        );

        // Add a SUSPECT manifest + another aged orphan: GC is now frozen.
        let orphan2 = put_object(&scratch, &objects_root, b"orphan two", "orphan2");
        age(&objects::object_path(&objects_root, &orphan2));
        let suspect = manifest::Manifest::new(
            "20260602T120000Z".into(),
            Integrity::Suspect,
            manifest::DbHandle {
                repr: "gz-full".into(),
                object: db_sha.clone(),
            },
            vec![],
        );
        manifest::write(&manifests_dir, &suspect).unwrap();
        assert!(!Store::backups_frozen(&backups).unwrap().is_empty());

        rotate(&backups).unwrap();
        assert!(
            objects::object_path(&objects_root, &orphan2).exists(),
            "GC is frozen while a SUSPECT manifest exists"
        );
    }

    #[test]
    fn legacy_and_manifest_snapshots_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("haven.db");
        let backups = root.join("backups");

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
        std::fs::write(item_dir.join("spec.md"), b"v1 content").unwrap();

        // A new (content-addressed) snapshot.
        let new_id = s.backup_now(&backups).unwrap().id;

        // Hand-build a LEGACY <ts>/ snapshot (yesterday, so rotation keeps it) from
        // the same store, using the retained legacy helpers.
        let legacy_id = time::utc_stamp(time::now_secs() - 86_400);
        let snap = backups.join(&legacy_id);
        std::fs::create_dir_all(&snap).unwrap();
        backup_conn_to_file(&s.conn, &snap.join("haven.db")).unwrap();
        archive_project_trees(root, &snap).unwrap();

        // Both list, tagged by format.
        let listed = Store::list_backups(&backups).unwrap();
        assert!(listed
            .iter()
            .any(|e| e.id == new_id && e.format == BackupFormat::Manifest));
        assert!(listed
            .iter()
            .any(|e| e.id == legacy_id && e.format == BackupFormat::Legacy));

        // The legacy snapshot restores (drop the store first so WAL checkpoints).
        drop(s);
        std::fs::write(item_dir.join("spec.md"), b"changed").unwrap();
        Store::restore_backup(&db_path, root, &backups, &legacy_id).unwrap();
        assert_eq!(
            std::fs::read(item_dir.join("spec.md")).unwrap(),
            b"v1 content"
        );

        // Rotation never deletes the other format's storage internals.
        rotate(&backups).unwrap();
        assert!(backups.join("manifests").is_dir(), "manifests/ not pruned");
        assert!(backups.join("objects").is_dir(), "objects/ not pruned");
        assert!(
            backups.join(&legacy_id).join("haven.db").exists(),
            "a recent legacy snapshot survives rotation"
        );
    }

    #[test]
    fn verify_quarantines_a_poisoned_object_and_freezes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("haven.db");
        let backups = root.join("backups");

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
        std::fs::write(item_dir.join("spec.md"), b"content").unwrap();
        let id = s.backup_now(&backups).unwrap().id;
        assert_eq!(Store::verify_backup(&backups, &id).unwrap(), Integrity::Ok);

        // Poison the spec.md object (corrupt its bytes at rest).
        let m = manifest::read(&backups.join("manifests").join(format!("{id}.json"))).unwrap();
        let spec_hash = m.projects[0]
            .paths
            .iter()
            .find_map(|e| match e {
                manifest::PathEntry::File { hash, .. } => Some(hash.clone()),
                _ => None,
            })
            .unwrap();
        std::fs::write(
            objects::object_path(&backups.join("objects"), &spec_hash),
            b"corrupt",
        )
        .unwrap();

        // Verify detects it → quarantines the object, marks the snapshot SUSPECT,
        // freezing the store.
        assert_eq!(
            Store::verify_backup(&backups, &id).unwrap(),
            Integrity::Suspect
        );
        assert!(
            !Store::backups_frozen(&backups).unwrap().is_empty(),
            "verify froze the store"
        );
        assert!(backups
            .join("objects")
            .join(".quarantine")
            .join(&spec_hash)
            .exists());
    }

    #[test]
    fn edge_cases_symlink_empty_file_and_no_self_ingestion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db_path = root.join("haven.db");
        let backups = root.join("backups");

        let s = Store::open(&db_path, root).unwrap();
        s.add_project("haven", Some("HV"), "Haven", None).unwrap();
        // A project literally keyed `objects`: its content lives at <root>/objects/,
        // which must NOT collide with the backup object store at <root>/backups/objects/.
        s.add_project("objects", Some("OB"), "Objects", None)
            .unwrap();
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
        std::fs::write(item_dir.join("spec.md"), b"body").unwrap();
        std::fs::write(item_dir.join("empty.md"), b"").unwrap();
        std::fs::write(item_dir.join(".DS_Store"), b"junk").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("spec.md", item_dir.join("link.md")).unwrap();
        let ob_dir = root.join("objects/items/OB-1");
        std::fs::create_dir_all(&ob_dir).unwrap();
        std::fs::write(ob_dir.join("note.md"), b"ob note").unwrap();

        let id = s.backup_now(&backups).unwrap().id;
        drop(s);

        // Wipe live content and restore.
        std::fs::remove_dir_all(root.join("haven/items")).unwrap();
        std::fs::remove_dir_all(root.join("objects/items")).unwrap();
        Store::restore_backup(&db_path, root, &backups, &id).unwrap();

        assert_eq!(std::fs::read(item_dir.join("spec.md")).unwrap(), b"body");
        assert_eq!(
            std::fs::read(item_dir.join("empty.md")).unwrap(),
            b"",
            "empty file is materialized, not skipped"
        );
        assert!(!item_dir.join(".DS_Store").exists(), "junk is filtered out");
        #[cfg(unix)]
        {
            let link = item_dir.join("link.md");
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                std::path::PathBuf::from("spec.md")
            );
        }
        assert_eq!(std::fs::read(ob_dir.join("note.md")).unwrap(), b"ob note");

        // No self-ingestion: the manifest references only the few real content
        // files, not the (recursively growing) backup object store.
        let m = manifest::read(&backups.join("manifests").join(format!("{id}.json"))).unwrap();
        let total: usize = m.projects.iter().map(|p| p.paths.len()).sum();
        assert!(total <= 5, "no self-ingestion blow-up (got {total})");
    }

    #[test]
    fn clear_quarantine_removes_the_marker_and_unfreezes() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let manifests = backups.join("manifests");
        std::fs::create_dir_all(&manifests).unwrap();
        let m = manifest::Manifest::new(
            "20260601T120000Z".into(),
            Integrity::Suspect,
            manifest::DbHandle {
                repr: "gz-full".into(),
                object: "a".repeat(64),
            },
            vec![],
        );
        manifest::write(&manifests, &m).unwrap();
        assert!(!Store::backups_frozen(&backups).unwrap().is_empty());

        Store::clear_quarantine(&backups, "20260601T120000Z-SUSPECT").unwrap();
        assert!(Store::backups_frozen(&backups).unwrap().is_empty());
        // A non-suspect id is rejected (can't accidentally clear a good snapshot).
        assert!(Store::clear_quarantine(&backups, "20260601T120000Z").is_err());
    }

    #[test]
    fn quick_check_flags_a_corrupt_database() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("garbage.db");
        std::fs::write(&junk, b"this is definitely not a sqlite database").unwrap();
        assert!(!quick_check_path(&junk).unwrap(), "garbage must fail check");
    }
}
