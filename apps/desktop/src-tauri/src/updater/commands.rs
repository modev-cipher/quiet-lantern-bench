//! Tauri binding: state, `update-state` events, the five commands, the scheduler.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use super::{Config, Handoff, Snapshot, Updater};

pub const EVENT: &str = "update-state";

/// Called from `setup`: builds the updater from build-time config and starts
/// the background cadence (first check after a delay, never blocking startup).
pub fn init(app: &AppHandle, data_dir: &std::path::Path) {
    let config = Config::from_env(data_dir, env!("CARGO_PKG_VERSION"));
    let emitter = app.clone();
    let updater = match Updater::new(
        config,
        Box::new(move |snapshot: &Snapshot| {
            let _ = emitter.emit(EVENT, snapshot);
        }),
    ) {
        Ok(u) => Arc::new(u),
        Err(e) => {
            tracing::error!("updater: could not start ({e}); updates disabled this session");
            return;
        }
    };
    app.manage(updater.clone());
    if !updater.config().enabled() {
        tracing::info!("updater: disabled in this build (no public key or url)");
        return;
    }
    let (first, every) = (
        updater.config().first_delay,
        updater.config().check_interval,
    );
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(first).await;
        loop {
            updater.check().await;
            tokio::time::sleep(every).await;
        }
    });
}

fn state(app: &AppHandle) -> Option<Arc<Updater>> {
    app.try_state::<Arc<Updater>>().map(|s| s.inner().clone())
}

#[tauri::command]
pub fn update_status(app: AppHandle) -> Option<Snapshot> {
    state(&app).map(|u| u.snapshot())
}

#[tauri::command]
pub async fn update_check_now(app: AppHandle) -> Option<Snapshot> {
    match state(&app) {
        Some(u) => Some(u.check().await),
        None => None,
    }
}

/// Starts the download in the background and returns at once; progress and
/// the outcome arrive as `update-state` events. On success the process exits
/// (Windows installer relaunches us) or restarts (self-replaced executable).
#[tauri::command]
pub fn update_install(app: AppHandle, updater: State<'_, Arc<Updater>>) -> Snapshot {
    let u = updater.inner().clone();
    tauri::async_runtime::spawn(async move {
        match u.install().await {
            Ok(Handoff::Exit) => app.exit(0),
            Ok(Handoff::Restart) => app.restart(),
            Err(_) => {}
        }
    });
    updater.snapshot()
}

#[tauri::command]
pub fn update_cancel(updater: State<'_, Arc<Updater>>) -> Snapshot {
    updater.cancel();
    updater.snapshot()
}

#[tauri::command]
pub fn update_dismiss(updater: State<'_, Arc<Updater>>) -> Snapshot {
    updater.clear_error();
    updater.dismiss();
    updater.snapshot()
}
