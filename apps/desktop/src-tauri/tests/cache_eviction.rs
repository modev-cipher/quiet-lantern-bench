use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures_util::future::BoxFuture;
use txtcaster_lib::cache_gc::{self, EvictionOutcome};
use txtcaster_lib::orchestrator::{
    residency_check, terminal_states_csv, JobOrchestrator, JobState, JobUpdate, ProgressEvent,
    ProgressSink, Stage, StageContext, StageOutcome, StageRegistry, StartOptions, UpdateSink,
    ALL_STATES,
};

fn test_options() -> StartOptions {
    StartOptions {
        vram_sampler: residency_check::disabled_sampler(),
        ..StartOptions::default()
    }
}

use txtcaster_lib::storage::{self, book, cache, job, new_id, now_ms, settings, voice, SqlitePool};

struct Scratch {
    _dir: tempfile::TempDir,
    cache_dir: PathBuf,
    pool: SqlitePool,
}

async fn scratch() -> Scratch {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).expect("cache dir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    Scratch {
        _dir: dir,
        cache_dir,
        pool,
    }
}

fn artifact_path(cache_dir: &Path, hash: &str) -> PathBuf {
    cache_dir.join(&hash[..2]).join(hash)
}

fn write_artifact(cache_dir: &Path, hash: &str, bytes: usize) -> i64 {
    let path = artifact_path(cache_dir, hash);
    std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
    std::fs::write(&path, vec![b'x'; bytes]).expect("write artifact");
    std::fs::metadata(&path).expect("metadata").len() as i64
}

fn backdate(path: &Path, age: Duration) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open for backdating");
    file.set_modified(SystemTime::now() - age)
        .expect("backdate mtime");
}

fn cache_dir_bytes(cache_dir: &Path) -> i64 {
    fn walk(dir: &Path, total: &mut i64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, total);
            } else if let Ok(meta) = std::fs::metadata(&path) {
                *total += meta.len() as i64;
            }
        }
    }
    let mut total = 0;
    walk(cache_dir, &mut total);
    total
}

async fn set_cap(pool: &SqlitePool, cap: i64) {
    settings::set_setting(pool, cache_gc::CAP_SETTING_KEY, &cap.to_string())
        .await
        .expect("set cap");
}

