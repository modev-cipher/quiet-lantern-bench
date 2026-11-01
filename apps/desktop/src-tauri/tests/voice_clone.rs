use std::path::Path;

use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};
use txtcaster_lib::voice_sample;

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

fn write_wav(path: &Path, seconds: f64) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let rate = 24_000u32;
    let frames = (seconds * f64::from(rate)) as u32;
    let data_len = frames * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend(vec![0u8; data_len as usize]);
    std::fs::write(path, out).expect("write wav");
}

fn cloned_profile(data_dir: &Path, id: &str) -> voice::VoiceProfile {
    let reference = voice_sample::reference_path(data_dir, id);
    write_wav(&reference, 5.0);
    voice::VoiceProfile {
        id: id.to_string(),
        name: "My voice".into(),
        engine: "chatterbox-0.5b".into(),
        kind: "cloned".into(),
        preset_voice_id: None,
        reference_wav_path: Some(reference.to_string_lossy().into_owned()),
        params_json: Some(r#"{"ref_sha256":"abc"}"#.into()),
        created_at: now_ms(),
    }
}

async fn seed_job(pool: &SqlitePool, voice_profile_id: &str) -> String {
    let b = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(1),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: new_id(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &b).await.expect("book");
    let job_id = new_id();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"book"}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: voice_profile_id.into(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "QUEUED".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: Some(job::default_tts_seed(voice_profile_id, &b.content_hash)),
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &record).await.expect("job");
    job_id
}

#[tokio::test]
async fn a_saved_reference_moves_inside_the_voices_tree_so_the_rename_cannot_tear() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    let sample_id = new_id();
    let staged = voice_sample::staging_path(data, &sample_id);
    write_wav(&staged, 5.0);

    let profile_id = new_id();
    let target = voice_sample::reference_path(data, &profile_id);
    std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&staged, &target).expect("rename must stay on one filesystem");

    assert!(target.is_file());
    assert!(!staged.exists());
    assert!(target.starts_with(voice_sample::voices_dir(data)));
}

#[tokio::test]
async fn startup_discards_unsaved_recordings_but_keeps_saved_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    let orphan = voice_sample::staging_path(data, &new_id());
    write_wav(&orphan, 5.0);
    let kept = voice_sample::reference_path(data, "voice-1");
    write_wav(&kept, 5.0);

    assert_eq!(voice_sample::sweep_staging(data), 1);
    assert!(!orphan.exists());
    assert!(kept.is_file());
    assert_eq!(voice_sample::sweep_staging(data), 0);
}

#[tokio::test]
async fn a_cloned_profile_round_trips_with_its_reference() {
    let (dir, pool) = test_pool().await;
    let profile = cloned_profile(dir.path(), &new_id());
    voice::insert_voice_profile(&pool, &profile)
        .await
        .expect("insert");
    let loaded = voice::get_voice_profile(&pool, &profile.id)
        .await
        .expect("get")
        .expect("profile");
    assert_eq!(loaded.kind, "cloned");
    assert_eq!(loaded.preset_voice_id, None);
    assert_eq!(loaded.reference_wav_path, profile.reference_wav_path);
    assert!(Path::new(loaded.reference_wav_path.as_deref().unwrap()).is_file());
}

#[tokio::test]
async fn a_voice_with_no_jobs_reports_zero_users() {
    let (dir, pool) = test_pool().await;
    let profile = cloned_profile(dir.path(), &new_id());
    voice::insert_voice_profile(&pool, &profile)
        .await
        .expect("insert");
    assert_eq!(
        job::count_jobs_for_voice_profile(&pool, &profile.id)
            .await
            .expect("count"),
        0
    );
}

#[tokio::test]
async fn a_voice_in_use_reports_every_job_that_would_lose_its_audio() {
    let (dir, pool) = test_pool().await;
    let profile = cloned_profile(dir.path(), &new_id());
    voice::insert_voice_profile(&pool, &profile)
        .await
        .expect("insert");
    seed_job(&pool, &profile.id).await;
    seed_job(&pool, &profile.id).await;
    assert_eq!(
        job::count_jobs_for_voice_profile(&pool, &profile.id)
            .await
            .expect("count"),
        2
    );

    let other = cloned_profile(dir.path(), &new_id());
    voice::insert_voice_profile(&pool, &other)
        .await
        .expect("insert other");
    assert_eq!(
        job::count_jobs_for_voice_profile(&pool, &other.id)
            .await
            .expect("count"),
        0,
        "job counts must not leak across voices"
    );
}

