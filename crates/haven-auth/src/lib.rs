//! `haven-auth` — the Auth0 token spine (SPEC §6).
//!
//! CLI-friendly OAuth: the **Device Authorization** flow (no browser plumbing on
//! the box running `haven`). Tokens live in the OS keyring; they auto-refresh
//! ~60s before expiry, and a sync `401` forces a refresh then resumes the pass.
//! Supabase verifies the resulting Auth0 JWT against Auth0's JWKS.
//!
//! ## Verification status
//! The pure logic here (token expiry math, (de)serialization, response parsing)
//! is unit-tested. The network calls and keyring access are **written but not
//! live-verified** — they need a real Auth0 tenant (domain, client id, API
//! audience). See `STATUS.md`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod device;
pub mod keyring_store;

pub use device::DeviceAuthorization;
pub use keyring_store::TokenStore;

/// Refresh this many seconds before the access token actually expires.
pub const REFRESH_SKEW_SECS: u64 = 60;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not logged in")]
    NotLoggedIn,
    #[error("no refresh token available; re-run `haven auth login`")]
    NoRefreshToken,
    #[error("device flow timed out before authorization")]
    DeviceFlowTimeout,
    #[error("authorization denied: {0}")]
    Denied(String),
    #[error("auth0 error: {0}")]
    Auth0(String),
}

pub type Result<T> = std::result::Result<T, AuthError>;

/// Auth0 tenant configuration for the Haven CLI app.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub domain: String,
    pub client_id: String,
    /// The Supabase API audience, so PostgREST/Storage accept the token.
    pub audience: String,
    pub scope: String,
}

impl AuthConfig {
    pub fn new(
        domain: impl Into<String>,
        client_id: impl Into<String>,
        audience: impl Into<String>,
    ) -> Self {
        AuthConfig {
            domain: domain.into(),
            client_id: client_id.into(),
            audience: audience.into(),
            scope: "openid profile email offline_access".to_string(),
        }
    }

    fn token_url(&self) -> String {
        format!("https://{}/oauth/token", self.domain)
    }
    fn device_code_url(&self) -> String {
        format!("https://{}/oauth/device/code", self.domain)
    }
}

/// Stored token set. `expires_at` is an absolute UNIX timestamp (seconds).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}

impl Tokens {
    /// Build from an Auth0 token response (which carries a relative
    /// `expires_in`), stamping an absolute `expires_at` from `now`.
    pub fn from_response(resp: TokenResponse, now: u64) -> Self {
        Tokens {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_at: now.saturating_add(resp.expires_in),
        }
    }

    /// True when the token is within `skew` seconds of expiry (or past it).
    pub fn is_expired(&self, now: u64, skew: u64) -> bool {
        now.saturating_add(skew) >= self.expires_at
    }
}

/// The raw Auth0 `/oauth/token` success payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default)]
    pub token_type: String,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Refresh the access token using the stored refresh token.
pub async fn refresh(cfg: &AuthConfig, refresh_token: &str) -> Result<Tokens> {
    let client = reqwest::Client::new();
    let resp = client
        .post(cfg.token_url())
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", cfg.client_id.as_str()),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Auth0(format!("refresh failed: {body}")));
    }
    let token: TokenResponse = resp.json().await?;
    // Auth0 may rotate the refresh token; keep the old one if absent.
    let mut tokens = Tokens::from_response(token, now_secs());
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

/// Return a valid access token, refreshing (and persisting) if it's near expiry.
/// The single entry point the sync layer calls before each pass.
pub async fn current_access_token(cfg: &AuthConfig, store: &TokenStore) -> Result<String> {
    let tokens = store.load()?.ok_or(AuthError::NotLoggedIn)?;
    if !tokens.is_expired(now_secs(), REFRESH_SKEW_SECS) {
        return Ok(tokens.access_token);
    }
    let refresh_token = tokens
        .refresh_token
        .clone()
        .ok_or(AuthError::NoRefreshToken)?;
    let fresh = refresh(cfg, &refresh_token).await?;
    // Persist the rotated token set, but don't fail the session if the keyring is
    // momentarily unavailable — return the fresh token so sync can proceed. (If
    // Auth0 rotated the refresh token, the next cold start may need a re-login.)
    if let Err(e) = store.save(&fresh) {
        eprintln!("warn: could not persist refreshed token: {e}");
    }
    Ok(fresh.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_math_uses_skew() {
        let t = Tokens {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 1000,
        };
        assert!(!t.is_expired(900, 60)); // 960 < 1000
        assert!(t.is_expired(940, 60)); // 1000 >= 1000
        assert!(t.is_expired(1001, 0)); // already past
    }

    #[test]
    fn from_response_stamps_absolute_expiry() {
        let resp = TokenResponse {
            access_token: "tok".into(),
            refresh_token: Some("r".into()),
            expires_in: 3600,
            token_type: "Bearer".into(),
        };
        let t = Tokens::from_response(resp, 1_000_000);
        assert_eq!(t.expires_at, 1_003_600);
        assert_eq!(t.refresh_token.as_deref(), Some("r"));
    }

    #[test]
    fn tokens_round_trip_json() {
        let t = Tokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: 42,
        };
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Tokens>(&s).unwrap(), t);
    }

    #[test]
    fn token_response_parses_auth0_shape() {
        let json = r#"{"access_token":"AT","refresh_token":"RT","expires_in":86400,"token_type":"Bearer"}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "AT");
        assert_eq!(resp.expires_in, 86400);
    }
}
