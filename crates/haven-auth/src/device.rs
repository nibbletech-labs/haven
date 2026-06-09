//! Auth0 Device Authorization flow (RFC 8628). `haven auth login` calls
//! [`start`] to get a user code + verification URL, prints them, then [`poll`]s
//! the token endpoint until the user authorizes in a browser.
//!
//! Network paths are **written, not live-verified** (need a real Auth0 tenant).

use std::time::Duration;

use serde::Deserialize;

use crate::{AuthConfig, AuthError, Result, TokenResponse, Tokens};

/// The `/oauth/device/code` response.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// Begin the device flow: request a user code + verification URL.
pub async fn start(cfg: &AuthConfig) -> Result<DeviceAuthorization> {
    let client = reqwest::Client::new();
    let resp = client
        .post(cfg.device_code_url())
        .form(&[
            ("client_id", cfg.client_id.as_str()),
            ("scope", cfg.scope.as_str()),
            ("audience", cfg.audience.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::Auth0(format!(
            "device authorization failed: {body}"
        )));
    }
    Ok(resp.json().await?)
}

/// The error payload returned by the token endpoint while pending.
#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Poll the token endpoint until the user authorizes (or the code expires).
/// Honours `authorization_pending` (keep waiting) and `slow_down` (back off).
pub async fn poll(cfg: &AuthConfig, auth: &DeviceAuthorization) -> Result<Tokens> {
    let client = reqwest::Client::new();
    let mut interval = auth.interval.max(1);
    let mut waited = 0u64;

    loop {
        if waited >= auth.expires_in {
            return Err(AuthError::DeviceFlowTimeout);
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        waited += interval;

        let resp = client
            .post(cfg.token_url())
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", auth.device_code.as_str()),
                ("client_id", cfg.client_id.as_str()),
            ])
            .send()
            .await?;

        if resp.status().is_success() {
            let token: TokenResponse = resp.json().await?;
            return Ok(Tokens::from_response(token, crate::now_secs()));
        }

        // Pending/slow-down/denied/expired are all reported as 4xx + an `error`.
        let err: TokenError = resp.json().await.unwrap_or(TokenError {
            error: "unknown_error".into(),
            error_description: None,
        });
        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                // RFC 8628 §3.5: bump the interval by 5s. Cap it so a server that
                // keeps returning slow_down can't grow the sleep without bound.
                interval = (interval + 5).min(60);
                continue;
            }
            "expired_token" => return Err(AuthError::DeviceFlowTimeout),
            "access_denied" => {
                return Err(AuthError::Denied(
                    err.error_description
                        .unwrap_or_else(|| "access denied".into()),
                ))
            }
            other => return Err(AuthError::Auth0(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_authorization() {
        let json = r#"{
            "device_code":"DC","user_code":"WXYZ-1234",
            "verification_uri":"https://example.auth0.com/activate",
            "verification_uri_complete":"https://example.auth0.com/activate?user_code=WXYZ-1234",
            "expires_in":900,"interval":5
        }"#;
        let auth: DeviceAuthorization = serde_json::from_str(json).unwrap();
        assert_eq!(auth.user_code, "WXYZ-1234");
        assert_eq!(auth.interval, 5);
        assert!(auth.verification_uri_complete.is_some());
    }

    #[test]
    fn interval_defaults_when_absent() {
        let json = r#"{"device_code":"DC","user_code":"U","verification_uri":"https://x","expires_in":600}"#;
        let auth: DeviceAuthorization = serde_json::from_str(json).unwrap();
        assert_eq!(auth.interval, 5);
    }
}