async fn seed_book_and_voice(pool: &SqlitePool) -> (String, String) {
    let record = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(10),
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
    book::insert_book(pool, &record).await.expect("insert book");
    let profile = voice::VoiceProfile {
        id: new_id(),
        name: "Default".into(),
        engine: "kokoro-82m".into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_heart".into()),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("insert voice");
    (record.id, profile.id)
}

async fn insert_job_in_state(pool: &SqlitePool, state: JobState) -> String {
    let (book_id, voice_profile_id) = seed_book_and_voice(pool).await;
    insert_job_for_book(pool, state, &book_id, &voice_profile_id).await
}

async fn insert_job_for_book(
    pool: &SqlitePool,
    state: JobState,
    book_id: &str,
    voice_profile_id: &str,
) -> String {
    let book_id = book_id.to_string();
    let voice_profile_id = voice_profile_id.to_string();
    let now = now_ms();
    let record = job::RenderJob {
        id: new_id(),
        book_id,
        scope_json: serde_json::json!({"kind":"pages","page_start":1,"page_end":10}).to_string(),
        mode: "narrator".into(),
        voice_profile_id,
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: state.as_str().to_string(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now,
        updated_at: now,
    };
    job::insert_job(pool, &record).await.expect("insert job");
    record.id
}

async fn record(pool: &SqlitePool, hash: &str, bytes: i64, job_id: Option<&str>) {
    cache::record_use(pool, hash, "wav", bytes, job_id)
        .await
        .expect("record use");
    tokio::time::sleep(Duration::from_millis(3)).await;
}

#[tokio::test]
async fn the_cache_stays_under_the_cap_across_repeated_renders() {
    let scratch = scratch().await;
    const ARTIFACT_BYTES: usize = 4096;
    const CAP: i64 = 12 * ARTIFACT_BYTES as i64;
    set_cap(&scratch.pool, CAP).await;

    for round in 0..4u32 {
        for index in 0..8u32 {
            let hash = format!("{:064x}", round * 100 + index);
            let bytes = write_artifact(&scratch.cache_dir, &hash, ARTIFACT_BYTES);
            assert_eq!(bytes, ARTIFACT_BYTES as i64);
            record(&scratch.pool, &hash, bytes, None).await;
        }
        let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
            .await
            .expect("enforce");
        let accounted = cache::total_bytes(&scratch.pool).await.expect("total");
        let on_disk = cache_dir_bytes(&scratch.cache_dir);
        assert!(
            accounted <= CAP,
            "round {round}: accounted {accounted} exceeds cap {CAP}"
        );
        assert_eq!(
            on_disk, accounted,
            "round {round}: on-disk bytes must match the accounted bytes"
        );
        assert!(!outcome.over_cap(), "round {round}: still over cap");
    }

    let survivors = cache::lru_evictable(&scratch.pool, 100, &terminal_states_csv())
        .await
        .expect("survivors");
    assert_eq!(survivors.len(), 12);
    for survivor in &survivors {
        assert!(
            artifact_path(&scratch.cache_dir, &survivor.hash).is_file(),
            "surviving row {} lost its file",
            survivor.hash
        );
    }
}

#[tokio::test]
async fn an_artifact_referenced_by_a_job_that_can_still_run_is_never_evicted() {
    let scratch = scratch().await;
    let job_id = insert_job_in_state(&scratch.pool, JobState::Synthesizing).await;
    let pinned = format!("{:064x}", 0xaa);
    let evictable = format!("{:064x}", 0xbb);
    let pinned_bytes = write_artifact(&scratch.cache_dir, &pinned, 4096);
    let evictable_bytes = write_artifact(&scratch.cache_dir, &evictable, 4096);
    record(&scratch.pool, &pinned, pinned_bytes, Some(&job_id)).await;
    record(&scratch.pool, &evictable, evictable_bytes, None).await;
    set_cap(&scratch.pool, 1).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.pinned_bytes, pinned_bytes);
    assert!(outcome.over_cap());
    assert!(
        artifact_path(&scratch.cache_dir, &pinned).is_file(),
        "the artifact of a job that can still run was unlinked"
    );
    assert!(cache::get_entry(&scratch.pool, &pinned)
        .await
        .expect("get pinned")
        .is_some());
    assert!(!artifact_path(&scratch.cache_dir, &evictable).exists());
    assert!(cache::get_entry(&scratch.pool, &evictable)
        .await
        .expect("get evictable")
        .is_none());
}

#[tokio::test]
async fn a_reference_from_a_finished_job_does_not_pin_its_artifact() {
    let scratch = scratch().await;
    let job_id = insert_job_in_state(&scratch.pool, JobState::Done).await;
    let hash = format!("{:064x}", 0xcc);
    let bytes = write_artifact(&scratch.cache_dir, &hash, 4096);
    record(&scratch.pool, &hash, bytes, Some(&job_id)).await;
    set_cap(&scratch.pool, 1).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.pinned_bytes, 0);
    assert!(!outcome.over_cap());
    assert!(!artifact_path(&scratch.cache_dir, &hash).exists());
}

#[tokio::test]
async fn a_file_that_cannot_be_unlinked_keeps_its_row() {
    let scratch = scratch().await;
    let stuck = format!("{:064x}", 0xdd);
    let clean = format!("{:064x}", 0xee);
    let clean_bytes = write_artifact(&scratch.cache_dir, &clean, 4096);
    let stuck_path = artifact_path(&scratch.cache_dir, &stuck);
    std::fs::create_dir_all(&stuck_path).expect("stuck dir");
    std::fs::write(stuck_path.join("occupied"), b"held open").expect("occupant");
    record(&scratch.pool, &stuck, 4096, None).await;
    record(&scratch.pool, &clean, clean_bytes, None).await;
    set_cap(&scratch.pool, 1).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.unlink_failed, 1);
    assert_eq!(outcome.evicted, 1);
    assert!(stuck_path.exists(), "the fixture must still be undeletable");
    assert!(
        cache::get_entry(&scratch.pool, &stuck)
            .await
            .expect("get stuck")
            .is_some(),
        "a file that is still on disk must keep a visible row"
    );
    assert!(!artifact_path(&scratch.cache_dir, &clean).exists());
}

#[tokio::test]
async fn an_entry_whose_file_is_already_gone_loses_its_row() {
    let scratch = scratch().await;
    let hash = format!("{:064x}", 0xf1);
    record(&scratch.pool, &hash, 4096, None).await;
    set_cap(&scratch.pool, 1).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.unlink_failed, 0);
    assert_eq!(cache::total_bytes(&scratch.pool).await.expect("total"), 0);
}

