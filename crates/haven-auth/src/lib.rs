//! `haven-auth` — the Auth0 token spine (SPEC §6).
//!
//! CLI-friendly OAuth: the **Device Authorization** flow (no browser plumbing on
//! the box running `haven`). Tokens live in the OS keyring; they auto-refresh
//! ~60s before expiry, and a sync `401` forces a refresh then resumes the pass.
//! Supabase verifies the resulting Auth0 JWT against Auth0's JWKS.
//!
//! ## Verification status
//! The pure logic here (token expiry math, (de)serialization, response parsing,
//! bearer selection) is unit-tested. The network calls and keyring access are
//! **written but not live-verified** — they need a real Auth0 tenant (domain +
//! client id; no API audience needed for the ID-token flow). See the backlog
//! (HV-3) for the live-verify plan.

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
    /// Optional custom-API audience. **Not needed for Supabase third-party
    /// auth**: Haven sends the OIDC ID token (whose `aud` is the client id),
    /// because the `role: authenticated` claim Supabase requires can only ride
    /// the ID token — Auth0 strips non-namespaced custom claims from access
    /// tokens. Kept for setups that do mint a custom-API access token.
    pub audience: Option<String>,
    pub scope: String,
}

impl AuthConfig {
    pub fn new(
        domain: impl Into<String>,
        client_id: impl Into<String>,
        audience: Option<String>,
    ) -> Self {
        AuthConfig {
            domain: domain.into(),
            client_id: client_id.into(),
            audience,
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
    /// The OIDC ID token — what actually goes to Supabase (see
    /// [`Tokens::bearer_token`]). Absent for pasted-token sessions and token
    /// sets stored before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}

impl Tokens {
    /// Build from an Auth0 token response (which carries a relative
    /// `expires_in`), stamping an absolute `expires_at` from `now`.
    ///
    /// Expiry is the **earlier** of `expires_in` (the access token's lifetime)
    /// and the ID token's own `exp` claim — Auth0 configures the two lifetimes
    /// separately, and serving an expired ID token would 401 at Supabase.
    pub fn from_response(resp: TokenResponse, now: u64) -> Self {
        let from_expires_in = (resp.expires_in > 0).then(|| now.saturating_add(resp.expires_in));
        let from_id_token = resp.id_token.as_deref().and_then(jwt_exp);
        let expires_at = match (from_expires_in, from_id_token) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).unwrap_or(now),
        };
        Tokens {
            access_token: resp.access_token,
            id_token: resp.id_token,
            refresh_token: resp.refresh_token,
            expires_at,
        }
    }

    /// True when the token is within `skew` seconds of expiry (or past it).
    pub fn is_expired(&self, now: u64, skew: u64) -> bool {
        now.saturating_add(skew) >= self.expires_at
    }

    /// The token to send as the HTTP Bearer: the **ID token** when present —
    /// it carries the `role: authenticated` claim Supabase requires, which
    /// Auth0 won't put on an access token — else the access token (pasted
    /// `$HAVEN_ACCESS_TOKEN`-style sessions, pre-ID-token stored sets).
    pub fn bearer_token(&self) -> &str {
        self.id_token.as_deref().unwrap_or(&self.access_token)
    }
}

/// Best-effort read of a JWT's `exp` claim, for expiry scheduling only — no
/// signature verification here (Supabase verifies against Auth0's JWKS).
fn jwt_exp(token: &str) -> Option<u64> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims["exp"].as_u64()
}

/// The raw Auth0 `/oauth/token` success payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
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

