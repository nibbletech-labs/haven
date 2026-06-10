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

pub use engine::{RemoteSnapshot, SyncConfig, SyncEngine};
pub use local::{write_hydrated, ReconcileStats};

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

/// Extract the `sub` claim from a JWT's payload, **without verifying the
/// signature** — the client only needs the subject to build Storage object keys
/// (`<sub>/<project>/items/...`); identity is enforced server-side, where
/// Supabase verifies the same token against Auth0's JWKS and Storage RLS
/// requires the key's first segment to equal the verified `sub`. A forged claim
/// here just produces uploads the server rejects.
pub fn jwt_sub(token: &str) -> Result<String, SyncError> {
    use base64::Engine;
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| SyncError::Permanent("token is not a JWT (no payload segment)".into()))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| SyncError::Permanent(format!("token payload is not base64url: {e}")))?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| SyncError::Permanent(format!("token payload is not JSON: {e}")))?;
    claims["sub"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| SyncError::Permanent("token has no `sub` claim".into()))
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
    fn jwt_sub_reads_the_subject_claim() {
        use base64::Engine;
        let enc = |v: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        // Same shape as the hand-built token used for the live push validation.
        let header = enc(&serde_json::json!({"alg": "HS256", "typ": "JWT"}));
        let payload = enc(&serde_json::json!({"sub": "test-user", "role": "authenticated"}));
        let token = format!("{header}.{payload}.fakesig");
        assert_eq!(jwt_sub(&token).unwrap(), "test-user");

        // Garbage and missing-claim tokens are rejected, not panicked on.
        assert!(jwt_sub("not-a-jwt").is_err());
        assert!(jwt_sub("a.%%%.c").is_err());
        let no_sub = format!("{header}.{}.s", enc(&serde_json::json!({"aud": "x"})));
        assert!(jwt_sub(&no_sub).is_err());
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