#[tokio::test]
async fn a_preview_is_swept_once_its_source_artifact_is_gone() {
    let scratch = scratch().await;
    let kept = format!("{:064x}", 0x11);
    let dropped = format!("{:064x}", 0x22);
    let kept_bytes = write_artifact(&scratch.cache_dir, &kept, 2048);
    let dropped_bytes = write_artifact(&scratch.cache_dir, &dropped, 4096);
    record(&scratch.pool, &dropped, dropped_bytes, None).await;
    record(&scratch.pool, &kept, kept_bytes, None).await;
    let preview_dir = scratch.cache_dir.join(cache_gc::PREVIEW_DIR);
    std::fs::create_dir_all(&preview_dir).expect("preview dir");
    let kept_preview = preview_dir.join(format!("{kept}.wav"));
    let dropped_preview = preview_dir.join(format!("{dropped}.wav"));
    std::fs::copy(artifact_path(&scratch.cache_dir, &kept), &kept_preview).expect("copy kept");
    std::fs::copy(
        artifact_path(&scratch.cache_dir, &dropped),
        &dropped_preview,
    )
    .expect("copy dropped");
    backdate(&kept_preview, cache_gc::PREVIEW_GRACE * 2);
    backdate(&dropped_preview, cache_gc::PREVIEW_GRACE * 2);
    set_cap(&scratch.pool, kept_bytes).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.previews_removed, 1);
    assert_eq!(outcome.preview_bytes_freed, dropped_bytes);
    assert!(kept_preview.is_file(), "a live source keeps its preview");
    assert!(
        !dropped_preview.exists(),
        "a preview outlived the artifact it was copied from"
    );
}

#[tokio::test]
async fn the_cap_defaults_to_twenty_gigabytes_and_zero_means_unlimited() {
    let scratch = scratch().await;
    assert_eq!(
        cache_gc::read_cap_bytes(&scratch.pool)
            .await
            .expect("unset"),
        Some(cache_gc::DEFAULT_CAP_BYTES)
    );
    assert_eq!(cache_gc::DEFAULT_CAP_BYTES, 21_474_836_480);

    settings::set_setting(&scratch.pool, cache_gc::CAP_SETTING_KEY, "0")
        .await
        .expect("zero");
    assert_eq!(
        cache_gc::read_cap_bytes(&scratch.pool).await.expect("zero"),
        None
    );

    settings::set_setting(&scratch.pool, cache_gc::CAP_SETTING_KEY, " 4096 ")
        .await
        .expect("padded");
    assert_eq!(
        cache_gc::read_cap_bytes(&scratch.pool)
            .await
            .expect("padded"),
        Some(4096)
    );

    settings::set_setting(&scratch.pool, cache_gc::CAP_SETTING_KEY, "twenty gigs")
        .await
        .expect("junk");
    assert_eq!(
        cache_gc::read_cap_bytes(&scratch.pool).await.expect("junk"),
        Some(cache_gc::DEFAULT_CAP_BYTES)
    );
}

#[tokio::test]
async fn an_unlimited_cap_evicts_nothing() {
    let scratch = scratch().await;
    let hash = format!("{:064x}", 0x33);
    let bytes = write_artifact(&scratch.cache_dir, &hash, 4096);
    record(&scratch.pool, &hash, bytes, None).await;
    settings::set_setting(&scratch.pool, cache_gc::CAP_SETTING_KEY, "0")
        .await
        .expect("unlimited");

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome, {
        EvictionOutcome {
            cap_bytes: None,
            bytes_before: bytes,
            bytes_after: bytes,
            ..EvictionOutcome::default()
        }
    });
    assert!(artifact_path(&scratch.cache_dir, &hash).is_file());
}

