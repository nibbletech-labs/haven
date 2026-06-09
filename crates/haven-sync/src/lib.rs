//! `haven-sync` — offline-first sync against Supabase (SPEC §5), mirroring the
//! proven servo pattern: local writes land immediately; a background pass pushes
//! to PostgREST with exponential backoff; the append-only lineage core never
//! conflicts; mutable rows are last-write-wins by `revision`; content blobs go
//! to Storage and download lazily.
//!
//! ## Verification status
//! The pure pieces — error classification, the backoff schedule, and the local
//! FK→`public_id` translation that builds push payloads — are unit-tested. The
//! HTTP transport ([`engine`]) is **written but not live-verified**; it needs a
//! real Supabase project (URL + keys) and a valid Auth0 token. See `STATUS.md`.

pub mod engine;
pub mod local;

use std::time::Duration;

use thiserror::Error;

pub use engine::{SyncConfig, SyncEngine};

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("unauthorized — token refresh required")]
    Unauthorized,
    #[error("permanent failure: {0}")]
    Permanent(String),
    #[error("transient failure: {0}")]
    Transient(String),
}

/// The servo error taxonomy (SPEC §5) — drives retry behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 409 / PK conflict — treat as success, mark synced, no retry.
    Duplicate,
    /// 401 — refresh the Auth0 token, then resume the pass.
    Unauthorized,
    /// network / 5xx / timeout — retry with backoff.
    Transient,
    /// 4xx non-409, CHECK/FK violation — mark failed, surface, no retry.
    Permanent,
}

/// Classify an HTTP status into the retry taxonomy.
pub fn classify_status(status: u16) -> ErrorClass {
    match status {
        409 => ErrorClass::Duplicate,
        401 => ErrorClass::Unauthorized,
        s if (500..=599).contains(&s) => ErrorClass::Transient,
        408 | 425 | 429 => ErrorClass::Transient,
        _ => ErrorClass::Permanent,
    }
}

/// The backoff schedule: `1s, 2s, 5s, 30s, 5m` (~7 passes). Returns `None` once
/// the schedule is exhausted (give up / mark failed).
pub fn backoff(attempt: u32) -> Option<Duration> {
    const SCHEDULE_SECS: [u64; 5] = [1, 2, 5, 30, 300];
    SCHEDULE_SECS
        .get(attempt as usize)
        .copied()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_statuses() {
        assert_eq!(classify_status(409), ErrorClass::Duplicate);
        assert_eq!(classify_status(401), ErrorClass::Unauthorized);
        assert_eq!(classify_status(500), ErrorClass::Transient);
        assert_eq!(classify_status(503), ErrorClass::Transient);
        assert_eq!(classify_status(429), ErrorClass::Transient);
        assert_eq!(classify_status(400), ErrorClass::Permanent);
        assert_eq!(classify_status(422), ErrorClass::Permanent);
    }

    #[test]
    fn backoff_schedule_then_exhausts() {
        assert_eq!(backoff(0), Some(Duration::from_secs(1)));
        assert_eq!(backoff(1), Some(Duration::from_secs(2)));
        assert_eq!(backoff(2), Some(Duration::from_secs(5)));
        assert_eq!(backoff(3), Some(Duration::from_secs(30)));
        assert_eq!(backoff(4), Some(Duration::from_secs(300)));
        assert_eq!(backoff(5), None);
    }
}
