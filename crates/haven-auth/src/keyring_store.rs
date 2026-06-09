//! Token persistence in the OS keyring (macOS Keychain / Secret Service via the
//! `keyring` crate). Tokens are never written to `config.toml` or the DB
//! (SPEC §6). Keyring access is **written, not live-verified** in CI.

use keyring::Entry;

use crate::{AuthError, Result, Tokens};

const SERVICE: &str = "haven";
const ACCOUNT: &str = "auth0";

/// Handle to the keyring entry holding the Haven token set.
pub struct TokenStore {
    service: String,
    account: String,
}

impl Default for TokenStore {
    fn default() -> Self {
        TokenStore {
            service: SERVICE.into(),
            account: ACCOUNT.into(),
        }
    }
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self) -> Result<Entry> {
        Ok(Entry::new(&self.service, &self.account)?)
    }

    /// Persist the token set (serialized as JSON).
    pub fn save(&self, tokens: &Tokens) -> Result<()> {
        let json = serde_json::to_string(tokens)?;
        self.entry()?.set_password(&json)?;
        Ok(())
    }

    /// Load the stored token set, or `None` if not logged in.
    pub fn load(&self) -> Result<Option<Tokens>> {
        match self.entry()?.get_password() {
            Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AuthError::Keyring(e)),
        }
    }

    /// Clear stored tokens (sign-out). Non-destructive to local data (SPEC §6).
    /// Missing credentials are treated as already-cleared.
    pub fn clear(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AuthError::Keyring(e)),
        }
    }
}