#[tokio::test]
async fn eviction_is_reported_on_the_job_that_triggered_it() {
    let scratch = scratch().await;
    let job_id = insert_job_in_state(&scratch.pool, JobState::Done).await;
    let hash = format!("{:064x}", 0x44);
    let bytes = write_artifact(&scratch.cache_dir, &hash, 4096);
    record(&scratch.pool, &hash, bytes, None).await;

    set_cap(&scratch.pool, 8192).await;
    cache_gc::enforce_cap_for_job(&scratch.pool, &scratch.cache_dir, &job_id)
        .await
        .expect("quiet pass");
    let quiet = job::get_job(&scratch.pool, &job_id)
        .await
        .expect("get job")
        .expect("job");
    assert_eq!(
        quiet.report_json, None,
        "a pass that evicted nothing must not write a report block"
    );

    set_cap(&scratch.pool, 1).await;
    cache_gc::enforce_cap_for_job(&scratch.pool, &scratch.cache_dir, &job_id)
        .await
        .expect("evicting pass");
    let record = job::get_job(&scratch.pool, &job_id)
        .await
        .expect("get job")
        .expect("job");
    let report: serde_json::Value =
        serde_json::from_str(&record.report_json.expect("report")).expect("json");
    let block = &report["cache_eviction"];
    assert_eq!(block["evicted"], serde_json::json!(1));
    assert_eq!(block["bytes_freed"], serde_json::json!(bytes));
    assert_eq!(block["cap_bytes"], serde_json::json!(1));
    assert_eq!(block["bytes_after"], serde_json::json!(0));
    assert_eq!(block["over_cap"], serde_json::json!(false));
}
#[tokio::test]
async fn a_running_job_evicts_stale_artifacts_at_every_stage_boundary() {
    let scratch = scratch().await;
    let job_id = insert_job_in_state(&scratch.pool, JobState::Paused).await;
    let owned = format!("{:064x}", 0x53);
    let stale_one = format!("{:064x}", 0x51);
    let stale_two = format!("{:064x}", 0x52);
    let owned_bytes = write_artifact(&scratch.cache_dir, &owned, 4096);
    let stale_one_bytes = write_artifact(&scratch.cache_dir, &stale_one, 4096);
    let stale_two_bytes = write_artifact(&scratch.cache_dir, &stale_two, 4096);
    record(&scratch.pool, &owned, owned_bytes, Some(&job_id)).await;
    record(&scratch.pool, &stale_one, stale_one_bytes, None).await;
    record(&scratch.pool, &stale_two, stale_two_bytes, None).await;
    set_cap(&scratch.pool, owned_bytes).await;

    let progress: ProgressSink = Arc::new(|_event: ProgressEvent| {});
    let updates_log = Arc::new(Mutex::new(Vec::<JobUpdate>::new()));
    let sink_log = updates_log.clone();
    let updates: UpdateSink = Arc::new(move |update| sink_log.lock().unwrap().push(update));
    let orchestrator = JobOrchestrator::start_with_options(
        scratch.pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        scratch.cache_dir.clone(),
        test_options(),
    );
    orchestrator.resume_job(&job_id).await.expect("resume job");

    let mut final_record = None;
    for _ in 0..3000 {
        let record = job::get_job(&scratch.pool, &job_id)
            .await
            .expect("get job")
            .expect("job");
        if record.state == JobState::Done.as_str() {
            final_record = Some(record);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let final_record = final_record.expect("job never reached DONE");

    assert!(
        artifact_path(&scratch.cache_dir, &owned).is_file(),
        "the running job's own artifact was evicted at a stage boundary"
    );
    assert!(!artifact_path(&scratch.cache_dir, &stale_one).exists());
    assert!(!artifact_path(&scratch.cache_dir, &stale_two).exists());
    assert_eq!(
        cache::total_bytes(&scratch.pool).await.expect("total"),
        owned_bytes
    );
    let report: serde_json::Value =
        serde_json::from_str(&final_record.report_json.expect("report")).expect("json");
    assert_eq!(
        report["cache_eviction"]["evicted"],
        serde_json::json!(2),
        "eviction must be observable in the job report"
    );
}

#[tokio::test]
async fn a_published_preview_accounts_the_wav_it_leaves_in_the_cache() {
    let scratch = scratch().await;
    let hash = format!("{:064x}", 0x61);
    let bytes = write_artifact(&scratch.cache_dir, &hash, 3072);

    let published = txtcaster_lib::publish_preview(&scratch.pool, &scratch.cache_dir, &hash)
        .await
        .expect("publish preview");

    assert!(Path::new(&published).is_file());
    backdate(Path::new(&published), cache_gc::PREVIEW_GRACE * 2);
    let entry = cache::get_entry(&scratch.pool, &hash)
        .await
        .expect("get entry")
        .expect("a preview must leave an accounted cache entry");
    assert_eq!(entry.bytes, bytes);
    assert_eq!(entry.kind, "wav");
    assert_eq!(
        cache::total_bytes(&scratch.pool).await.expect("total"),
        bytes
    );

    set_cap(&scratch.pool, 1).await;
    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");
    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.previews_removed, 1);
    assert!(!Path::new(&published).exists());
    assert_eq!(cache_dir_bytes(&scratch.cache_dir), 0);
}

#[tokio::test]
async fn a_preview_of_audio_that_never_reached_the_cache_is_refused() {
    let scratch = scratch().await;
    let hash = format!("{:064x}", 0x62);

    let error = txtcaster_lib::publish_preview(&scratch.pool, &scratch.cache_dir, &hash)
        .await
        .expect_err("missing audio must be refused");

    assert_eq!(error, "preview audio missing from cache");
    assert!(cache::get_entry(&scratch.pool, &hash)
        .await
        .expect("get entry")
        .is_none());
}

#[tokio::test]
async fn a_malformed_artifact_key_is_never_unlinked() {
    let scratch = scratch().await;
    let escape = "../../escape";
    assert!(cache_gc::evictable_path(&scratch.cache_dir, escape).is_none());
    assert!(cache_gc::evictable_path(&scratch.cache_dir, "x").is_none());
    assert!(cache_gc::evictable_path(&scratch.cache_dir, "not-hex-at-all").is_none());
    let valid = format!("{:064x}", 0x71);
    assert_eq!(
        cache_gc::evictable_path(&scratch.cache_dir, &valid),
        Some(artifact_path(&scratch.cache_dir, &valid))
    );

    let outside = scratch.cache_dir.parent().expect("parent").join("escape");
    std::fs::write(&outside, b"not ours").expect("write outside");
    cache::upsert_entry(&scratch.pool, escape, "wav", 4096)
        .await
        .expect("upsert");
    set_cap(&scratch.pool, 1).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 0);
    assert_eq!(outcome.unlink_failed, 1);
    assert!(outside.is_file(), "eviction unlinked outside the cache dir");
    assert!(cache::get_entry(&scratch.pool, escape)
        .await
        .expect("get")
        .is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn a_shard_that_cannot_be_read_keeps_its_row() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch().await;
    let hash = format!("{:064x}", 0x72);
    let bytes = write_artifact(&scratch.cache_dir, &hash, 4096);
    record(&scratch.pool, &hash, bytes, None).await;
    let shard = artifact_path(&scratch.cache_dir, &hash)
        .parent()
        .expect("shard")
        .to_path_buf();
    let original = std::fs::metadata(&shard)
        .expect("shard metadata")
        .permissions();
    let mut sealed = original.clone();
    sealed.set_mode(0o500);
    std::fs::set_permissions(&shard, sealed).expect("seal shard");
    if std::fs::write(shard.join("probe"), b"probe").is_ok() {
        std::fs::set_permissions(&shard, original).expect("restore shard");
        return;
    }
    set_cap(&scratch.pool, 1).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir).await;
    std::fs::set_permissions(&shard, original).expect("restore shard");
    let outcome = outcome.expect("enforce");

    assert_eq!(outcome.evicted, 0);
    assert_eq!(outcome.unlink_failed, 1);
    assert!(artifact_path(&scratch.cache_dir, &hash).is_file());
    assert!(
        cache::get_entry(&scratch.pool, &hash)
            .await
            .expect("get")
            .is_some(),
        "a file this pass could not even stat must keep its row"
    );
}

