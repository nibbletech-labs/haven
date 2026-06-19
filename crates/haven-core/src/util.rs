//! Small shared helpers with no home of their own.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Lowercase hex encoding of a byte slice. The canonical content-hash encoding
/// used across the crate: artifact `content_hash` (content layer) and
/// content-addressed backup object names both name bytes by `hex(Sha256::digest)`,
/// so the two must use the *same* encoding to interoperate.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// `fsync` a directory so a rename into it (a new file appearing) is durable.
/// Best-effort minimal durability for the backup commit ordering (HV-90 owns the
/// *ordering*; a general fsync mechanism is HV-89). Opening a directory read-only
/// and `sync_all`-ing it is the portable POSIX idiom; on platforms that reject a
/// dir handle for fsync the error is swallowed (durability degrades, correctness
/// does not — the rename is still atomic).
pub(crate) fn fsync_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::File::open(path) {
        Ok(f) => match f.sync_all() {
            Ok(()) => Ok(()),
            // Some filesystems return EINVAL/EACCES for a directory fsync; the
            // rename is still atomic, so treat as a soft durability miss.
            Err(_) => Ok(()),
        },
        Err(e) => Err(e),
    }
}

/// Whether a path's mtime is older than `dur` ago. Used for grace-window / stale
/// checks (GC won't reap a freshly-written object; a crashed process's lock can be
/// stolen once stale). mtime is preferred over embedded PIDs — PID reuse is
/// unreliable across platforms. An unreadable mtime is treated as "not old".
pub(crate) fn older_than(path: &Path, dur: Duration) -> bool {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => SystemTime::now()
            .duration_since(mtime)
            .map(|age| age > dur)
            .unwrap_or(false),
        Err(_) => false,
    }
}
