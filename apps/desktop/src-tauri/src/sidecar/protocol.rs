use std::time::Duration;

use serde::Serialize;

const HANDSHAKE_PREFIX: &str = "TXTCASTER_READY";

pub fn parse_handshake(line: &str) -> Option<u16> {
    let rest = line.trim().strip_prefix(HANDSHAKE_PREFIX)?;
    rest.trim().parse().ok()
}

#[derive(Debug, Clone, Copy)]
pub struct BackoffPolicy {
    initial: Duration,
    max: Duration,
    current: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(4))
    }
}

impl BackoffPolicy {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            current: initial,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EngineStatus {
    NotInstalled,
    #[serde(rename_all = "camelCase")]
    NeedsUpdate {
        installed_api: Option<u32>,
        required_api: u32,
    },
    #[serde(rename_all = "camelCase")]
    Downloading {
        bytes_done: u64,
        bytes_total: u64,
        speed_bps: u64,
    },
    #[serde(rename_all = "camelCase")]
    Extracting {
        files_done: usize,
        files_total: usize,
    },
    Starting,
    Ready {
        port: u16,
    },
    Restarting {
        attempt: u32,
    },
    #[serde(rename_all = "camelCase")]
    Failed {
        error_code: String,
        message: String,
    },
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_handshake() {
        assert_eq!(parse_handshake("TXTCASTER_READY 46727"), Some(46727));
        assert_eq!(parse_handshake("  TXTCASTER_READY 8080\n"), Some(8080));
    }

    #[test]
    fn rejects_garbage_lines() {
        assert_eq!(parse_handshake("hello world"), None);
        assert_eq!(parse_handshake("TXTCASTER_READY"), None);
        assert_eq!(parse_handshake("TXTCASTER_READY abc"), None);
        assert_eq!(parse_handshake("TXTCASTER_READY 99999999"), None);
        assert_eq!(parse_handshake(""), None);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut backoff = BackoffPolicy::default();
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
    }

    #[test]
    fn backoff_resets_to_initial() {
        let mut backoff = BackoffPolicy::default();
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn status_serializes_with_tag_and_camel_case() {
        let ready = serde_json::to_value(EngineStatus::Ready { port: 4321 }).unwrap();
        assert_eq!(ready, serde_json::json!({"state": "ready", "port": 4321}));
        let restarting = serde_json::to_value(EngineStatus::Restarting { attempt: 2 }).unwrap();
        assert_eq!(
            restarting,
            serde_json::json!({"state": "restarting", "attempt": 2})
        );
        let starting = serde_json::to_value(EngineStatus::Starting).unwrap();
        assert_eq!(starting, serde_json::json!({"state": "starting"}));
        let not_installed = serde_json::to_value(EngineStatus::NotInstalled).unwrap();
        assert_eq!(not_installed, serde_json::json!({"state": "notInstalled"}));
        let downloading = serde_json::to_value(EngineStatus::Downloading {
            bytes_done: 1024,
            bytes_total: 2048,
            speed_bps: 512,
        })
        .unwrap();
        assert_eq!(
            downloading,
            serde_json::json!({
                "state": "downloading",
                "bytesDone": 1024,
                "bytesTotal": 2048,
                "speedBps": 512,
            })
        );
        let extracting = serde_json::to_value(EngineStatus::Extracting {
            files_done: 5,
            files_total: 10,
        })
        .unwrap();
        assert_eq!(
            extracting,
            serde_json::json!({
                "state": "extracting",
                "filesDone": 5,
                "filesTotal": 10,
            })
        );
        let failed = serde_json::to_value(EngineStatus::Failed {
            error_code: "IO_ERROR".to_string(),
            message: "permission denied".to_string(),
        })
        .unwrap();
        assert_eq!(
            failed,
            serde_json::json!({
                "state": "failed",
                "errorCode": "IO_ERROR",
                "message": "permission denied",
            })
        );
    }
}
