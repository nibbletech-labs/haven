//! Content-addressed object store for backups (HV-90).
//!
//! `objects/<aa>/<sha256>` — each unique file's bytes, written once and named by
//! the sha256 of its **uncompressed** content (`aa` = first two hex chars, a
//! fan-out dir). Bytes on disk are gzip-compressed; compression is a storage
//! detail, never part of identity, so two identical files dedup regardless of how
//! they compress and `verify` must decompress-then-hash.
//!
//! Objects are immutable once named. A write is temp → fsync → rename (atomic);
//! an object whose name already exists is a no-op (the dedup win). Reaping
//! (`gc`) only deletes a *named* object that no retained manifest references AND
//! that is older than a grace window (so a crashed-mid-snapshot object a future
//! identical snapshot will re-reference is not lost); `temp_sweep` reclaims
//! orphaned `.tmp-*` writes (which `gc` never sees).

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

use crate::error::{HavenError, Result};
use crate::util::{fsync_dir, hex, older_than};

pub(super) const OBJECTS_DIR: &str = "objects";
const QUARANTINE_DIR: &str = ".quarantine";
const TMP_PREFIX: &str = ".tmp-";
/// GC / temp-sweep grace: never reap anything younger than this.
const GC_GRACE: Duration = Duration::from_secs(3600);

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_name() -> String {
    // Cross-process unique via pid; within-process via a counter. The lock
    // serializes writers anyway, so this only needs to avoid self-collision.
    format!(
        "{TMP_PREFIX}{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// `objects/<aa>/<sha256>`.
pub(super) fn object_path(objects_root: &Path, sha: &str) -> PathBuf {
    objects_root.join(&sha[..2]).join(sha)
}

/// Stream-hash a file's uncompressed content → `(sha256-hex, byte length)`.
/// Streamed so an arbitrarily large file never lands fully in memory.
pub(super) fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((hex(&hasher.finalize()), len))
}

/// Write `src`'s bytes as a content-addressed object: one streaming pass hashes
/// the uncompressed content while gzipping it to a temp; if the resulting object
/// name already exists the temp is discarded (dedup no-op), else it is fsync'd and
/// atomically renamed into place. Returns `(sha256, uncompressed_size, newly_written)`.
pub(super) fn write_object(objects_root: &Path, src: &Path) -> Result<(String, u64, bool)> {
    std::fs::create_dir_all(objects_root)?;
    let tmp = objects_root.join(temp_name());
    let (sha, size) = {
        let mut input = std::fs::File::open(src)?;
        let out = std::fs::File::create(&tmp)?;
        let mut enc = GzEncoder::new(out, Compression::fast());
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut size = 0u64;
        loop {
            let n = match input.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
            };
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            enc.write_all(&buf[..n])?;
            size += n as u64;
        }
        let out = enc.finish()?;
        out.sync_all()?; // object bytes durable BEFORE the rename
        (hex(&hasher.finalize()), size)
    };
    let final_path = object_path(objects_root, &sha);
    if final_path.exists() {
        let _ = std::fs::remove_file(&tmp); // already have it — the dedup win
        return Ok((sha, size, false));
    }
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&tmp, &final_path)?;
    if let Some(parent) = final_path.parent() {
        let _ = fsync_dir(parent);
    }
    Ok((sha, size, true))
}

