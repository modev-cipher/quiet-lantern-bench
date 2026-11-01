//! A failed launch still opens the window: it explains the failure instead of the process
//! dying silently, which on Windows would otherwise show nothing at all.

use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StartupFailureCode {
    AlreadyRunning,
    DatabaseDamaged,
    DiskFull,
    NoAccess,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupFailure {
    pub code: StartupFailureCode,
    pub detail: String,
    pub data_dir: Option<PathBuf>,
}

impl StartupFailure {
    pub fn classify(detail: &str, data_dir: Option<PathBuf>) -> Self {
        let lower = detail.to_lowercase();
        let code = if lower.contains("another instance") {
            StartupFailureCode::AlreadyRunning
        } else if lower.contains("not a database")
            || lower.contains("malformed")
            || lower.contains("migration")
        {
            StartupFailureCode::DatabaseDamaged
        } else if lower.contains("no space")
            || lower.contains("disk is full")
            || lower.contains("disk full")
        {
            StartupFailureCode::DiskFull
        } else if lower.contains("permission denied")
            || lower.contains("access is denied")
            || lower.contains("readonly")
            || lower.contains("read-only")
        {
            StartupFailureCode::NoAccess
        } else {
            StartupFailureCode::Unknown
        };
        Self {
            code,
            detail: detail.to_string(),
            data_dir,
        }
    }
}

#[tauri::command]
pub fn startup_failure(app: tauri::AppHandle) -> Option<StartupFailure> {
    use tauri::Manager;
    app.try_state::<StartupFailure>()
        .map(|state| state.inner().clone())
}

#[tauri::command]
pub fn startup_quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_failure_texts_get_a_specific_explanation() {
        let cases = [
            (
                "another instance of txtcaster is already running",
                StartupFailureCode::AlreadyRunning,
            ),
            (
                "error returned from database: (code: 26) file is not a database",
                StartupFailureCode::DatabaseDamaged,
            ),
            (
                "database disk image is malformed",
                StartupFailureCode::DatabaseDamaged,
            ),
            (
                "No space left on device (os error 28)",
                StartupFailureCode::DiskFull,
            ),
            (
                "(code: 13) database or disk is full",
                StartupFailureCode::DiskFull,
            ),
            (
                "Permission denied (os error 13)",
                StartupFailureCode::NoAccess,
            ),
            (
                "attempt to write a readonly database",
                StartupFailureCode::NoAccess,
            ),
            ("failed to bind media server", StartupFailureCode::Unknown),
        ];
        for (text, code) in cases {
            assert_eq!(StartupFailure::classify(text, None).code, code, "{text}");
        }
    }
}
