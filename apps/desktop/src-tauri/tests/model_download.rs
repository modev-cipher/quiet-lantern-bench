#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use txtcaster_lib::catalog::{
    CatalogEntry, Download, DownloadFile, License, ModelFormat, ModelSource, ModelTask,
};
use txtcaster_lib::model_manager::{
    manifest_hash, DownloadProgress, ModelManager, ModelManagerError,
};
use txtcaster_lib::storage::{self, models, SqlitePool};

struct FileServer {
    port: u16,
    hits: Arc<AtomicUsize>,
}

fn spawn_server(body: Vec<u8>, drop_after: Option<usize>) -> FileServer {
    spawn_server_dropping(body, drop_after, 1)
}

fn spawn_server_dropping(body: Vec<u8>, drop_after: Option<usize>, drop_hits: usize) -> FileServer {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = hits.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let hits = hits_clone.clone();
            tokio::spawn(async move {
                let hit = hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 4096];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&req);
                let range_start = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|r| r.split('-').next())
                    .and_then(|s| s.parse::<usize>().ok());
                let (status, range, start) = match range_start {
                    Some(s) if s < body.len() => (
                        "206 Partial Content",
                        Some(format!("bytes {s}-{}/{}", body.len() - 1, body.len())),
                        s,
                    ),
                    _ => ("200 OK", None, 0),
                };
                let headers: Vec<(&str, &str)> = match range.as_deref() {
                    Some(value) => vec![("Content-Range", value)],
                    None => Vec::new(),
                };
                let mut payload = body[start..].to_vec();
                if let Some(limit) = drop_after {
                    if hit < drop_hits && payload.len() > limit {
                        payload.truncate(limit);
                        let _ = mock_http::write_head(
                            &mut sock,
                            status,
                            &headers,
                            Some(body.len() - start),
                        )
                        .await;
                        let _ = sock.write_all(&payload).await;
                        let _ = sock.flush().await;
                        return;
                    }
                }
                let _ =
                    mock_http::write_head(&mut sock, status, &headers, Some(payload.len())).await;
                let _ = sock.write_all(&payload).await;
                let _ = sock.flush().await;
            });
        }
    });
    FileServer { port, hits }
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn entry_for(port: u16, files: Vec<DownloadFile>) -> CatalogEntry {
    CatalogEntry {
        id: "test-model".into(),
        task: ModelTask::Llm,
        display_name: "Test Model".into(),
        family: "test".into(),
        quant: None,
        format: ModelFormat::Gguf,
        disk_bytes: files.iter().map(|f| f.size_bytes).sum(),
        full_gpu_vram_mb: None,
        min_free_ram_mb: 1000,
        hybrid_capable: false,
        ctx_max: None,
        quality_score: 50,
        speed_score: 50,
        capabilities: vec![],
        languages: vec![],
        license: License {
            spdx: "MIT".into(),
            commercial_ok: true,
        },
        download: Download { files },
        source: ModelSource::Curated,
        evidence: vec![format!("http://127.0.0.1:{port}/")],
        deprecated: false,
        replacement: None,
        perf: None,
    }
}

fn dl_file(port: u16, dest: &str, body: &[u8]) -> DownloadFile {
    DownloadFile {
        url: format!("http://127.0.0.1:{port}/{dest}"),
        sha256: sha256_hex(body),
        dest_path: dest.into(),
        size_bytes: body.len() as u64,
    }
}

async fn test_manager() -> (tempfile::TempDir, ModelManager, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let manager = ModelManager::new(dir.path().join("models"), pool.clone());
    (dir, manager, pool)
}

fn channels() -> (
    mpsc::Sender<DownloadProgress>,
    mpsc::Receiver<DownloadProgress>,
    watch::Sender<bool>,
    watch::Receiver<bool>,
) {
    let (tx, rx) = mpsc::channel(1024);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    (tx, rx, cancel_tx, cancel_rx)
}

