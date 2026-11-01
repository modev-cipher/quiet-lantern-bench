use txtcaster_lib::catalog::{bundled_catalog, Catalog, CatalogEntry};
use txtcaster_lib::model_manager::install_rel_dir;
use txtcaster_lib::storage::{self, models, now_ms, voice, SqlitePool};
use txtcaster_lib::voices::sync_preset_voice_profiles;

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

fn install_kokoro(models_root: &std::path::Path, voices: &[&str]) -> models::ModelInstall {
    let entry = bundled_catalog()
        .models
        .iter()
        .find(|e| e.id == "kokoro-82m")
        .expect("kokoro entry");
    let rel = install_rel_dir(entry);
    let dir = models_root.join(&rel);
    std::fs::create_dir_all(dir.join("voices")).expect("mkdir");
    std::fs::write(dir.join("kokoro-v1_0.pth"), b"stub").expect("write");
    std::fs::write(dir.join("config.json"), b"{}").expect("write");
    for voice in voices {
        std::fs::write(dir.join("voices").join(format!("{voice}.pt")), b"stub")
            .expect("write voice");
    }
    models::ModelInstall {
        model_id: entry.id.clone(),
        path: format!("{rel}/"),
        sha256: "deadbeef".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    }
}

fn with_single_voice_model() -> Catalog {
    let mut catalog = bundled_catalog().clone();
    let entry: CatalogEntry = serde_json::from_value(serde_json::json!({
        "id": "single-voice-en", "task": "tts", "display_name": "Single voice (en)",
        "family": "single", "format": "onnx", "disk_bytes": 2, "min_free_ram_mb": 1024,
        "hybrid_capable": false, "quality_score": 50, "speed_score": 90,
        "capabilities": ["timestamps"], "languages": ["en"],
        "license": {"spdx": "MIT", "commercial_ok": true},
        "download": {"files": [
            {"url": "https://example.invalid/voice.onnx", "sha256": "00", "dest_path": "en-voice.onnx", "size_bytes": 1},
            {"url": "https://example.invalid/voice.onnx.json", "sha256": "00", "dest_path": "en-voice.onnx.json", "size_bytes": 1}
        ]},
        "source": "curated"
    }))
    .expect("synthetic single-voice entry");
    catalog.models.push(entry);
    catalog
}

fn install_entry(
    catalog: &Catalog,
    models_root: &std::path::Path,
    model_id: &str,
    files: &[&str],
) -> models::ModelInstall {
    let entry = catalog
        .models
        .iter()
        .find(|e| e.id == model_id)
        .unwrap_or_else(|| panic!("{model_id} entry"));
    let rel = install_rel_dir(entry);
    let dir = models_root.join(&rel);
    for file in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, b"stub").expect("write");
    }
    models::ModelInstall {
        model_id: entry.id.clone(),
        path: format!("{rel}/"),
        sha256: "deadbeef".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    }
}

#[tokio::test]
async fn syncs_one_profile_for_installed_single_voice_model() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let catalog = with_single_voice_model();
    let install = install_entry(
        &catalog,
        &models_root,
        "single-voice-en",
        &["en-voice.onnx", "en-voice.onnx.json"],
    );
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let synced = sync_preset_voice_profiles(&pool, &catalog, &[install], &models_root)
        .await
        .expect("sync");
    assert_eq!(synced, 1);

    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert_eq!(profiles.len(), 1);
    let profile = &profiles[0];
    assert_eq!(profile.id, "preset-single-en-voice");
    assert_eq!(profile.name, "Single voice (en)");
    assert_eq!(profile.engine, "single-voice-en");
    assert_eq!(profile.kind, "preset");
    assert_eq!(profile.preset_voice_id.as_deref(), Some("en-voice"));
    assert_eq!(profile.reference_wav_path, None);
}

#[tokio::test]
async fn skips_single_voice_model_whose_weights_are_missing_from_disk() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let catalog = with_single_voice_model();
    let install = install_entry(
        &catalog,
        &models_root,
        "single-voice-en",
        &["en-voice.onnx.json"],
    );
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let synced = sync_preset_voice_profiles(&pool, &catalog, &[install], &models_root)
        .await
        .expect("sync");
    assert_eq!(synced, 0);
    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert!(profiles.is_empty());
}

#[tokio::test]
async fn cloning_only_models_get_no_preset_profile() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let install = install_entry(
        bundled_catalog(),
        &models_root,
        "chatterbox-0.5b",
        &["ve.safetensors", "t3_cfg.safetensors", "s3gen.safetensors"],
    );
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let synced = sync_preset_voice_profiles(&pool, bundled_catalog(), &[install], &models_root)
        .await
        .expect("sync");
    assert_eq!(synced, 0);
    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert!(profiles.is_empty());
}

