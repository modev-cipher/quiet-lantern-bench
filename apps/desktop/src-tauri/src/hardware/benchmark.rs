use std::io::Write;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::sidecar::client::post_json;

pub const DISK_PROBE_BYTES: usize = 100 * 1024 * 1024;
const DISK_CHUNK: usize = 1024 * 1024;
const PROBE_MAX_TOKENS: i64 = 64;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskProbe {
    pub write_mb_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LlmProbeResult {
    pub device: String,
    pub tok_s: f64,
    pub tokens: i64,
    pub elapsed_s: f64,
}

pub fn disk_probe(dir: &Path) -> Option<DiskProbe> {
    let path = dir.join(".txtcaster-diskprobe.tmp");
    let chunk = vec![0u8; DISK_CHUNK];
    let started = Instant::now();
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&path)?;
        let mut written = 0usize;
        while written < DISK_PROBE_BYTES {
            file.write_all(&chunk)?;
            written += chunk.len();
        }
        file.sync_all()
    })();
    let elapsed = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(&path);
    result.ok()?;
    if elapsed <= 0.0 {
        return None;
    }
    let mb = DISK_PROBE_BYTES as f64 / (1024.0 * 1024.0);
    Some(DiskProbe {
        write_mb_s: mb / elapsed,
    })
}

pub async fn probe_llm_tok_s(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    model_path: &str,
    resource_plan: serde_json::Value,
    threads: Option<u32>,
) -> Result<LlmProbeResult, String> {
    let body = json!({
        "model_path": model_path,
        "resource_plan": resource_plan,
        "threads": threads,
        "max_tokens": PROBE_MAX_TOKENS,
    });
    post_json(client, port, token, "/benchmark/llm", &body)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_probe_measures_positive_throughput() {
        let dir = tempfile::tempdir().unwrap();
        let probe = disk_probe(dir.path()).expect("probe succeeds");
        assert!(probe.write_mb_s > 0.0);
        let leftover = dir.path().join(".txtcaster-diskprobe.tmp");
        assert!(!leftover.exists(), "probe file cleaned up");
    }

    #[test]
    fn disk_probe_returns_none_on_bad_dir() {
        let probe = disk_probe(Path::new("/nonexistent/txtcaster/probe/dir"));
        assert!(probe.is_none());
    }
}