/// Return a valid bearer token for remote calls — the ID token when present
/// (see [`Tokens::bearer_token`]) — refreshing (and persisting) if it's near
/// expiry. The single entry point the sync layer calls before each pass.
pub async fn current_bearer_token(cfg: &AuthConfig, store: &TokenStore) -> Result<String> {
    let tokens = store.load()?.ok_or(AuthError::NotLoggedIn)?;
    if !tokens.is_expired(now_secs(), REFRESH_SKEW_SECS) {
        return Ok(tokens.bearer_token().to_string());
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
    Ok(fresh.bearer_token().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unsigned JWT whose payload carries the given claims (enough for the
    /// claim-reading helpers, which never verify signatures).
    fn fake_jwt(claims: serde_json::Value) -> String {
        use base64::Engine;
        let enc = |v: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!(
            "{}.{}.sig",
            enc(&serde_json::json!({"alg":"RS256","typ":"JWT"})),
            enc(&claims)
        )
    }

    #[test]
    fn expiry_math_uses_skew() {
        let t = Tokens {
            access_token: "a".into(),
            id_token: None,
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
            id_token: None,
            refresh_token: Some("r".into()),
            expires_in: 3600,
            token_type: "Bearer".into(),
        };
        let t = Tokens::from_response(resp, 1_000_000);
        assert_eq!(t.expires_at, 1_003_600);
        assert_eq!(t.refresh_token.as_deref(), Some("r"));
    }

    #[test]
    fn from_response_expiry_is_the_earlier_of_expires_in_and_id_token_exp() {
        // ID token expires before the access token: its exp must win.
        let resp = TokenResponse {
            access_token: "tok".into(),
            id_token: Some(fake_jwt(serde_json::json!({"exp": 1_002_000u64}))),
            refresh_token: None,
            expires_in: 86_400,
            token_type: "Bearer".into(),
        };
        assert_eq!(Tokens::from_response(resp, 1_000_000).expires_at, 1_002_000);

        // …and vice versa.
        let resp = TokenResponse {
            access_token: "tok".into(),
            id_token: Some(fake_jwt(serde_json::json!({"exp": 2_000_000u64}))),
            refresh_token: None,
            expires_in: 3600,
            token_type: "Bearer".into(),
        };
        assert_eq!(Tokens::from_response(resp, 1_000_000).expires_at, 1_003_600);
    }

    #[test]
    fn bearer_prefers_id_token_and_falls_back_to_access_token() {
        let mut t = Tokens {
            access_token: "AT".into(),
            id_token: Some("IDT".into()),
            refresh_token: None,
            expires_at: 42,
        };
        assert_eq!(t.bearer_token(), "IDT");
        t.id_token = None; // pasted-token / pre-ID-token session
        assert_eq!(t.bearer_token(), "AT");
    }

    #[test]
    fn tokens_round_trip_json_and_read_pre_id_token_sets() {
        let t = Tokens {
            access_token: "a".into(),
            id_token: Some("idt".into()),
            refresh_token: Some("r".into()),
            expires_at: 42,
        };
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Tokens>(&s).unwrap(), t);

        // A token set stored before `id_token` existed must still load.
        let legacy = r#"{"access_token":"a","refresh_token":"r","expires_at":42}"#;
        let t: Tokens = serde_json::from_str(legacy).unwrap();
        assert_eq!(t.id_token, None);
        assert_eq!(t.bearer_token(), "a");
    }

    #[test]
    fn token_response_parses_auth0_shape() {
        let json = r#"{"access_token":"AT","id_token":"IDT","refresh_token":"RT","expires_in":86400,"token_type":"Bearer"}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "AT");
        assert_eq!(resp.id_token.as_deref(), Some("IDT"));
        assert_eq!(resp.expires_in, 86400);

        // id_token absent (no openid scope) must still parse.
        let json = r#"{"access_token":"AT","expires_in":3600,"token_type":"Bearer"}"#;
        assert!(serde_json::from_str::<TokenResponse>(json)
            .unwrap()
            .id_token
            .is_none());
    }

    #[test]
    fn jwt_exp_reads_the_claim_and_tolerates_junk() {
        assert_eq!(
            jwt_exp(&fake_jwt(serde_json::json!({"exp": 123u64}))),
            Some(123)
        );
        assert_eq!(jwt_exp(&fake_jwt(serde_json::json!({"sub": "x"}))), None);
        assert_eq!(jwt_exp("not-a-jwt"), None);
    }
}
