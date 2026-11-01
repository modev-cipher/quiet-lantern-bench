use tokio::sync::{mpsc, watch};

use txtcaster_lib::catalog::bundled_catalog;
use txtcaster_lib::model_manager::{manifest_hash, ModelManager};
use txtcaster_lib::storage::{self, models, now_ms};

#[tokio::test]
#[ignore]
async fn real_gemma_install_topup_updates_manifest_without_redownload() {
    let gguf_src = std::env::var("E2E_GGUF").expect("E2E_GGUF");
    let mmproj_src = std::env::var("E2E_MMPROJ").expect("E2E_MMPROJ");
    let old_sha = std::env::var("E2E_OLD_SHA").expect("E2E_OLD_SHA");

    let base = std::path::Path::new(&gguf_src)
        .parent()
        .expect("parent")
        .to_path_buf();
    let dir = tempfile::tempdir_in(&base).expect("tempdir");
    let models_root = dir.path().join("models");
    let install_dir = models_root.join("llm/gemma4-e4b-q4km");
    std::fs::create_dir_all(&install_dir).expect("mkdir");
    std::fs::hard_link(&gguf_src, install_dir.join("gemma-4-E4B-it-Q4_K_M.gguf"))
        .expect("link gguf");
    std::fs::hard_link(&mmproj_src, install_dir.join("mmproj-F16.gguf")).expect("link mmproj");

    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let stale = models::ModelInstall {
        model_id: "gemma4-e4b-q4km".into(),
        path: "llm/gemma4-e4b-q4km/".into(),
        sha256: old_sha,
        disk_bytes: 4977169568,
        source: "curated".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(&pool, &stale)
        .await
        .expect("seed stale row");

    let entry = bundled_catalog()
        .models
        .iter()
        .find(|e| e.id == "gemma4-e4b-q4km")
        .expect("entry")
        .clone();

    let manager = ModelManager::new(models_root, pool.clone());
    let (tx, mut rx) = mpsc::channel(4096);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    manager
        .download(&entry, tx, cancel_rx)
        .await
        .expect("top-up");

    let mut downloaded = false;
    while let Ok(p) = rx.try_recv() {
        if matches!(
            p.state,
            txtcaster_lib::model_manager::DownloadState::Downloading
        ) {
            downloaded = true;
        }
    }
    assert!(!downloaded, "no network download should have happened");

    let row = models::get_model_install(&pool, "gemma4-e4b-q4km")
        .await
        .expect("query")
        .expect("row");
    assert_eq!(row.sha256, manifest_hash(&entry.download.files));
    assert_eq!(row.disk_bytes as u64, entry.disk_bytes);
    println!("updated manifest sha: {}", row.sha256);
}