/// Decompress object `sha` to `dest` (parent dirs created). Errors if the object
/// is missing/unreadable — restore turns that into a refusal at staging.
pub(super) fn read_object_to(objects_root: &Path, sha: &str, dest: &Path) -> Result<()> {
    let obj = object_path(objects_root, sha);
    let f = std::fs::File::open(&obj)
        .map_err(|e| HavenError::Invalid(format!("backup object {sha} unreadable: {e}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = std::fs::File::create(dest)?;
    let mut dec = GzDecoder::new(f);
    std::io::copy(&mut dec, &mut out)?;
    Ok(())
}

/// Verify an object resolves and re-hashes to its name (decompress → sha256 →
/// compare). A missing object or a corrupt gzip is `false`, not an error — the
/// caller (verify) turns a `false` into quarantine + freeze.
pub(super) fn verify_object(objects_root: &Path, sha: &str) -> Result<bool> {
    let obj = object_path(objects_root, sha);
    let Ok(f) = std::fs::File::open(&obj) else {
        return Ok(false);
    };
    let mut dec = GzDecoder::new(f);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match dec.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return Ok(false), // corrupt gzip member
        }
    }
    Ok(hex(&hasher.finalize()) == sha)
}

/// Move a poisoned object out of the addressable store into `objects/.quarantine/`
/// so no future write-skip ever trusts its (wrong) name again.
pub(super) fn quarantine_object(objects_root: &Path, sha: &str) -> Result<()> {
    let obj = object_path(objects_root, sha);
    if !obj.exists() {
        return Ok(());
    }
    let q = objects_root.join(QUARANTINE_DIR);
    std::fs::create_dir_all(&q)?;
    let _ = std::fs::rename(&obj, q.join(sha));
    Ok(())
}

/// Mark-and-sweep: delete named objects not in `live` and older than the grace
/// window. Skips dot-children (`.quarantine`, temps). The caller must hold the
/// backup lock and must NOT call this while the store is frozen (a SUSPECT
/// snapshot) — that decision lives in the orchestration, not here.
pub(super) fn gc(objects_root: &Path, live: &HashSet<String>) -> Result<usize> {
    gc_with_grace(objects_root, live, GC_GRACE)
}

fn gc_with_grace(objects_root: &Path, live: &HashSet<String>, grace: Duration) -> Result<usize> {
    if !objects_root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for aa in std::fs::read_dir(objects_root)? {
        let aa = aa?;
        let name = aa.file_name().to_string_lossy().into_owned();
        // Fan-out dirs are 2 hex chars; skip the quarantine + any dotfile/temp.
        if name.starts_with('.') || !aa.file_type()?.is_dir() {
            continue;
        }
        for obj in std::fs::read_dir(aa.path())? {
            let obj = obj?;
            let sha = obj.file_name().to_string_lossy().into_owned();
            if sha.starts_with('.') || live.contains(&sha) {
                continue;
            }
            if !older_than(&obj.path(), grace) {
                continue; // grace window — a racing/crashed snapshot may re-reference it
            }
            std::fs::remove_file(obj.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Reclaim orphaned `.tmp-*` object writes (a crash between temp-write and rename)
/// older than the grace window. `gc` only sees *named* objects, so without this
/// temps would leak forever.
pub(super) fn temp_sweep(objects_root: &Path) -> Result<usize> {
    temp_sweep_with_grace(objects_root, GC_GRACE)
}

fn temp_sweep_with_grace(objects_root: &Path, grace: Duration) -> Result<usize> {
    if !objects_root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(objects_root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(TMP_PREFIX) && older_than(&entry.path(), grace) {
            let _ = std::fs::remove_file(entry.path());
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_src(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn write_is_idempotent_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let objs = dir.path().join("objects");
        let a = write_src(dir.path(), "a.txt", b"hello world");
        let b = write_src(dir.path(), "b.txt", b"hello world"); // identical bytes
        let (sha1, size1, new1) = write_object(&objs, &a).unwrap();
        assert!(new1 && size1 == 11);
        let (sha2, _, new2) = write_object(&objs, &b).unwrap();
        assert_eq!(sha1, sha2, "identical content → same object");
        assert!(!new2, "second identical write is a dedup no-op");
        assert!(object_path(&objs, &sha1).exists());
        // The object name is exactly the standalone hash of the source bytes.
        assert_eq!(hash_file(&a).unwrap(), (sha1, 11));
    }

    #[test]
    fn read_round_trips_and_verify_detects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let objs = dir.path().join("objects");
        let src = write_src(dir.path(), "s.txt", b"some content bytes");
        let (sha, _, _) = write_object(&objs, &src).unwrap();
        assert!(verify_object(&objs, &sha).unwrap());

        let dest = dir.path().join("out.txt");
        read_object_to(&objs, &sha, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"some content bytes");

        // Corrupt the stored bytes → verify fails (missing also fails).
        std::fs::write(object_path(&objs, &sha), b"not a gzip stream").unwrap();
        assert!(!verify_object(&objs, &sha).unwrap());
        assert!(!verify_object(&objs, &"f".repeat(64)).unwrap());
    }

    #[test]
    fn gc_reaps_only_unreferenced_past_grace() {
        let dir = tempfile::tempdir().unwrap();
        let objs = dir.path().join("objects");
        let keep = write_src(dir.path(), "k.txt", b"keep me");
        let drop_ = write_src(dir.path(), "d.txt", b"drop me");
        let (keep_sha, _, _) = write_object(&objs, &keep).unwrap();
        let (drop_sha, _, _) = write_object(&objs, &drop_).unwrap();

        let mut live = HashSet::new();
        live.insert(keep_sha.clone());

        // Fresh objects: grace protects even the unreferenced one.
        assert_eq!(gc(&objs, &live).unwrap(), 0);
        // Zero grace: the unreferenced one is reaped, the referenced one survives.
        assert_eq!(gc_with_grace(&objs, &live, Duration::ZERO).unwrap(), 1);
        assert!(object_path(&objs, &keep_sha).exists());
        assert!(!object_path(&objs, &drop_sha).exists());
    }

    #[test]
    fn quarantine_moves_the_object_out_of_addressable_space() {
        let dir = tempfile::tempdir().unwrap();
        let objs = dir.path().join("objects");
        let src = write_src(dir.path(), "p.txt", b"poison");
        let (sha, _, _) = write_object(&objs, &src).unwrap();
        quarantine_object(&objs, &sha).unwrap();
        assert!(!object_path(&objs, &sha).exists());
        assert!(objs.join(QUARANTINE_DIR).join(&sha).exists());
    }

    #[test]
    fn temp_sweep_reclaims_orphans_past_grace() {
        let dir = tempfile::tempdir().unwrap();
        let objs = dir.path().join("objects");
        std::fs::create_dir_all(&objs).unwrap();
        std::fs::write(objs.join(format!("{TMP_PREFIX}123-0")), b"orphan").unwrap();
        assert_eq!(temp_sweep(&objs).unwrap(), 0, "grace protects a fresh temp");
        assert_eq!(temp_sweep_with_grace(&objs, Duration::ZERO).unwrap(), 1);
    }
}