#[tokio::test]
async fn a_drain_larger_than_one_batch_terminates_under_the_cap() {
    let scratch = scratch().await;
    const ROWS: usize = 600;
    for index in 0..ROWS {
        let hash = format!("{index:064x}");
        write_artifact(&scratch.cache_dir, &hash, 64);
        cache::record_use(&scratch.pool, &hash, "wav", 64, None)
            .await
            .expect("record");
    }
    assert_eq!(
        cache::total_bytes(&scratch.pool).await.expect("total"),
        (ROWS * 64) as i64
    );
    set_cap(&scratch.pool, 64).await;

    let mut evicted = 0u64;
    let mut passes = 0u32;
    loop {
        let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
            .await
            .expect("enforce");
        passes += 1;
        assert!(
            outcome.evicted > 0,
            "a pass that starts over the cap has to make progress or the drain cannot terminate"
        );
        evicted += outcome.evicted;
        if !outcome.budget_exhausted {
            break;
        }
        assert!(
            passes < 16,
            "a drain of {ROWS} rows must converge in a bounded number of passes, not {passes}"
        );
    }

    assert_eq!(evicted, (ROWS - 1) as u64);
    assert_eq!(cache::total_bytes(&scratch.pool).await.expect("total"), 64);
    assert_eq!(cache_dir_bytes(&scratch.cache_dir), 64);
}

#[tokio::test]
async fn the_report_accumulates_across_a_job_s_boundaries() {
    let scratch = scratch().await;
    let job_id = insert_job_in_state(&scratch.pool, JobState::Done).await;
    for pass in 0..3u32 {
        let hash = format!("{:064x}", 0x800 + pass);
        let bytes = write_artifact(&scratch.cache_dir, &hash, 4096);
        record(&scratch.pool, &hash, bytes, None).await;
        set_cap(&scratch.pool, 1).await;
        cache_gc::enforce_cap_for_job(&scratch.pool, &scratch.cache_dir, &job_id)
            .await
            .expect("pass");
    }

    let record = job::get_job(&scratch.pool, &job_id)
        .await
        .expect("get job")
        .expect("job");
    let report: serde_json::Value =
        serde_json::from_str(&record.report_json.expect("report")).expect("json");
    let block = &report["cache_eviction"];
    assert_eq!(block["passes"], serde_json::json!(3));
    assert_eq!(
        block["evicted"],
        serde_json::json!(3),
        "a later pass must not hide what earlier passes deleted"
    );
    assert_eq!(block["bytes_freed"], serde_json::json!(3 * 4096));
}

