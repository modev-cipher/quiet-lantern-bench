//! Build-time configuration and the snapshot the UI renders.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use super::manifest::url_is_acceptable;
use super::signing::PublicKey;

/// Owner-facing defaults (§0.5). Each is overridable at build time by the env
/// var of the same name; none is read at runtime except the manifest URL, which
/// is safe to override because the pinned key still decides what is trusted.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/torkiv-labs/txtcaster-releases/releases/latest/download/latest.json";
pub const CHECK_INTERVAL_HOURS: u64 = 6;
pub const FIRST_CHECK_DELAY_SECS: u64 = 10;
pub const HTTP_TIMEOUT_SECS: u64 = 30;

#[derive(Clone, Debug)]
pub struct Config {
    pub current_version: String,
    pub manifest_url: Option<reqwest::Url>,
    pub pubkey: Option<PublicKey>,
    pub downloads_dir: PathBuf,
    pub check_interval: Duration,
    pub first_delay: Duration,
    /// `$APPIMAGE` as seen at startup (Linux AppImage runtime sets it).
    pub appimage: Option<String>,
}

impl Config {
    pub fn enabled(&self) -> bool {
        self.pubkey.is_some() && self.manifest_url.is_some()
    }

    /// Production wiring. A missing or malformed key disables the updater
    /// outright: the app never falls back to "trust anything".
    pub fn from_env(data_dir: &std::path::Path, current_version: &str) -> Self {
        let pubkey = match option_env!("TXTCASTER_UPDATE_PUBKEY") {
            None => None,
            Some(text) => match PublicKey::from_base64(text) {
                Ok(k) => Some(k),
                Err(e) => {
                    tracing::error!(
                        "updater: compiled-in public key rejected ({e}); updater disabled"
                    );
                    None
                }
            },
        };
        let url_text = std::env::var("TXTCASTER_UPDATE_MANIFEST_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| option_env!("TXTCASTER_UPDATE_MANIFEST_URL").map(str::to_string))
            .unwrap_or_else(|| DEFAULT_MANIFEST_URL.to_string());
        let manifest_url = match reqwest::Url::parse(url_text.trim()) {
            Ok(u) if url_is_acceptable(&u) => Some(u),
            _ => {
                tracing::error!("updater: manifest url {url_text:?} refused; updater disabled");
                None
            }
        };
        Config {
            current_version: current_version.to_string(),
            manifest_url,
            pubkey,
            downloads_dir: data_dir.join("updates"),
            check_interval: Duration::from_secs(CHECK_INTERVAL_HOURS * 3600),
            first_delay: Duration::from_secs(FIRST_CHECK_DELAY_SECS),
            appimage: std::env::var("APPIMAGE").ok().filter(|s| !s.is_empty()),
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Verifying,
    Installing,
    Paused,
    Failed,
}

/// What the UI sees. Emitted as `update-state` on every transition.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub phase: Phase,
    pub enabled: bool,
    pub current_version: String,
    pub available_version: Option<String>,
    pub notes: Option<String>,
    pub release_url: Option<String>,
    /// "restart" = we install and relaunch; "open" = we can only open the release page.
    pub install_kind: Option<&'static str>,
    pub received: u64,
    pub total: u64,
    pub error_code: Option<&'static str>,
    pub error_message: Option<String>,
    /// Unix seconds of the last completed manifest fetch (success or refusal).
    pub last_checked_unix: Option<u64>,
    /// The last attempt could not reach the origin (silent; Settings may say so).
    pub last_attempt_offline: bool,
    /// "Later" was clicked for `available_version`; the banner hides, Settings does not.
    pub dismissed: bool,
}

impl Snapshot {
    pub fn initial(config: &Config) -> Self {
        Snapshot {
            phase: if config.enabled() {
                Phase::Idle
            } else {
                Phase::Disabled
            },
            enabled: config.enabled(),
            current_version: config.current_version.clone(),
            available_version: None,
            notes: None,
            release_url: None,
            install_kind: None,
            received: 0,
            total: 0,
            error_code: None,
            error_message: None,
            last_checked_unix: None,
            last_attempt_offline: false,
            dismissed: false,
        }
    }
}