#[tokio::test]
async fn full_download_verifies_and_records_install() {
    let body = vec![7u8; 256 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;
    let (tx, mut rx, _cancel_tx, cancel_rx) = channels();

    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("download");

    let installed = dir.path().join("models/llm/test-model/model.gguf");
    assert_eq!(tokio::fs::read(&installed).await.expect("read"), body);
    assert!(!dir
        .path()
        .join("models/llm/test-model/model.gguf.part")
        .exists());

    let row = models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .expect("row");
    assert_eq!(row.path, "llm/test-model/");
    assert_eq!(row.sha256, manifest_hash(&entry.download.files));
    assert_eq!(row.disk_bytes as u64, body.len() as u64);
    assert_eq!(row.source, "curated");

    let mut saw_done = false;
    while let Ok(p) = rx.try_recv() {
        if matches!(p.state, txtcaster_lib::model_manager::DownloadState::Done) {
            saw_done = true;
            assert_eq!(p.bytes_done, p.bytes_total);
        }
    }
    assert!(saw_done);
}

#[tokio::test]
async fn a_dropped_connection_is_resumed_within_the_same_download() {
    let body: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let server = spawn_server(body.clone(), Some(100 * 1024));
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;

    let (tx, mut rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("one dropped connection must not fail the download");

    let installed = dir.path().join("models/llm/test-model/model.gguf");
    assert_eq!(tokio::fs::read(&installed).await.expect("read"), body);
    assert_eq!(
        server.hits.load(Ordering::SeqCst),
        2,
        "exactly one reconnect"
    );
    let mut saw_reconnecting = false;
    while let Ok(event) = rx.try_recv() {
        if event.state == txtcaster_lib::model_manager::DownloadState::Reconnecting {
            saw_reconnecting = true;
            assert!(event.bytes_done > 0, "reconnecting reports the kept bytes");
        }
    }
    assert!(saw_reconnecting, "the retry is reported, not silent");
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_some());
}

#[tokio::test]
async fn a_connection_that_keeps_dropping_gives_up_and_keeps_the_part_file() {
    let body: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let server = spawn_server_dropping(body.clone(), Some(64 * 1024), usize::MAX);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(err.is_transient(), "gave up on a transient failure: {err}");
    assert_eq!(
        server.hits.load(Ordering::SeqCst),
        txtcaster_lib::model_manager::MAX_DOWNLOAD_ATTEMPTS as usize
    );
    let part = dir.path().join("models/llm/test-model/model.gguf.part");
    let part_len = tokio::fs::metadata(&part).await.expect("part kept").len();
    assert!(part_len > 0 && part_len < body.len() as u64);
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_none());

    let healthy = spawn_server(body.clone(), None);
    let entry = entry_for(
        healthy.port,
        vec![dl_file(healthy.port, "model.gguf", &body)],
    );
    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("a later Resume continues from the kept bytes");
    assert_eq!(
        tokio::fs::read(dir.path().join("models/llm/test-model/model.gguf"))
            .await
            .expect("read"),
        body
    );
}

#[tokio::test]
async fn cancelling_while_waiting_to_reconnect_stops_promptly() {
    let body: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let server = spawn_server_dropping(body.clone(), Some(64 * 1024), usize::MAX);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (_dir, manager, _pool) = test_manager().await;
    let (tx, mut rx, cancel_tx, cancel_rx) = channels();
    let watcher = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if event.state == txtcaster_lib::model_manager::DownloadState::Reconnecting {
                let _ = cancel_tx.send(true);
                break;
            }
        }
    });
    let started = std::time::Instant::now();
    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    watcher.await.expect("watcher");
    assert!(matches!(err, ModelManagerError::Cancelled), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

#[tokio::test]
async fn corrupt_body_rejected_with_hash_mismatch_after_retry() {
    let body = vec![1u8; 64 * 1024];
    let server = spawn_server(body.clone(), None);
    let mut file = dl_file(server.port, "model.gguf", &body);
    file.sha256 = "0".repeat(64);
    let entry = entry_for(server.port, vec![file]);
    let (dir, manager, pool) = test_manager().await;
    let (tx, _rx, _cancel_tx, cancel_rx) = channels();

    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(matches!(err, ModelManagerError::HashMismatch { .. }));
    assert!(
        server.hits.load(Ordering::SeqCst) >= 2,
        "expected one retry"
    );
    assert!(!dir
        .path()
        .join("models/llm/test-model/model.gguf.part")
        .exists());
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_none());
}