#[tokio::test]
async fn a_preview_written_moments_ago_is_left_alone() {
    let scratch = scratch().await;
    let hash = format!("{:064x}", 0x91);
    let bytes = write_artifact(&scratch.cache_dir, &hash, 2048);
    let published = txtcaster_lib::publish_preview(&scratch.pool, &scratch.cache_dir, &hash)
        .await
        .expect("publish");
    record(&scratch.pool, &hash, bytes, None).await;
    cache::remove(&scratch.pool, &hash).await.expect("remove");

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.previews_removed, 0);
    assert!(
        Path::new(&published).is_file(),
        "a preview the user may still be listening to was swept"
    );

    backdate(Path::new(&published), cache_gc::PREVIEW_GRACE * 2);
    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");
    assert_eq!(outcome.previews_removed, 1);
    assert!(!Path::new(&published).exists());
}

fn backfill_sql() -> String {
    let migration = storage::MIGRATOR
        .iter()
        .find(|migration| migration.sql.contains("INSERT OR IGNORE INTO cache_ref"))
        .expect("no migration backfills cache_ref");
    assert_eq!(migration.version, 16);
    let statements: Vec<&str> = migration
        .sql
        .split(';')
        .filter(|statement| statement.contains("INSERT OR IGNORE INTO cache_ref"))
        .collect();
    assert_eq!(
        statements.len(),
        2,
        "the backfill must cover both the wav and the crop_png reference"
    );
    for state in ALL_STATES {
        for statement in &statements {
            assert_eq!(
                statement.contains(&format!("'{}'", state.as_str())),
                state.is_terminal(),
                "the backfill must treat {} the way lru_evictable does",
                state.as_str()
            );
        }
    }
    statements.join(";")
}

#[tokio::test]
async fn the_migration_pins_the_artifacts_of_jobs_that_predate_the_reference_table() {
    let scratch = scratch().await;
    let (book_id, voice_profile_id) = seed_book_and_voice(&scratch.pool).await;
    let job_id = insert_job_for_book(
        &scratch.pool,
        JobState::Aligning,
        &book_id,
        &voice_profile_id,
    )
    .await;
    let chapter = book::Chapter {
        id: new_id(),
        book_id: book_id.clone(),
        idx: 0,
        title: "One".into(),
        page_start: 1,
        page_end: 10,
        is_pseudo: false,
        word_count: Some(100),
    };
    book::insert_chapter(&scratch.pool, &chapter)
        .await
        .expect("chapter");
    let wav = format!("{:064x}", 0xa1);
    let crop = format!("{:064x}", 0xa2);
    let stale = format!("{:064x}", 0xa3);
    let wav_bytes = write_artifact(&scratch.cache_dir, &wav, 4096);
    let crop_bytes = write_artifact(&scratch.cache_dir, &crop, 4096);
    let stale_bytes = write_artifact(&scratch.cache_dir, &stale, 4096);
    job::insert_segments(
        &scratch.pool,
        &[job::ScriptSegment {
            id: new_id(),
            job_id: job_id.clone(),
            chapter_id: chapter.id.clone(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "hello".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: Some(wav.clone()),
            duration_ms: Some(1000),
            original_text: None,
        }],
    )
    .await
    .expect("segments");
    book::insert_figure_region(
        &scratch.pool,
        &book::FigureRegion {
            id: new_id(),
            book_id: book_id.clone(),
            page_no: 1,
            bbox_json: "[0,0,10,10]".into(),
            kind: "figure".into(),
            confidence: 0.9,
            crop_hash: Some(crop.clone()),
            caption: None,
            explanation: None,
            caption_confidence: None,
        },
    )
    .await
    .expect("figure");
    for (hash, bytes) in [
        (&wav, wav_bytes),
        (&crop, crop_bytes),
        (&stale, stale_bytes),
    ] {
        cache::upsert_entry(&scratch.pool, hash, "wav", bytes)
            .await
            .expect("legacy entry");
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    sqlx::query("DELETE FROM cache_ref")
        .execute(&scratch.pool)
        .await
        .expect("simulate a database that predates the reference table");

    sqlx::raw_sql(&backfill_sql())
        .execute(&scratch.pool)
        .await
        .expect("run the shipped backfill");

    set_cap(&scratch.pool, 1).await;
    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.pinned_bytes, wav_bytes + crop_bytes);
    assert!(
        artifact_path(&scratch.cache_dir, &wav).is_file(),
        "an unfinished job's synthesized audio was evicted after the upgrade"
    );
    assert!(artifact_path(&scratch.cache_dir, &crop).is_file());
    assert!(!artifact_path(&scratch.cache_dir, &stale).exists());
}

struct LitteringStage {
    cache_dir: PathBuf,
    bytes: usize,
    written: Arc<Mutex<Vec<String>>>,
}

impl Stage for LitteringStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, txtcaster_lib::orchestrator::OrchestratorError>> {
        Box::pin(async move {
            let hash = format!("{:064x}", 0xb00 + self.written.lock().unwrap().len());
            write_artifact(&self.cache_dir, &hash, self.bytes);
            cache::record_use(&ctx.pool, &hash, "page_png", self.bytes as i64, None).await?;
            self.written.lock().unwrap().push(hash);
            Ok(StageOutcome::Completed)
        })
    }
}

