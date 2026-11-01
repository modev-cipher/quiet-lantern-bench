//! In-app updater (UPD-1). Contract and threat model: docs/notes/upd-1-auto-updater.md §0.
//! The core here is tauri-free so the sabotage suite runs against a loopback
//! fixture origin; `commands.rs` binds it to the app (events, exit, restart).

pub mod commands;
pub mod download;
pub mod error;
mod flow;
pub mod install;
pub mod manifest;
pub mod signing;
pub mod state;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_sabotage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use error::UpdateError;
pub use install::Handoff;
pub use state::{Config, Phase, Snapshot};

use manifest::Manifest;

pub type OnChange = Box<dyn Fn(&Snapshot) + Send + Sync>;

pub struct Updater {
    config: Config,
    client: reqwest::Client,
    inner: Mutex<Inner>,
    cancel: AtomicBool,
    busy: AtomicBool,
    paused_logged: AtomicBool,
    on_change: OnChange,
}

struct Inner {
    snapshot: Snapshot,
    manifest: Option<Manifest>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Updater {
    pub fn new(config: Config, on_change: OnChange) -> Result<Self, UpdateError> {
        let client = reqwest::Client::builder()
            .user_agent(format!("txtCaster/{}", config.current_version))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .redirect(match &config.manifest_url {
                Some(origin) => manifest::redirect_policy(origin),
                None => reqwest::redirect::Policy::none(),
            })
            .build()
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        Ok(Updater {
            inner: Mutex::new(Inner {
                snapshot: Snapshot::initial(&config),
                manifest: None,
            }),
            config,
            client,
            cancel: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            paused_logged: AtomicBool::new(false),
            on_change,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().snapshot.clone()
    }

    fn update<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let (result, snapshot) = {
            let mut inner = self.inner.lock().unwrap();
            let result = f(&mut inner);
            (result, inner.snapshot.clone())
        };
        (self.on_change)(&snapshot);
        result
    }

    fn fail(&self, error: &UpdateError, fallback: Phase) {
        if error.is_silent() {
            tracing::debug!("updater: {error}");
            self.update(|i| {
                i.snapshot.phase = fallback;
                i.snapshot.last_attempt_offline = matches!(error, UpdateError::Offline(_));
            });
            return;
        }
        tracing::warn!("updater: {error}");
        self.update(|i| {
            i.snapshot.phase = Phase::Failed;
            i.snapshot.error_code = Some(error.code());
            i.snapshot.error_message = Some(error.to_string());
            i.snapshot.last_attempt_offline = false;
        });
    }

    /// The user clicked Later: hide the banner for this version, this session.
    pub fn dismiss(&self) {
        self.update(|i| i.snapshot.dismissed = true);
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Acknowledge a failure notice: back to whatever was true before it.
    pub fn clear_error(&self) {
        self.update(|i| {
            if i.snapshot.phase == Phase::Failed {
                i.snapshot.error_code = None;
                i.snapshot.error_message = None;
                i.snapshot.phase = match (&i.manifest, i.snapshot.available_version.is_some()) {
                    (Some(_), true) => Phase::Available,
                    _ => Phase::Idle,
                };
            }
        });
    }
}