#[tokio::test]
async fn insufficient_disk_reports_exact_numbers() {
    let server = spawn_server(vec![0u8; 8], None);
    let mut file = dl_file(server.port, "model.gguf", &[0u8; 8]);
    file.size_bytes = u64::MAX / 2;
    let entry = entry_for(server.port, vec![file]);
    let (_dir, manager, _pool) = test_manager().await;
    let (tx, _rx, _cancel_tx, cancel_rx) = channels();

    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    match err {
        ModelManagerError::InsufficientDisk { needed, available } => {
            assert_eq!(needed, u64::MAX / 2);
            assert!(available < needed);
        }
        other => panic!("expected InsufficientDisk, got {other}"),
    }
}

#[tokio::test]
async fn cancel_keeps_part_file_for_later_resume() {
    let body = vec![9u8; 256 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, _pool) = test_manager().await;
    let (tx, _rx, cancel_tx, cancel_rx) = channels();

    cancel_tx.send(true).expect("cancel");
    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(matches!(err, ModelManagerError::Cancelled));

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("resume after cancel");
    let installed = dir.path().join("models/llm/test-model/model.gguf");
    assert_eq!(tokio::fs::read(&installed).await.expect("read"), body);
}

#[tokio::test]
async fn exactly_complete_part_file_is_reused_without_range_request() {
    let body = vec![6u8; 64 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;

    let part = dir.path().join("models/llm/test-model/model.gguf.part");
    tokio::fs::create_dir_all(part.parent().unwrap())
        .await
        .expect("mkdir");
    tokio::fs::write(&part, &body).await.expect("seed part");

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("reuse complete part");

    assert_eq!(server.hits.load(Ordering::SeqCst), 0, "no request expected");
    let installed = dir.path().join("models/llm/test-model/model.gguf");
    assert_eq!(tokio::fs::read(&installed).await.expect("read"), body);
    assert!(!part.exists());
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_some());
}

#[tokio::test]
async fn corrupt_complete_part_file_is_discarded_and_redownloaded() {
    let body = vec![6u8; 64 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, _pool) = test_manager().await;

    let part = dir.path().join("models/llm/test-model/model.gguf.part");
    tokio::fs::create_dir_all(part.parent().unwrap())
        .await
        .expect("mkdir");
    tokio::fs::write(&part, vec![0u8; body.len()])
        .await
        .expect("seed corrupt part");

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("redownload after corrupt part");

    assert!(server.hits.load(Ordering::SeqCst) >= 1);
    let installed = dir.path().join("models/llm/test-model/model.gguf");
    assert_eq!(tokio::fs::read(&installed).await.expect("read"), body);
}

#[tokio::test]
async fn unsafe_entry_id_rejected_before_touching_disk() {
    let body = vec![1u8; 16];
    let server = spawn_server(body.clone(), None);
    let mut entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    entry.id = "../escape".into();
    let (dir, manager, _pool) = test_manager().await;
    let (tx, _rx, _cancel_tx, cancel_rx) = channels();

    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(matches!(err, ModelManagerError::UnsafeModelId(_)));
    assert!(!dir.path().join("models/llm").exists());
}

#[tokio::test]
async fn multi_file_entry_installs_all_files_in_subdirs() {
    let body_a = vec![3u8; 32 * 1024];
    let body_b = vec![4u8; 16 * 1024];
    let server_a = spawn_server(body_a.clone(), None);
    let server_b = spawn_server(body_b.clone(), None);
    let entry = entry_for(
        server_a.port,
        vec![
            dl_file(server_a.port, "model.gguf", &body_a),
            dl_file(server_b.port, "voices/af_heart.bin", &body_b),
        ],
    );
    let (dir, manager, pool) = test_manager().await;
    let (tx, _rx, _cancel_tx, cancel_rx) = channels();

    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("download");

    let root = dir.path().join("models/llm/test-model");
    assert_eq!(
        tokio::fs::read(root.join("model.gguf")).await.expect("a"),
        body_a
    );
    assert_eq!(
        tokio::fs::read(root.join("voices/af_heart.bin"))
            .await
            .expect("b"),
        body_b
    );
    let row = models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .expect("row");
    assert_eq!(row.disk_bytes as u64, (body_a.len() + body_b.len()) as u64);
}

#[tokio::test]
async fn duplicate_download_rejected_and_delete_removes_everything() {
    let body = vec![5u8; 8 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("download");

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(matches!(err, ModelManagerError::AlreadyInstalled(_)));

    assert_eq!(manager.list_installed().await.expect("list").len(), 1);
    manager.delete("test-model").await.expect("delete");
    assert!(!dir.path().join("models/llm/test-model").exists());
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_none());
    let err = manager.delete("test-model").await.unwrap_err();
    assert!(matches!(err, ModelManagerError::NotInstalled(_)));
}

