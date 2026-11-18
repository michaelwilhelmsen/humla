//! GitHub Copilot as a summary and chat provider (#227), reached through the
//! user's Copilot subscription: Humla signs in with its own OAuth app and
//! drives the Copilot CLI through `copilot-sdk`.

pub mod auth;
pub mod cli;
pub mod session;

use std::path::{Path, PathBuf};

use copilot_sdk::Client;

use crate::keychain;

pub use session::Runtime;

/// The Keychain account holding [`auth::Credentials`] as JSON.
pub const CREDENTIALS_ACCOUNT: &str = "copilot_credentials";

/// Used when no model is chosen: Copilot picks one the account may use.
pub const DEFAULT_MODEL: &str = "auto";

pub const SIGNED_OUT: &str = "GitHub Copilot is signed out. Sign in in Settings.";
pub const NO_CLI: &str = "The Copilot CLI isn't installed. Download it in Settings.";

pub fn read_credentials(cache: &keychain::Cache) -> Result<Option<auth::Credentials>, String> {
    Ok(keychain::read(cache, CREDENTIALS_ACCOUNT)?.and_then(|json| serde_json::from_str(&json).ok()))
}

pub fn write_credentials(cache: &keychain::Cache, creds: Option<&auth::Credentials>) -> Result<(), String> {
    let json = creds.map(|c| serde_json::to_string(c).expect("credentials serialize"));
    keychain::write(cache, CREDENTIALS_ACCOUNT, json.as_deref())
}

/// Whether a failed start is answered by downloading the pinned CLI: only when
/// the CLI spoke another protocol and isn't that download already.
pub fn should_replace_cli(e: &anyhow::Error, cli: Option<&Path>, pinned: &Path) -> bool {
    e.downcast_ref::<session::WrongProtocol>().is_some() && cli != Some(pinned)
}

impl Runtime {
    /// A GitHub token good for the next call, refreshed and stored when needed.
    pub async fn token(&self, cache: &keychain::Cache) -> anyhow::Result<String> {
        let _refreshing = self.refreshing.lock().await;
        let before = read_credentials(cache).map_err(anyhow::Error::msg)?;
        let mut creds = before.clone();
        let result = auth::fresh_token(&auth::GitHub, &mut creds, auth::now_ms()).await;
        if creds != before {
            write_credentials(cache, creds.as_ref()).map_err(anyhow::Error::msg)?;
        }
        match result {
            Ok(token) => Ok(token),
            Err(auth::TokenError::SignedOut) => anyhow::bail!(SIGNED_OUT),
            Err(auth::TokenError::Unreachable(e)) => anyhow::bail!("{e}"),
        }
    }

    /// The CLI's path, looked for once per launch and again after a miss.
    pub async fn cli(&self, app_data: &Path) -> Option<PathBuf> {
        let mut known = self.cli_path.lock().await;
        if let Some(p) = known.as_ref().filter(|p| p.is_file()) {
            return Some(p.clone());
        }
        *known = cli::locate(app_data).await;
        known.clone()
    }

    pub async fn forget_cli(&self) {
        *self.cli_path.lock().await = None;
    }

    /// A running client signed in as the user.
    pub async fn ready_client(&self, cache: &keychain::Cache, app_data: &Path) -> anyhow::Result<Client> {
        let token = self.token(cache).await?;
        let cli = self.cli(app_data).await.ok_or_else(|| anyhow::anyhow!(NO_CLI))?;
        self.client(&cli, &token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cli_on_another_protocol_that_isnt_the_download_is_replaced() {
        let pinned = Path::new("/data/copilot/1.0.114/copilot");
        let own = Path::new("/opt/homebrew/bin/copilot");
        let wrong = || anyhow::Error::new(session::WrongProtocol("version mismatch".into()));
        assert!(should_replace_cli(&wrong(), Some(own), pinned));
        assert!(!should_replace_cli(&wrong(), Some(pinned), pinned), "the download itself is never replaced");
        assert!(!should_replace_cli(&anyhow::anyhow!(SIGNED_OUT), Some(own), pinned));
        assert!(!should_replace_cli(&anyhow::anyhow!("startup timeout"), Some(own), pinned));
    }
}