#[tokio::test]
async fn deleting_a_voice_removes_its_row_and_leaves_the_others_alone() {
    let (dir, pool) = test_pool().await;
    let doomed = cloned_profile(dir.path(), &new_id());
    let kept = cloned_profile(dir.path(), &new_id());
    voice::insert_voice_profile(&pool, &doomed)
        .await
        .expect("insert");
    voice::insert_voice_profile(&pool, &kept)
        .await
        .expect("insert");

    voice::delete_voice_profile(&pool, &doomed.id)
        .await
        .expect("delete");
    std::fs::remove_dir_all(voice_sample::profile_dir(dir.path(), &doomed.id)).expect("rmdir");

    assert!(voice::get_voice_profile(&pool, &doomed.id)
        .await
        .expect("get")
        .is_none());
    assert!(!voice_sample::profile_dir(dir.path(), &doomed.id).exists());
    assert!(voice_sample::reference_path(dir.path(), &kept.id).is_file());
}

#[tokio::test]
async fn the_reference_a_job_points_at_survives_a_startup_sweep() {
    let (dir, pool) = test_pool().await;
    let profile = cloned_profile(dir.path(), &new_id());
    voice::insert_voice_profile(&pool, &profile)
        .await
        .expect("insert");
    seed_job(&pool, &profile.id).await;

    voice_sample::sweep_staging(dir.path());
    let loaded = voice::get_voice_profile(&pool, &profile.id)
        .await
        .expect("get")
        .expect("profile");
    assert!(Path::new(loaded.reference_wav_path.as_deref().unwrap()).is_file());
}

fn ffmpeg_available() -> bool {
    let binary = std::env::var("TXTCASTER_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    std::process::Command::new(binary)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

async fn synthesize_speechlike(target: &Path, seconds: f64) {
    let binary = std::env::var("TXTCASTER_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let duration = seconds.to_string();
    let target_arg = target.to_string_lossy().to_string();
    let status = tokio::process::Command::new(binary)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=180:sample_rate=44100",
            "-t",
            &duration,
            "-af",
            "tremolo=f=6:d=0.8,volume=0.4",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
            &target_arg,
        ])
        .status()
        .await
        .expect("spawn ffmpeg");
    assert!(status.success(), "fixture generation failed");
}

#[tokio::test]
async fn conditioning_produces_the_24k_mono_reference_the_backend_expects() {
    if !ffmpeg_available() {
        eprintln!("SKIP: ffmpeg is not on PATH; conditioning chain not exercised");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.wav");
    synthesize_speechlike(&source, 6.0).await;

    let target = voice_sample::staging_path(dir.path(), "sample-1");
    let seconds = voice_sample::condition_into(&source, &target)
        .await
        .expect("conditioning");

    assert!(target.is_file());
    assert!(
        (seconds - 6.0).abs() < 0.5,
        "expected about 6s of speech, measured {seconds}"
    );

    let bytes = std::fs::read(&target).expect("read");
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    assert_eq!(channels, 1, "the reference must be mono");
    assert_eq!(
        rate, 24_000,
        "the reference must match the model sample rate"
    );
    assert_eq!(bits, 16, "the reference must be s16le");
}

#[tokio::test]
async fn conditioning_refuses_a_sample_shorter_than_the_cloning_floor() {
    if !ffmpeg_available() {
        eprintln!("SKIP: ffmpeg is not on PATH; duration floor not exercised");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.wav");
    synthesize_speechlike(&source, 1.2).await;

    let target = voice_sample::staging_path(dir.path(), "sample-short");
    let err = voice_sample::condition_into(&source, &target)
        .await
        .expect_err("a 1.2s sample must be refused");
    let message = err.to_string();
    assert!(message.contains("at least 3 seconds"), "{message}");
    assert!(
        !target.exists(),
        "a refused sample must not be left in staging"
    );
}

#[tokio::test]
async fn conditioning_caps_a_long_sample_at_the_window_the_model_uses() {
    if !ffmpeg_available() {
        eprintln!("SKIP: ffmpeg is not on PATH; duration cap not exercised");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.wav");
    synthesize_speechlike(&source, 30.0).await;

    let target = voice_sample::staging_path(dir.path(), "sample-long");
    let seconds = voice_sample::condition_into(&source, &target)
        .await
        .expect("conditioning");
    assert!(
        seconds <= voice_sample::MAX_SECONDS + 0.2,
        "a 30s sample must be capped at {}s, got {seconds}",
        voice_sample::MAX_SECONDS
    );
}

#[tokio::test]
async fn conditioning_rejects_a_file_that_is_not_audio() {
    if !ffmpeg_available() {
        eprintln!("SKIP: ffmpeg is not on PATH; decode failure not exercised");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("notes.txt");
    std::fs::write(&source, b"this is not audio at all").expect("write");

    let target = voice_sample::staging_path(dir.path(), "sample-bad");
    let err = voice_sample::condition_into(&source, &target)
        .await
        .expect_err("a text file must be refused");
    assert!(err.to_string().contains("not audio txtCaster can read"));
    assert!(!target.exists());
}
