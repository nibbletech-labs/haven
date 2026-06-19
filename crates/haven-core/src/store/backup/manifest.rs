//! Snapshot manifests (HV-90): the per-snapshot index that maps each logical
//! path to a content-addressed object hash. In the content-addressed format a
//! manifest *is* the snapshot — it replaces HV-41's self-contained `<ts>/` dir.
//!
//! A manifest is the only index from paths/DB to objects, so a torn or partial
//! manifest would mean a silently-incomplete restore. Two defences: it is written
//! atomically (temp → fsync → rename), and it is **self-validating** — a
//! `path_count` and a `body_checksum` (sha256 over the canonical body, the
//! `body_checksum` field excluded) let a reader reject a truncated/corrupt
//! manifest rather than restore from it.

use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Integrity;
use crate::error::{HavenError, Result};
use crate::util::{fsync_dir, hex};

/// Bumped if the on-disk manifest schema changes incompatibly. A reader refuses a
/// manifest whose version it does not understand.
pub(super) const MANIFEST_VERSION: u32 = 1;

/// One snapshot, as stored at `manifests/<UTC-ts>[-SUSPECT].json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub version: u32,
    /// `utc_stamp` — the snapshot id (lexical == chronological).
    pub ts: String,
    pub integrity: Integrity,
    pub hash_algo: String,
    pub compression: String,
    /// The DB stored as a content-addressed *handle*, not an inline blob — so a
    /// future `repr` (e.g. page-level dedup) slots in with no format break.
    pub db: DbHandle,
    pub projects: Vec<ProjectEntry>,
    /// Total `PathEntry` count across all projects (self-validation: a torn
    /// manifest whose array was truncated fails this check).
    pub path_count: usize,
    /// sha256 of the canonical body (every field above; this one excluded).
    pub body_checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DbHandle {
    /// v1: `"gz-full"` (one gzip object per snapshot). Restore switches on this.
    pub repr: String,
    /// sha256 of the *uncompressed* consolidated DB.
    pub object: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProjectEntry {
    pub key: String,
    pub paths: Vec<PathEntry>,
}

/// One logical path in a project's content tree. Files reference an object by its
/// uncompressed-content sha256; symlinks are recorded by target (never followed,
/// so the walk can't escape the tree or loop) and own no object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum PathEntry {
    File {
        path: String,
        hash: String,
        size: u64,
    },
    Symlink {
        path: String,
        target: String,
    },
}

/// The canonical body, excluding `body_checksum`. A dedicated borrow-struct (not
/// "the file minus a line") so the checksum is over structured bytes and is
/// independent of file formatting — write and read MUST compute it identically.
#[derive(Serialize)]
struct ManifestBody<'a> {
    version: u32,
    ts: &'a str,
    integrity: Integrity,
    hash_algo: &'a str,
    compression: &'a str,
    db: &'a DbHandle,
    projects: &'a [ProjectEntry],
    path_count: usize,
}

fn body_checksum(m: &Manifest) -> String {
    let body = ManifestBody {
        version: m.version,
        ts: &m.ts,
        integrity: m.integrity,
        hash_algo: &m.hash_algo,
        compression: &m.compression,
        db: &m.db,
        projects: &m.projects,
        path_count: m.path_count,
    };
    // Vec-only (no HashMap) + fixed struct field order ⇒ deterministic bytes.
    let bytes = serde_json::to_vec(&body).expect("manifest body serializes");
    hex(&Sha256::digest(&bytes))
}

impl Manifest {
    /// Build a manifest, filling the self-validation fields. The caller supplies
    /// `ts`/`integrity`/`db`/`projects`; this sets `version`, `path_count`,
    /// `body_checksum` and the fixed algo/compression tags.
    pub(super) fn new(
        ts: String,
        integrity: Integrity,
        db: DbHandle,
        projects: Vec<ProjectEntry>,
    ) -> Manifest {
        let path_count = projects.iter().map(|p| p.paths.len()).sum();
        let mut m = Manifest {
            version: MANIFEST_VERSION,
            ts,
            integrity,
            hash_algo: "sha256".into(),
            compression: "gzip".into(),
            db,
            projects,
            path_count,
            body_checksum: String::new(),
        };
        m.body_checksum = body_checksum(&m);
        m
    }

    /// Re-stamp with a new `ts` (recomputing the body checksum). Used to side-step
    /// a same-second manifest-name collision without rebuilding from objects.
    pub(super) fn restamp(&mut self, ts: String) {
        self.ts = ts;
        self.body_checksum = body_checksum(self);
    }