#[tokio::test]
async fn each_boundary_bounds_the_overshoot_to_one_stage_of_output() {
    const LITTER: usize = 4096;
    let scratch = scratch().await;
    let job_id = insert_job_in_state(&scratch.pool, JobState::Paused).await;
    set_cap(&scratch.pool, LITTER as i64).await;
    let written = Arc::new(Mutex::new(Vec::new()));

    let mut registry = StageRegistry::default();
    for stage in [
        JobState::Ingesting,
        JobState::Layout,
        JobState::Vision,
        JobState::Scripting,
        JobState::Synthesizing,
        JobState::Aligning,
        JobState::Rendering,
    ] {
        registry.register(
            stage,
            Arc::new(LitteringStage {
                cache_dir: scratch.cache_dir.clone(),
                bytes: LITTER,
                written: written.clone(),
            }),
        );
    }
    let progress: ProgressSink = Arc::new(|_event: ProgressEvent| {});
    let updates: UpdateSink = Arc::new(|_update: JobUpdate| {});
    let orchestrator = JobOrchestrator::start_with_options(
        scratch.pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        scratch.cache_dir.clone(),
        test_options(),
    );
    orchestrator.resume_job(&job_id).await.expect("resume");

    let mut final_record = None;
    for _ in 0..3000 {
        let record = job::get_job(&scratch.pool, &job_id)
            .await
            .expect("get job")
            .expect("job");
        if record.state == JobState::Done.as_str() {
            final_record = Some(record);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let final_record = final_record.expect("job never reached DONE");

    let littered = written.lock().unwrap().len();
    assert_eq!(littered, 7, "every stage must have written one artifact");
    assert_eq!(
        cache::total_bytes(&scratch.pool).await.expect("total"),
        LITTER as i64,
        "the cache must end one artifact large, not seven"
    );
    assert_eq!(cache_dir_bytes(&scratch.cache_dir), LITTER as i64);
    let report: serde_json::Value =
        serde_json::from_str(&final_record.report_json.expect("report")).expect("json");
    let block = &report["cache_eviction"];
    assert_eq!(
        block["evicted"],
        serde_json::json!(littered - 1),
        "each boundary must have drained the previous stage's output"
    );
    assert_eq!(
        block["passes"],
        serde_json::json!(littered - 1),
        "eviction must run at every boundary, not once at the end"
    );
}

#[tokio::test]
async fn a_terminal_state_releases_its_artifacts_and_every_other_state_pins_them() {
    for state in ALL_STATES {
        let scratch = scratch().await;
        let job_id = insert_job_in_state(&scratch.pool, state).await;
        let hash = format!("{:064x}", 0xc1);
        let bytes = write_artifact(&scratch.cache_dir, &hash, 4096);
        record(&scratch.pool, &hash, bytes, Some(&job_id)).await;
        set_cap(&scratch.pool, 1).await;

        let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
            .await
            .expect("enforce");

        if state.is_terminal() {
            assert_eq!(
                outcome.evicted,
                1,
                "{} can never run again, so it must not pin anything",
                state.as_str()
            );
            assert_eq!(outcome.pinned_bytes, 0, "{}", state.as_str());
            assert!(!artifact_path(&scratch.cache_dir, &hash).exists());
        } else {
            assert_eq!(
                outcome.evicted,
                0,
                "{} may still run, so its artifact must survive",
                state.as_str()
            );
            assert_eq!(outcome.pinned_bytes, bytes, "{}", state.as_str());
            assert!(artifact_path(&scratch.cache_dir, &hash).is_file());
        }
    }
}

#[tokio::test]
async fn a_failed_job_stops_pinning_the_cache_it_can_no_longer_use() {
    let scratch = scratch().await;
    let (book_id, voice_profile_id) = seed_book_and_voice(&scratch.pool).await;
    let running = insert_job_for_book(
        &scratch.pool,
        JobState::Synthesizing,
        &book_id,
        &voice_profile_id,
    )
    .await;
    let failed =
        insert_job_for_book(&scratch.pool, JobState::Failed, &book_id, &voice_profile_id).await;
    let abandoned = format!("{:064x}", 0xd1);
    let live = format!("{:064x}", 0xd2);
    let abandoned_bytes = write_artifact(&scratch.cache_dir, &abandoned, 4096);
    let live_bytes = write_artifact(&scratch.cache_dir, &live, 4096);
    record(&scratch.pool, &abandoned, abandoned_bytes, Some(&failed)).await;
    record(&scratch.pool, &live, live_bytes, Some(&running)).await;
    set_cap(&scratch.pool, live_bytes).await;

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("enforce");

    assert_eq!(outcome.evicted, 1);
    assert_eq!(outcome.pinned_bytes, live_bytes);
    assert!(!outcome.over_cap(), "the cap must be reachable again");
    assert!(!artifact_path(&scratch.cache_dir, &abandoned).exists());
    assert!(artifact_path(&scratch.cache_dir, &live).is_file());
}

#[test]
fn the_releasing_state_list_is_exactly_the_terminal_states() {
    let csv = terminal_states_csv();
    for state in ALL_STATES {
        let needle = format!(",{},", state.as_str());
        assert_eq!(
            csv.contains(&needle),
            state.is_terminal(),
            "{} is on the wrong side of the pin",
            state.as_str()
        );
    }
}

fn temp_write(cache_dir: &Path, shard: &str, name: &str, bytes: usize) -> PathBuf {
    let path = cache_dir.join(shard).join(name);
    std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
    std::fs::write(&path, vec![b'x'; bytes]).expect("write temp");
    path
}

#[tokio::test]
async fn an_abandoned_atomic_write_older_than_the_grace_window_is_swept() {
    let scratch = scratch().await;
    let orphan = temp_write(&scratch.cache_dir, "ab", ".tmp-abandoned", 64);
    backdate(&orphan, cache_gc::PREVIEW_GRACE + Duration::from_secs(60));

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(outcome.temps_removed, 1);
    assert_eq!(outcome.temp_bytes_freed, 64);
    assert!(!orphan.exists());
}

#[tokio::test]
async fn a_temporary_still_inside_the_grace_window_is_left_for_the_writer_that_owns_it() {
    let scratch = scratch().await;
    let live = temp_write(&scratch.cache_dir, "cd", ".tmp-inflight", 32);

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(
        outcome.temps_removed, 0,
        "the grace window is what makes this sweep safe next to a concurrent atomic write; \
         without it the sweep races the writer between mkstemp and rename"
    );
    assert!(live.exists());
}

#[tokio::test]
async fn the_sweep_leaves_published_artifacts_alone() {
    let scratch = scratch().await;
    let hash = "abc123def456";
    write_artifact(&scratch.cache_dir, hash, 128);
    let published = artifact_path(&scratch.cache_dir, hash);
    backdate(
        &published,
        cache_gc::PREVIEW_GRACE + Duration::from_secs(60),
    );

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(outcome.temps_removed, 0);
    assert!(
        published.exists(),
        "only the .tmp- prefix marks an abandoned write"
    );
}

#[tokio::test]
async fn a_directory_that_is_not_a_shard_is_not_swept() {
    let scratch = scratch().await;
    let stray = temp_write(&scratch.cache_dir, "previews", ".tmp-not-a-shard", 16);
    backdate(&stray, cache_gc::PREVIEW_GRACE + Duration::from_secs(60));

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(
        outcome.temps_removed, 0,
        "shards are two hex characters; sweeping any directory under the cache would reach \
         previews and anything else that lands there later"
    );
    assert!(stray.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_shard_symlinked_outside_the_cache_is_refused() {
    let scratch = scratch().await;
    let outside = scratch
        .cache_dir
        .parent()
        .expect("parent")
        .join("elsewhere");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let victim = outside.join(".tmp-not-ours");
    std::fs::write(&victim, b"precious").expect("victim");
    backdate(&victim, cache_gc::PREVIEW_GRACE + Duration::from_secs(60));
    std::os::unix::fs::symlink(&outside, scratch.cache_dir.join("ef")).expect("symlink shard");

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(outcome.temps_removed, 0);
    assert!(
        victim.exists(),
        "a shard name that canonicalises outside the cache root must be refused, or a symlink \
         turns this sweep into an arbitrary unlink"
    );
}

#[tokio::test]
async fn an_abandoned_chapter_partial_older_than_the_grace_window_is_swept() {
    let scratch = scratch().await;
    let orphan = temp_write(
        &scratch.cache_dir,
        "ab",
        &format!("{}.partial0", "a".repeat(64)),
        64,
    );
    backdate(&orphan, cache_gc::PREVIEW_GRACE + Duration::from_secs(60));

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(outcome.temps_removed, 1);
    assert_eq!(outcome.temp_bytes_freed, 64);
    assert!(!orphan.exists());
}

#[tokio::test]
async fn a_chapter_partial_still_inside_the_grace_window_is_left_for_the_encoder_that_owns_it() {
    let scratch = scratch().await;
    let live = temp_write(
        &scratch.cache_dir,
        "cd",
        &format!("{}.partial2", "b".repeat(64)),
        32,
    );

    let outcome = cache_gc::enforce_cap(&scratch.pool, &scratch.cache_dir)
        .await
        .expect("sweep");

    assert_eq!(outcome.temps_removed, 0);
    assert!(live.exists());
}