#[tokio::test]
async fn manifest_mismatch_fetches_only_missing_file_and_updates_install() {
    let body_model = vec![6u8; 64 * 1024];
    let body_mmproj = vec![8u8; 16 * 1024];
    let server_model = spawn_server(body_model.clone(), None);
    let server_mmproj = spawn_server(body_mmproj.clone(), None);
    let old_entry = entry_for(
        server_model.port,
        vec![dl_file(server_model.port, "model.gguf", &body_model)],
    );
    let (dir, manager, pool) = test_manager().await;

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&old_entry, tx, cancel_rx)
        .await
        .expect("initial download");
    assert_eq!(server_model.hits.load(Ordering::SeqCst), 1);
    let old_row = models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .expect("row");

    let new_entry = entry_for(
        server_model.port,
        vec![
            dl_file(server_model.port, "model.gguf", &body_model),
            dl_file(server_mmproj.port, "mmproj-F16.gguf", &body_mmproj),
        ],
    );
    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&new_entry, tx, cancel_rx)
        .await
        .expect("top-up download");

    assert_eq!(server_model.hits.load(Ordering::SeqCst), 1);
    assert_eq!(server_mmproj.hits.load(Ordering::SeqCst), 1);
    let root = dir.path().join("models/llm/test-model");
    assert_eq!(
        tokio::fs::read(root.join("mmproj-F16.gguf"))
            .await
            .expect("mmproj"),
        body_mmproj
    );
    let row = models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .expect("row");
    assert_ne!(row.sha256, old_row.sha256);
    assert_eq!(row.sha256, manifest_hash(&new_entry.download.files));
    assert_eq!(
        row.disk_bytes as u64,
        (body_model.len() + body_mmproj.len()) as u64
    );
    assert_eq!(manager.list_installed().await.expect("list").len(), 1);
}