    /// Every object hash a restore/GC must treat as live: the DB object plus every
    /// `File` entry's hash (symlinks own no object).
    pub(super) fn referenced_objects(&self) -> std::collections::HashSet<String> {
        let mut set = std::collections::HashSet::new();
        set.insert(self.db.object.clone());
        for p in &self.projects {
            for e in &p.paths {
                if let PathEntry::File { hash, .. } = e {
                    set.insert(hash.clone());
                }
            }
        }
        set
    }
}

/// Reject a torn/corrupt/unknown manifest (mirrors the corrupt-source refusal in
/// `restore_backup`): unknown version, a `path_count` that disagrees with the
/// actual entries, or a `body_checksum` that does not recompute.
pub(super) fn validate(m: &Manifest) -> Result<()> {
    if m.version != MANIFEST_VERSION {
        return Err(HavenError::Invalid(format!(
            "backup manifest version {} unsupported (this binary writes v{MANIFEST_VERSION})",
            m.version
        )));
    }
    let actual: usize = m.projects.iter().map(|p| p.paths.len()).sum();
    if actual != m.path_count {
        return Err(HavenError::Invalid(format!(
            "backup manifest path_count {} != {actual} actual entries; refusing a torn manifest",
            m.path_count
        )));
    }
    if body_checksum(m) != m.body_checksum {
        return Err(HavenError::Invalid(
            "backup manifest body checksum mismatch; refusing a torn/corrupt manifest".into(),
        ));
    }
    Ok(())
}

/// Parse and validate a manifest file. Any parse/validation failure is reported
/// as `Invalid` so restore refuses it rather than restoring partially.
pub(super) fn read(path: &Path) -> Result<Manifest> {
    let bytes = std::fs::read(path)?;
    let m: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| HavenError::Invalid(format!("backup manifest {}: {e}", path.display())))?;
    validate(&m)?;
    Ok(m)
}

/// Atomically write a manifest into `manifests_dir` as `<ts>[-SUSPECT].json`:
/// serialize → temp → fsync → rename → fsync dir. Refuses to clobber an existing
/// name (race-free under the backup lock; belt against a same-second collision).
/// Returns the file name written.
pub(super) fn write(manifests_dir: &Path, m: &Manifest) -> Result<String> {
    validate(m)?; // never persist an inconsistent manifest
    std::fs::create_dir_all(manifests_dir)?;
    let suffix = if m.integrity == Integrity::Suspect {
        super::SUSPECT_SUFFIX
    } else {
        ""
    };
    let name = format!("{}{suffix}.json", m.ts);
    let final_path = manifests_dir.join(&name);
    if final_path.exists() {
        return Err(HavenError::Conflict(format!(
            "backup manifest {name} already exists; refusing to overwrite"
        )));
    }
    let tmp = manifests_dir.join(format!(".tmp-{}-{name}", std::process::id()));
    let json = serde_json::to_vec_pretty(m)
        .map_err(|e| HavenError::Invalid(format!("serializing backup manifest: {e}")))?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &final_path)?;
    let _ = fsync_dir(manifests_dir);
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest::new(
            "20260619T120000Z".into(),
            Integrity::Ok,
            DbHandle {
                repr: "gz-full".into(),
                object: "a".repeat(64),
            },
            vec![ProjectEntry {
                key: "haven".into(),
                paths: vec![
                    PathEntry::File {
                        path: "items/HV-1/spec.md".into(),
                        hash: "b".repeat(64),
                        size: 12,
                    },
                    PathEntry::Symlink {
                        path: "items/HV-2".into(),
                        target: "../HV-1".into(),
                    },
                ],
            }],
        )
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let m = sample();
        assert_eq!(m.path_count, 2);
        let name = write(dir.path(), &m).unwrap();
        assert_eq!(name, "20260619T120000Z.json");
        let back = read(&dir.path().join(&name)).unwrap();
        assert_eq!(back.body_checksum, m.body_checksum);
        assert_eq!(back.path_count, 2);
        // db object + one file hash (symlink owns none).
        assert_eq!(back.referenced_objects().len(), 2);
    }

    #[test]
    fn suspect_manifest_gets_the_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = sample();
        m.integrity = Integrity::Suspect;
        m = Manifest::new(m.ts, m.integrity, m.db, m.projects); // refresh checksum
        let name = write(dir.path(), &m).unwrap();
        assert_eq!(name, "20260619T120000Z-SUSPECT.json");
    }

    #[test]
    fn tampered_path_count_is_rejected() {
        let mut m = sample();
        m.path_count = 99;
        assert!(validate(&m).is_err());
    }

    #[test]
    fn tampered_body_is_rejected() {
        let mut m = sample();
        // Mutate a field without recomputing the checksum (a torn/edited manifest).
        m.ts = "20260619T130000Z".into();
        assert!(validate(&m).is_err());
    }

    #[test]
    fn refuses_to_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let m = sample();
        write(dir.path(), &m).unwrap();
        assert!(write(dir.path(), &m).is_err());
    }
}