#[tokio::test]
async fn syncs_profiles_for_every_installed_tts_model() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let catalog = with_single_voice_model();
    let kokoro = install_kokoro(&models_root, &["af_heart", "jm_kumo"]);
    let single = install_entry(
        &catalog,
        &models_root,
        "single-voice-en",
        &["en-voice.onnx"],
    );
    models::insert_model_install(&pool, &kokoro)
        .await
        .expect("install kokoro");
    models::insert_model_install(&pool, &single)
        .await
        .expect("install single voice");

    let synced = sync_preset_voice_profiles(&pool, &catalog, &[kokoro, single], &models_root)
        .await
        .expect("sync");
    assert_eq!(synced, 3);

    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    let engines: std::collections::HashSet<&str> =
        profiles.iter().map(|p| p.engine.as_str()).collect();
    assert!(engines.contains("kokoro-82m"));
    assert!(engines.contains("single-voice-en"));
}

#[tokio::test]
async fn syncs_profiles_for_voice_packs_on_disk() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let install = install_kokoro(&models_root, &["af_heart", "jm_kumo", "zf_xiaobei"]);
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let synced = sync_preset_voice_profiles(&pool, bundled_catalog(), &[install], &models_root)
        .await
        .expect("sync");
    assert_eq!(synced, 3);

    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert_eq!(profiles.len(), 3);
    let heart = profiles
        .iter()
        .find(|p| p.id == "preset-kokoro-af-heart")
        .expect("heart profile");
    assert_eq!(heart.name, "Heart · American English (female)");
    assert_eq!(heart.engine, "kokoro-82m");
    assert_eq!(heart.kind, "preset");
    assert_eq!(heart.preset_voice_id.as_deref(), Some("af_heart"));
    let kumo = profiles
        .iter()
        .find(|p| p.id == "preset-kokoro-jm-kumo")
        .expect("kumo profile");
    assert_eq!(kumo.name, "Kumo · Japanese (male)");
}

#[tokio::test]
async fn sync_is_idempotent_and_preserves_existing_rows() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let install = install_kokoro(&models_root, &["af_heart"]);
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let existing = voice::VoiceProfile {
        id: "preset-kokoro-af-heart".into(),
        name: "Kokoro · af_heart".into(),
        engine: "kokoro-82m".into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_heart".into()),
        reference_wav_path: None,
        params_json: None,
        created_at: 42,
    };
    voice::insert_voice_profile(&pool, &existing)
        .await
        .expect("seed");

    sync_preset_voice_profiles(
        &pool,
        bundled_catalog(),
        std::slice::from_ref(&install),
        &models_root,
    )
    .await
    .expect("sync one");
    sync_preset_voice_profiles(&pool, bundled_catalog(), &[install], &models_root)
        .await
        .expect("sync two");

    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name, "Kokoro · af_heart");
    assert_eq!(profiles[0].created_at, 42);
}

#[tokio::test]
async fn skips_voice_packs_missing_from_disk() {
    let (dir, pool) = test_pool().await;
    let models_root = dir.path().join("models");
    let install = install_kokoro(&models_root, &["af_heart"]);
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let synced = sync_preset_voice_profiles(&pool, bundled_catalog(), &[install], &models_root)
        .await
        .expect("sync");
    assert_eq!(synced, 1);

    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, "preset-kokoro-af-heart");
}

#[tokio::test]
async fn no_profiles_without_installed_model() {
    let (dir, pool) = test_pool().await;
    let synced =
        sync_preset_voice_profiles(&pool, bundled_catalog(), &[], &dir.path().join("models"))
            .await
            .expect("sync");
    assert_eq!(synced, 0);
    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert!(profiles.is_empty());
}

#[tokio::test]
#[ignore]
async fn e2e_real_kokoro_install_exposes_all_voices() {
    let Ok(models_root) = std::env::var("E2E_MODELS_ROOT") else {
        panic!("set E2E_MODELS_ROOT to run");
    };
    let models_root = std::path::PathBuf::from(models_root);
    let (_dir, pool) = test_pool().await;
    let install = models::ModelInstall {
        model_id: "kokoro-82m".into(),
        path: "tts/kokoro-82m/".into(),
        sha256: "real".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");
    let synced = sync_preset_voice_profiles(
        &pool,
        bundled_catalog(),
        std::slice::from_ref(&install),
        &models_root,
    )
    .await
    .expect("sync");
    assert_eq!(synced, 41);
    let profiles = voice::list_voice_profiles(&pool).await.expect("profiles");
    assert_eq!(profiles.len(), 41);
    let languages: std::collections::HashSet<String> = profiles
        .iter()
        .filter_map(|p| p.name.split("· ").nth(1).map(|s| s.to_string()))
        .collect();
    assert_eq!(
        languages.len(),
        4,
        "American and British English, Japanese and Chinese, got {languages:?}"
    );
}