#[tokio::test]
async fn matching_manifest_still_rejected_as_already_installed() {
    let body = vec![5u8; 8 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (_dir, manager, _pool) = test_manager().await;

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("download");

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(matches!(err, ModelManagerError::AlreadyInstalled(_)));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn existing_valid_file_emits_verifying_before_done_without_download() {
    let body = vec![9u8; 24 * 1024 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;

    let dest = dir.path().join("models/llm/test-model/model.gguf");
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .expect("mkdir");
    tokio::fs::write(&dest, &body).await.expect("seed dest");

    let (tx, mut rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("verify existing");

    assert_eq!(server.hits.load(Ordering::SeqCst), 0, "no request expected");
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_some());

    let mut states = Vec::new();
    let mut verify_bytes = Vec::new();
    while let Ok(p) = rx.try_recv() {
        if matches!(
            p.state,
            txtcaster_lib::model_manager::DownloadState::Verifying
        ) {
            verify_bytes.push(p.bytes_done);
        }
        states.push(p.state);
    }
    assert!(
        states
            .iter()
            .any(|s| matches!(s, txtcaster_lib::model_manager::DownloadState::Verifying)),
        "expected verifying events, got {states:?}"
    );
    assert_eq!(verify_bytes[0], 0, "first verifying event announces start");
    assert!(
        verify_bytes.len() >= 2 && verify_bytes.windows(2).all(|w| w[0] <= w[1]),
        "expected increasing hash progress, got {verify_bytes:?}"
    );
    assert!(matches!(
        states.last(),
        Some(txtcaster_lib::model_manager::DownloadState::Done)
    ));
}

#[tokio::test]
async fn fresh_download_emits_initial_downloading_event_before_first_chunk() {
    let body = vec![3u8; 16 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (_dir, manager, _pool) = test_manager().await;

    let (tx, mut rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("download");

    let first = rx.try_recv().expect("first event");
    assert!(matches!(
        first.state,
        txtcaster_lib::model_manager::DownloadState::Downloading
    ));
    assert_eq!(first.bytes_done, 0);
}

#[tokio::test]
async fn cancel_during_verification_aborts_and_keeps_existing_file() {
    let body = vec![4u8; 24 * 1024 * 1024];
    let server = spawn_server(body.clone(), None);
    let entry = entry_for(server.port, vec![dl_file(server.port, "model.gguf", &body)]);
    let (dir, manager, pool) = test_manager().await;

    let dest = dir.path().join("models/llm/test-model/model.gguf");
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .expect("mkdir");
    tokio::fs::write(&dest, &body).await.expect("seed dest");

    let (tx, _rx, cancel_tx, cancel_rx) = channels();
    cancel_tx.send(true).expect("cancel");
    let err = manager.download(&entry, tx, cancel_rx).await.unwrap_err();
    assert!(matches!(err, ModelManagerError::Cancelled));

    assert_eq!(server.hits.load(Ordering::SeqCst), 0, "no request expected");
    assert!(dest.exists(), "existing file must survive cancellation");
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_none());

    let (tx, _rx, _cancel_tx, cancel_rx) = channels();
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("verify succeeds after cancel cleared");
    assert!(models::get_model_install(&pool, "test-model")
        .await
        .expect("query")
        .is_some());
}

#[tokio::test]
async fn retired_figure_models_are_removed_with_their_files_and_others_are_kept() {
    let (dir, manager, pool) = test_manager().await;
    let models_root = dir.path().join("models");
    for (model_id, rel) in [
        ("qwen3-vl-4b-q4km", "vlm/qwen3-vl-4b-q4km/"),
        ("gemma4-e2b-q4km", "llm/gemma4-e2b-q4km/"),
    ] {
        std::fs::create_dir_all(models_root.join(rel)).expect("mkdir");
        std::fs::write(models_root.join(rel).join("w.gguf"), b"stub").expect("write");
        models::insert_model_install(
            &pool,
            &models::ModelInstall {
                model_id: model_id.to_string(),
                path: rel.to_string(),
                sha256: "deadbeef".into(),
                disk_bytes: 4,
                source: "curated".into(),
                installed_at: 0,
            },
        )
        .await
        .expect("install row");
    }

    let removed = manager.purge_retired().await;

    assert_eq!(removed, vec!["qwen3-vl-4b-q4km".to_string()]);
    assert!(!models_root.join("vlm/qwen3-vl-4b-q4km").exists());
    assert!(models_root.join("llm/gemma4-e2b-q4km/w.gguf").exists());
    let left: Vec<String> = models::list_model_installs(&pool)
        .await
        .expect("list")
        .into_iter()
        .map(|i| i.model_id)
        .collect();
    assert_eq!(left, vec!["gemma4-e2b-q4km".to_string()]);
    assert!(manager.purge_retired().await.is_empty(), "idempotent");
}

#[tokio::test]
async fn the_purged_tts_models_are_removed_with_their_files_and_others_are_kept() {
    let (dir, manager, pool) = test_manager().await;
    let models_root = dir.path().join("models");
    for (model_id, rel) in [
        ("xtts-v2", "tts/xtts-v2/"),
        (
            "piper-en-us-lessac-medium",
            "tts/piper-en-us-lessac-medium/",
        ),
        ("orpheus-3b", "tts/orpheus-3b/"),
        ("f5-tts", "tts/f5-tts/"),
        ("kokoro-82m", "tts/kokoro-82m/"),
        ("chatterbox-0.5b", "tts/chatterbox-0.5b/"),
    ] {
        std::fs::create_dir_all(models_root.join(rel)).expect("mkdir");
        std::fs::write(models_root.join(rel).join("w.bin"), b"stub").expect("write");
        models::insert_model_install(
            &pool,
            &models::ModelInstall {
                model_id: model_id.to_string(),
                path: rel.to_string(),
                sha256: "deadbeef".into(),
                disk_bytes: 4,
                source: "curated".into(),
                installed_at: 0,
            },
        )
        .await
        .expect("install row");
    }

    let mut removed = manager.purge_retired().await;
    removed.sort();

    assert_eq!(
        removed,
        vec![
            "f5-tts",
            "orpheus-3b",
            "piper-en-us-lessac-medium",
            "xtts-v2"
        ]
    );
    for gone in [
        "xtts-v2",
        "piper-en-us-lessac-medium",
        "orpheus-3b",
        "f5-tts",
    ] {
        assert!(!models_root.join("tts").join(gone).exists(), "{gone}");
    }
    let mut left: Vec<String> = models::list_model_installs(&pool)
        .await
        .expect("list")
        .into_iter()
        .map(|i| i.model_id)
        .collect();
    left.sort();
    assert_eq!(left, vec!["chatterbox-0.5b", "kokoro-82m"]);
}
