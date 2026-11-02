use txtcaster_lib::orchestrator::terminal_states_csv;
use txtcaster_lib::storage::{
    self, book, cache, chapter_render, export, job, models, new_id, now_ms, paths, settings, voice,
    SqlitePool,
};
use txtcaster_lib::words::WordsJson;

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

/// False when the platform refused to create the link, so a symlink assertion
/// reports a skip instead of passing because nothing was there.
fn link_dir(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn sample_book() -> book::Book {
    book::Book {
        id: new_id(),
        title: "Operating Systems".into(),
        author: Some("A. Tanenbaum".into()),
        source_path: "/books/os.pdf".into(),
        format: "pdf".into(),
        page_count: Some(642),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: "abc123".into(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    }
}

fn sample_voice() -> voice::VoiceProfile {
    voice::VoiceProfile {
        id: new_id(),
        name: "Default".into(),
        engine: "kokoro-82m".into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_heart".into()),
        reference_wav_path: None,
        params_json: Some(r#"{"speed":1.0}"#.into()),
        created_at: now_ms(),
    }
}

#[tokio::test]
async fn migration_applies_on_fresh_db() {
    let (_dir, pool) = test_pool().await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'")
        .fetch_one(&pool)
        .await
        .expect("query");
    assert!(count >= 11);
}

#[tokio::test]
async fn book_round_trip() {
    let (_dir, pool) = test_pool().await;
    let sample = sample_book();
    book::insert_book(&pool, &sample).await.expect("insert");
    let loaded = book::get_book(&pool, &sample.id).await.expect("get");
    assert_eq!(loaded, Some(sample.clone()));
    let all = book::list_books(&pool).await.expect("list");
    assert_eq!(all, vec![sample]);
}

#[tokio::test]
async fn layout_analysed_persists_so_a_declined_book_can_be_improved_later() {
    let (_dir, pool) = test_pool().await;
    let sample = sample_book();
    book::insert_book(&pool, &sample).await.expect("insert");
    assert!(
        !book::get_book(&pool, &sample.id)
            .await
            .expect("get")
            .expect("book")
            .layout_analysed,
        "a book imported without the layout pass starts unanalysed"
    );

    book::set_layout_analysed(&pool, &sample.id, true)
        .await
        .expect("set");
    assert!(
        book::get_book(&pool, &sample.id)
            .await
            .expect("get")
            .expect("book")
            .layout_analysed
    );

    book::set_layout_analysed(&pool, &sample.id, false)
        .await
        .expect("unset");
    assert!(
        !book::get_book(&pool, &sample.id)
            .await
            .expect("get")
            .expect("book")
            .layout_analysed,
        "the flag must be reversible or a failed run could never be retried"
    );
}

#[tokio::test]
async fn chapter_round_trip() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "Introduction".into(),
        page_start: 1,
        page_end: 14,
        is_pseudo: false,
        word_count: Some(4200),
    };
    book::insert_chapter(&pool, &chapter).await.expect("insert");
    let loaded = book::list_chapters(&pool, &b.id).await.expect("list");
    assert_eq!(loaded, vec![chapter]);
}

#[tokio::test]
async fn page_round_trip_and_updates() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let page = book::Page {
        id: new_id(),
        book_id: b.id.clone(),
        page_no: 1,
        width_pt: Some(595.0),
        height_pt: Some(842.0),
        render_hash: None,
        text_json: None,
        needs_upgrade: false,
    };
    book::insert_page(&pool, &page).await.expect("insert");
    let text = r#"[{"bbox":[0,0,10,10],"text":"hi","font_size":11.5,"role":"body","reading_order":0,"column":0}]"#;
    book::set_page_text(&pool, &page.id, text)
        .await
        .expect("text");
    book::set_page_render_hash(&pool, &page.id, "deadbeef")
        .await
        .expect("hash");
    let loaded = book::get_page(&pool, &b.id, 1)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(loaded.text_json.as_deref(), Some(text));
    assert_eq!(loaded.render_hash.as_deref(), Some("deadbeef"));
}

#[tokio::test]
async fn figure_region_round_trip() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let figure = book::FigureRegion {
        id: new_id(),
        book_id: b.id.clone(),
        page_no: 3,
        bbox_json: "[10,20,300,200]".into(),
        kind: "chart".into(),
        confidence: 0.92,
        crop_hash: None,
        caption: None,
        explanation: None,
        caption_confidence: None,
    };
    book::insert_figure_region(&pool, &figure)
        .await
        .expect("insert");
    book::set_figure_vision_output(&pool, &figure.id, "A bar chart", "Shows CPU usage", 0.88)
        .await
        .expect("vision output");
    let loaded = book::list_figures_for_page(&pool, &b.id, 3)
        .await
        .expect("list");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].caption.as_deref(), Some("A bar chart"));
    assert_eq!(loaded[0].explanation.as_deref(), Some("Shows CPU usage"));
    assert_eq!(loaded[0].caption_confidence, Some(0.88));
    assert_eq!(loaded[0].kind, "chart");
}

#[tokio::test]
async fn a_figure_is_never_served_across_a_book_boundary() {
    let (_dir, pool) = test_pool().await;
    let mine = sample_book();
    let mut theirs = sample_book();
    theirs.id = new_id();
    theirs.content_hash = new_id();
    book::insert_book(&pool, &mine).await.expect("my book");
    book::insert_book(&pool, &theirs).await.expect("their book");
    let figure = book::FigureRegion {
        id: new_id(),
        book_id: theirs.id.clone(),
        page_no: 4,
        bbox_json: "[10,20,300,200]".into(),
        kind: "chart".into(),
        confidence: 0.9,
        crop_hash: None,
        caption: None,
        explanation: None,
        caption_confidence: None,
    };
    book::insert_figure_region(&pool, &figure)
        .await
        .expect("insert");

    assert!(
        book::get_figure_region(&pool, &theirs.id, &figure.id)
            .await
            .expect("own book")
            .is_some(),
        "the owning book must still see its own figure"
    );
    assert!(
        book::get_figure_region(&pool, &mine.id, &figure.id)
            .await
            .expect("other book")
            .is_none(),
        "figure ids are only unique per book by convention, so the book predicate belongs in the \
         query rather than in a filter a caller can forget"
    );
}

#[tokio::test]
async fn voice_profile_round_trip() {
    let (_dir, pool) = test_pool().await;
    let profile = sample_voice();
    voice::insert_voice_profile(&pool, &profile)
        .await
        .expect("insert");
    let loaded = voice::get_voice_profile(&pool, &profile.id)
        .await
        .expect("get");
    assert_eq!(loaded, Some(profile.clone()));
    assert_eq!(
        voice::list_voice_profiles(&pool).await.expect("list"),
        vec![profile.clone()]
    );
    voice::delete_voice_profile(&pool, &profile.id)
        .await
        .expect("delete");
    assert_eq!(
        voice::list_voice_profiles(&pool).await.expect("list"),
        vec![]
    );
}

#[tokio::test]
async fn render_job_round_trip_and_updates() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":10}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: v.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: Some(r#"{"model_id":"phi-4-mini","device":"cpu"}"#.into()),
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "QUEUED".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: Some(1200),
        report_json: None,
        output_paths_json: None,
        tts_seed: Some(job::default_tts_seed(&v.id, &b.content_hash)),
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &render_job).await.expect("insert");
    assert_eq!(
        job::get_job(&pool, &render_job.id).await.expect("get"),
        Some(render_job.clone())
    );

    job::update_job_state(&pool, &render_job.id, "SYNTHESIZING")
        .await
        .expect("state");
    let checkpoint = r#"{"stage":"SYNTHESIZING","done_segments":214,"total_segments":890}"#;
    job::update_job_checkpoint(&pool, &render_job.id, checkpoint)
        .await
        .expect("checkpoint");
    job::set_job_report(
        &pool,
        &render_job.id,
        r#"{"cache_hit_ratio":0.0}"#,
        r#"["/out/a.mp3"]"#,
    )
    .await
    .expect("report");

    let loaded = job::get_job(&pool, &render_job.id)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(loaded.state, "SYNTHESIZING");
    assert_eq!(loaded.checkpoint_json.as_deref(), Some(checkpoint));
    assert_eq!(
        loaded.report_json.as_deref(),
        Some(r#"{"cache_hit_ratio":0.0}"#)
    );
    assert_eq!(
        loaded.output_paths_json.as_deref(),
        Some(r#"["/out/a.mp3"]"#)
    );
    assert!(loaded.updated_at >= render_job.updated_at);

    let active = job::list_jobs_in_states(&pool, &["SYNTHESIZING", "QUEUED"])
        .await
        .expect("list");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, render_job.id);
    assert_eq!(active[0].tts_seed, render_job.tts_seed);

    let none = job::list_jobs_in_states(&pool, &[]).await.expect("empty");
    assert_eq!(none, vec![]);

    job::update_job_state(&pool, &render_job.id, "DONE")
        .await
        .expect("done");
    let stale = job::update_job_state_if(&pool, &render_job.id, "SYNTHESIZING", "PAUSED")
        .await
        .expect("conditional");
    assert!(!stale);
    let loaded = job::get_job(&pool, &render_job.id)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(loaded.state, "DONE");
    let matched = job::update_job_state_if(&pool, &render_job.id, "DONE", "PAUSED")
        .await
        .expect("conditional");
    assert!(matched);
}

#[tokio::test]
async fn script_segment_and_alignment_round_trip() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "Ch 1".into(),
        page_start: 1,
        page_end: 10,
        is_pseudo: false,
        word_count: None,
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"chapters","chapter_ids":[]}"#.into(),
        mode: "teacher".into(),
        voice_profile_id: v.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "QUEUED".into(),
        checkpoint_json: None,
        overnight: true,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &render_job).await.expect("job");

    let segments: Vec<job::ScriptSegment> = (0..3)
        .map(|i| job::ScriptSegment {
            id: new_id(),
            job_id: render_job.id.clone(),
            chapter_id: chapter.id.clone(),
            idx: i,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: format!("Segment {i}"),
            source_span_json: Some(r#"{"page":1,"char_start":0,"char_end":9}"#.into()),
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        })
        .collect();
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let loaded = job::list_segments(&pool, &render_job.id)
        .await
        .expect("list");
    assert_eq!(loaded, segments);

    job::set_segment_audio(&pool, &segments[0].id, "wavhash", 3120)
        .await
        .expect("audio");
    let updated = job::list_segments(&pool, &render_job.id)
        .await
        .expect("list");
    assert_eq!(updated[0].audio_hash.as_deref(), Some("wavhash"));
    assert_eq!(updated[0].duration_ms, Some(3120));

    let alignment = job::Alignment {
        segment_id: segments[0].id.clone(),
        source: "tts_native".into(),
        words_json: WordsJson::from_stored(r#"[{"w":"Segment","s":0,"e":400}]"#.into()),
        wer: None,
    };
    job::upsert_alignment(&pool, &alignment)
        .await
        .expect("align");
    let realigned = job::Alignment {
        segment_id: segments[0].id.clone(),
        source: "whisperx".into(),
        words_json: WordsJson::from_stored(r#"[{"w":"Segment","s":10,"e":410}]"#.into()),
        wer: Some(0.02),
    };
    job::upsert_alignment(&pool, &realigned)
        .await
        .expect("realign");
    assert_eq!(
        job::get_alignment(&pool, &segments[0].id)
            .await
            .expect("get"),
        Some(realigned)
    );
}

#[tokio::test]
async fn model_install_round_trip() {
    let (_dir, pool) = test_pool().await;
    let install = models::ModelInstall {
        model_id: "kokoro-82m".into(),
        path: "/models/tts/kokoro-82m".into(),
        sha256: "feed".into(),
        disk_bytes: 82_000_000,
        source: "curated".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(&pool, &install)
        .await
        .expect("insert");
    assert_eq!(
        models::get_model_install(&pool, "kokoro-82m")
            .await
            .expect("get"),
        Some(install.clone())
    );
    assert_eq!(
        models::list_model_installs(&pool).await.expect("list"),
        vec![install]
    );
    models::delete_model_install(&pool, "kokoro-82m")
        .await
        .expect("delete");
    assert_eq!(
        models::list_model_installs(&pool).await.expect("list"),
        vec![]
    );
}

#[tokio::test]
async fn hw_benchmark_singleton_upsert() {
    let (_dir, pool) = test_pool().await;
    let first = models::HwBenchmark {
        profile_json: r#"{"devices":[]}"#.into(),
        compute_score: 120,
        tok_s_gpu: None,
        tok_s_cpu: Some(6.5),
        rtf_table_json: None,
        fingerprint: Some("none|ram:16000|cores:8".into()),
        measured_at: now_ms(),
    };
    models::upsert_hw_benchmark(&pool, &first)
        .await
        .expect("insert");
    let second = models::HwBenchmark {
        profile_json: r#"{"devices":[{"key":"pci:0000:01:00.0"}]}"#.into(),
        compute_score: 420,
        tok_s_gpu: Some(21.5),
        tok_s_cpu: Some(6.5),
        rtf_table_json: Some("{}".into()),
        fingerprint: Some("pci:0000:01:00.0/cuda/12288|ram:16000|cores:8".into()),
        measured_at: now_ms(),
    };
    models::upsert_hw_benchmark(&pool, &second)
        .await
        .expect("upsert");
    let loaded = models::get_hw_benchmark(&pool, "pci:0000:01:00.0/cuda/12288|ram:16000|cores:8")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(loaded.compute_score, second.compute_score);
    assert_eq!(loaded.tok_s_gpu, second.tok_s_gpu);
    assert_eq!(loaded.profile_json, second.profile_json);
    assert_eq!(loaded.fingerprint, second.fingerprint);
    assert_eq!(
        loaded.rtf_table_json, first.rtf_table_json,
        "the benchmark upsert owns the measurement columns; update_learned owns \
         rtf_table_json and an upsert must not carry it"
    );
}

async fn seeded_job(pool: &storage::SqlitePool) -> String {
    let mut b = sample_book();
    b.id = new_id();
    b.content_hash = new_id();
    let mut v = sample_voice();
    v.id = new_id();
    book::insert_book(pool, &b).await.expect("book");
    voice::insert_voice_profile(pool, &v).await.expect("voice");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":10}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: v.id.clone(),
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
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &render_job).await.expect("insert");
    render_job.id
}

#[tokio::test]
async fn a_plan_rewrite_loses_to_a_write_that_landed_first() {
    let (_dir, pool) = test_pool().await;
    let job_id = seeded_job(&pool).await;

    let legacy = r#"{"model_id":"m","device":"cuda"}"#;
    job::set_job_resource_plan(&pool, &job_id, legacy)
        .await
        .expect("seed");

    let degraded = r#"{"model_id":"m","device":"cpu","n_gpu_layers":0}"#;
    job::set_job_resource_plan(&pool, &job_id, degraded)
        .await
        .expect("degrade");

    let upcast = r#"{"model_id":"m","device":{"backend":"cuda","ordinal":0},"plan_revision":2}"#;
    let applied =
        job::rewrite_plan_if_unchanged(&pool, &job_id, job::PlanColumn::Resource, legacy, upcast)
            .await
            .expect("rewrite");

    assert!(
        !applied,
        "the column moved under us, so the rewrite must not land"
    );
    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    assert_eq!(
        record.resource_plan_json.as_deref(),
        Some(degraded),
        "an unguarded rewrite would silently revert a degradation without bumping the \
         recovery revision, so the ladder would climb while the plan kept being restored"
    );
}

#[tokio::test]
async fn every_plan_column_can_be_rewritten_conditionally() {
    let (_dir, pool) = test_pool().await;
    let legacy = r#"{"model_id":"m","device":"cuda"}"#;
    let upcast = r#"{"model_id":"m","device":{"backend":"cuda","ordinal":0},"plan_revision":2}"#;

    for column in [
        job::PlanColumn::Resource,
        job::PlanColumn::Vision,
        job::PlanColumn::Tts,
        job::PlanColumn::Align,
    ] {
        let job_id = seeded_job(&pool).await;
        match column {
            job::PlanColumn::Resource => job::set_job_resource_plan(&pool, &job_id, legacy).await,
            job::PlanColumn::Vision => job::set_job_vision_plan(&pool, &job_id, legacy).await,
            job::PlanColumn::Tts => job::set_job_tts_plan(&pool, &job_id, legacy).await,
            job::PlanColumn::Align => job::set_job_align_plan(&pool, &job_id, legacy).await,
        }
        .expect("seed");

        assert!(
            job::rewrite_plan_if_unchanged(&pool, &job_id, column, legacy, upcast)
                .await
                .expect("rewrite"),
            "{column:?} did not accept an upcast of the text it still held"
        );
        assert!(
            !job::rewrite_plan_if_unchanged(&pool, &job_id, column, legacy, upcast)
                .await
                .expect("rewrite"),
            "{column:?} rewrote a column that had already moved on"
        );

        let record = job::get_job(&pool, &job_id)
            .await
            .expect("job")
            .expect("row");
        let stored = match column {
            job::PlanColumn::Resource => record.resource_plan_json,
            job::PlanColumn::Vision => record.vision_plan_json,
            job::PlanColumn::Tts => record.tts_plan_json,
            job::PlanColumn::Align => record.align_plan_json,
        };
        assert_eq!(stored.as_deref(), Some(upcast), "{column:?}");
    }
}

#[tokio::test]
async fn a_plan_rewrite_lands_when_the_column_is_untouched() {
    let (_dir, pool) = test_pool().await;
    let job_id = seeded_job(&pool).await;

    let legacy = r#"{"model_id":"m","device":"cuda"}"#;
    job::set_job_resource_plan(&pool, &job_id, legacy)
        .await
        .expect("seed");
    let upcast = r#"{"model_id":"m","device":{"backend":"cuda","ordinal":0},"plan_revision":2}"#;
    assert!(job::rewrite_plan_if_unchanged(
        &pool,
        &job_id,
        job::PlanColumn::Resource,
        legacy,
        upcast,
    )
    .await
    .expect("rewrite"));

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    assert_eq!(record.resource_plan_json.as_deref(), Some(upcast));
}

#[tokio::test]
async fn a_benchmark_refresh_does_not_erase_learned_coefficients() {
    let (_dir, pool) = test_pool().await;
    models::update_learned(&pool, |learned| {
        learned.record_tts_rtf("kokoro-82m", "cpu", 0.9);
    })
    .await
    .expect("learn");

    models::upsert_hw_benchmark(
        &pool,
        &models::HwBenchmark {
            profile_json: r#"{"devices":[]}"#.into(),
            compute_score: 420,
            tok_s_gpu: Some(21.5),
            tok_s_cpu: Some(6.5),
            rtf_table_json: None,
            fingerprint: Some("none|ram:16000|cores:8".into()),
            measured_at: now_ms(),
        },
    )
    .await
    .expect("benchmark");

    assert_eq!(
        models::load_learned(&pool)
            .await
            .expect("learned")
            .tts_rtf("kokoro-82m", "cpu"),
        Some(0.9),
        "migration 0011 states that learned coefficients share this row and survive a \
         hardware change; a benchmark write that carries no table must not erase them"
    );
}

#[tokio::test]
async fn every_concurrent_learned_update_lands() {
    let (_dir, pool) = test_pool().await;
    let writers = (0..8).map(|n| {
        let pool = pool.clone();
        tokio::spawn(async move {
            models::update_learned(&pool, |learned| {
                learned.record_tts_rtf(&format!("voice-{n}"), "cpu", 0.5);
            })
            .await
        })
    });
    for writer in writers {
        writer.await.expect("join").expect("update_learned");
    }

    let learned = models::load_learned(&pool).await.expect("learned");
    for n in 0..8 {
        assert_eq!(learned.tts_rtf(&format!("voice-{n}"), "cpu"), Some(0.5));
    }
}

#[tokio::test]
async fn a_benchmark_measured_on_other_hardware_is_not_served() {
    let (_dir, pool) = test_pool().await;
    let measured = models::HwBenchmark {
        profile_json: r#"{"devices":[]}"#.into(),
        compute_score: 420,
        tok_s_gpu: Some(21.5),
        tok_s_cpu: Some(6.5),
        rtf_table_json: Some("{}".into()),
        fingerprint: Some("pci:0000:01:00.0/cuda/12288|ram:16000|cores:8".into()),
        measured_at: now_ms(),
    };
    models::upsert_hw_benchmark(&pool, &measured)
        .await
        .expect("insert");
    assert_eq!(
        models::get_hw_benchmark(&pool, "none|ram:16000|cores:8")
            .await
            .expect("get"),
        None,
        "an unplugged eGPU must not keep quoting the ETA it used to earn"
    );
}

#[tokio::test]
async fn a_benchmark_with_no_fingerprint_re_benchmarks_once() {
    let (_dir, pool) = test_pool().await;
    let legacy = models::HwBenchmark {
        profile_json: r#"{"devices":[]}"#.into(),
        compute_score: 420,
        tok_s_gpu: Some(21.5),
        tok_s_cpu: Some(6.5),
        rtf_table_json: Some(r#"{"tts":{"kokoro-82m:cpu":{"value":0.9,"samples":1}}}"#.into()),
        fingerprint: None,
        measured_at: now_ms(),
    };
    models::upsert_hw_benchmark(&pool, &legacy)
        .await
        .expect("insert");
    assert_eq!(
        models::get_hw_benchmark(&pool, "none|ram:16000|cores:8")
            .await
            .expect("get"),
        None,
        "a row that predates the column cannot be attributed to an inventory"
    );
    assert_eq!(
        models::load_learned(&pool)
            .await
            .expect("learned")
            .tts_rtf("kokoro-82m", "cpu"),
        Some(0.9),
        "learned coefficients share the row but are keyed on device identity, so they \
         survive a benchmark that does not"
    );
}

#[tokio::test]
async fn cache_entry_lru_helpers() {
    let (_dir, pool) = test_pool().await;
    cache::upsert_entry(&pool, "aaa", "wav", 100)
        .await
        .expect("a");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    cache::upsert_entry(&pool, "bbb", "page_png", 200)
        .await
        .expect("b");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    cache::upsert_entry(&pool, "ccc", "align", 300)
        .await
        .expect("c");

    assert_eq!(cache::total_bytes(&pool).await.expect("total"), 600);
    let entry = cache::get_entry(&pool, "bbb")
        .await
        .expect("get")
        .expect("some");
    assert_eq!(entry.kind, "page_png");
    assert_eq!(entry.bytes, 200);

    let lru = cache::lru_evictable(&pool, 2, &terminal_states_csv())
        .await
        .expect("lru");
    assert_eq!(
        lru.iter().map(|e| e.hash.as_str()).collect::<Vec<_>>(),
        vec!["aaa", "bbb"]
    );

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert!(cache::touch(&pool, "aaa").await.expect("touch"));
    assert!(!cache::touch(&pool, "missing").await.expect("touch missing"));
    let lru = cache::lru_evictable(&pool, 3, &terminal_states_csv())
        .await
        .expect("lru");
    assert_eq!(
        lru.iter().map(|e| e.hash.as_str()).collect::<Vec<_>>(),
        vec!["bbb", "ccc", "aaa"]
    );

    cache::remove(&pool, "bbb").await.expect("remove");
    assert_eq!(cache::total_bytes(&pool).await.expect("total"), 400);
}

#[tokio::test]
async fn deleting_book_cascades_children() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "Ch".into(),
        page_start: 1,
        page_end: 2,
        is_pseudo: true,
        word_count: None,
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let page = book::Page {
        id: new_id(),
        book_id: b.id.clone(),
        page_no: 1,
        width_pt: None,
        height_pt: None,
        render_hash: None,
        text_json: None,
        needs_upgrade: false,
    };
    book::insert_page(&pool, &page).await.expect("page");

    let v = sample_voice();
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"chapters","chapter_ids":[]}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: v.id.clone(),
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
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &render_job).await.expect("job");
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: render_job.id.clone(),
        chapter_id: chapter.id.clone(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "Hello".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: None,
        duration_ms: None,
        original_text: None,
    };
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .expect("segment");

    book::delete_book(&pool, &b.id).await.expect("delete");
    assert_eq!(
        book::list_chapters(&pool, &b.id).await.expect("chapters"),
        vec![]
    );
    assert_eq!(book::get_page(&pool, &b.id, 1).await.expect("page"), None);
    assert_eq!(
        job::get_job(&pool, &render_job.id).await.expect("job"),
        None
    );
    assert_eq!(
        job::list_segments(&pool, &render_job.id)
            .await
            .expect("segments"),
        vec![]
    );
    assert_eq!(
        voice::get_voice_profile(&pool, &v.id).await.expect("voice"),
        Some(v)
    );
}

#[tokio::test]
async fn deleting_job_cascades_segments_and_alignments() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "Ch".into(),
        page_start: 1,
        page_end: 2,
        is_pseudo: true,
        word_count: None,
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let v = sample_voice();
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"chapters","chapter_ids":[]}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: v.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "CANCELLED".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &render_job).await.expect("job");
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: render_job.id.clone(),
        chapter_id: chapter.id.clone(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "Hello".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: None,
        duration_ms: None,
        original_text: None,
    };
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .expect("segment");
    let alignment = job::Alignment {
        segment_id: segment.id.clone(),
        source: "tts_native".into(),
        words_json: WordsJson::from_stored("[]".into()),
        wer: None,
    };
    job::upsert_alignment(&pool, &alignment)
        .await
        .expect("alignment");

    job::update_job_state(&pool, &render_job.id, "SYNTHESIZING")
        .await
        .expect("mark running");
    let deleted = job::delete_job_if_settled(&pool, &render_job.id)
        .await
        .expect("delete attempt");
    assert!(!deleted, "running job must not be deletable");
    assert!(job::get_job(&pool, &render_job.id)
        .await
        .expect("job")
        .is_some());

    job::update_job_state(&pool, &render_job.id, "CANCELLED")
        .await
        .expect("settle");
    let deleted = job::delete_job_if_settled(&pool, &render_job.id)
        .await
        .expect("delete");
    assert!(deleted);
    assert_eq!(
        job::get_job(&pool, &render_job.id).await.expect("job"),
        None
    );
    assert_eq!(
        job::list_segments(&pool, &render_job.id)
            .await
            .expect("segments"),
        vec![]
    );
    assert_eq!(
        job::get_alignment(&pool, &segment.id)
            .await
            .expect("alignment query"),
        None
    );
    assert_eq!(book::get_book(&pool, &b.id).await.expect("book"), Some(b));
}

#[tokio::test]
async fn check_constraints_reject_invalid_values() {
    let (_dir, pool) = test_pool().await;
    let mut invalid = sample_book();
    invalid.format = "docx".into();
    assert!(book::insert_book(&pool, &invalid).await.is_err());

    let mut bad_voice = sample_voice();
    bad_voice.kind = "synthetic".into();
    assert!(voice::insert_voice_profile(&pool, &bad_voice)
        .await
        .is_err());
}

#[tokio::test]
async fn a_chapter_render_row_cannot_disagree_with_itself_about_its_artifact() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let chapter = book::Chapter {
        id: new_id(),
        book_id: book_id.clone(),
        idx: 0,
        title: "One".into(),
        page_start: 1,
        page_end: 2,
        is_pseudo: false,
        word_count: Some(10),
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let row = chapter_render::ChapterRender {
        job_id: job_id.clone(),
        chapter_id: chapter.id.clone(),
        idx: 0,
        hash: "h".into(),
        artifact: chapter_render::ARTIFACT_MP3.into(),
        duration_ms: 1000,
        encoded_ms: Some(1038),
    };
    chapter_render::replace_all(&pool, &job_id, std::slice::from_ref(&row))
        .await
        .expect("a materialised chapter carries its encoded duration");

    let marker_with_duration = chapter_render::ChapterRender {
        artifact: chapter_render::ARTIFACT_NONE.into(),
        ..row.clone()
    };
    assert!(
        chapter_render::replace_all(&pool, &job_id, std::slice::from_ref(&marker_with_duration))
            .await
            .is_err(),
        "a dirty-marker row has no artifact to measure, so it must not carry an encoded duration"
    );

    let artifact_without_duration = chapter_render::ChapterRender {
        encoded_ms: None,
        ..row.clone()
    };
    assert!(
        chapter_render::replace_all(
            &pool,
            &job_id,
            std::slice::from_ref(&artifact_without_duration)
        )
        .await
        .is_err(),
        "markers are built from encoded_ms, so an mp3 row without one would silently fall \
         back to nominal drift"
    );

    assert_eq!(
        chapter_render::list(&pool, &job_id).await.expect("rows"),
        vec![row],
        "a rejected transaction must leave the committed row untouched"
    );
}

#[tokio::test]
async fn the_review_query_ships_only_the_segments_a_reviewer_can_act_on() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let chapter = book::Chapter {
        id: new_id(),
        book_id: book_id.clone(),
        idx: 0,
        title: "One".into(),
        page_start: 1,
        page_end: 2,
        is_pseudo: false,
        word_count: Some(10),
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");

    let mut segments = Vec::new();
    for idx in 0..5 {
        segments.push(job::ScriptSegment {
            id: format!("seg-{idx}"),
            job_id: job_id.clone(),
            chapter_id: chapter.id.clone(),
            idx,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: format!("Sentence {idx}."),
            source_span_json: None,
            flags_json: match idx {
                1 => Some(r#"["verify_failed"]"#.into()),
                2 => Some("[]".into()),
                _ => None,
            },
            audio_hash: Some(format!("h{idx}")),
            duration_ms: Some(900),
            original_text: None,
        });
    }
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    job::edit_segment_text(&pool, "seg-3", "An edited sentence.")
        .await
        .expect("edit");
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: "seg-1".into(),
            source: "whisperx".into(),
            words_json: WordsJson::from_stored("[]".into()),
            wer: Some(0.4),
        },
    )
    .await
    .expect("alignment");

    let rows = job::list_reviewable_segments(&pool, &job_id, &[])
        .await
        .expect("review rows");
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec!["seg-1", "seg-3"],
        "a clean, unedited sentence is not reviewable, and an empty flag array is not a flag"
    );
    assert_eq!(rows[0].wer, Some(0.4), "the join carries the score");
    assert_eq!(rows[0].align_source.as_deref(), Some("whisperx"));
    assert!(
        rows[1].original_text.is_some(),
        "an edited row is reviewable so it can be reverted"
    );
    assert_eq!(
        rows[1].wer, None,
        "no alignment row means no score, not an error"
    );

    let with_missing = job::list_reviewable_segments(&pool, &job_id, &["seg-4"])
        .await
        .expect("review rows");
    assert_eq!(
        with_missing
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["seg-1", "seg-3", "seg-4"],
        "a segment the report listed as missing carries no flag, so the filter must accept          the reported ids or the panel silently drops the missing band"
    );
}

#[tokio::test]
async fn app_setting_round_trip() {
    let (_dir, pool) = test_pool().await;
    assert_eq!(
        settings::get_setting(&pool, "default_model.llm")
            .await
            .expect("get"),
        None
    );

    settings::set_setting(&pool, "default_model.llm", "phi4-mini-q4km")
        .await
        .expect("set");
    settings::set_setting(&pool, "default_model.tts", "kokoro-82m")
        .await
        .expect("set");
    settings::set_setting(&pool, "ui.theme", "dark")
        .await
        .expect("set");

    settings::set_setting(&pool, "default_model.llm", "gemma4-e2b-q4km")
        .await
        .expect("upsert");
    assert_eq!(
        settings::get_setting(&pool, "default_model.llm")
            .await
            .expect("get"),
        Some("gemma4-e2b-q4km".into())
    );

    let defaults = settings::list_settings_with_prefix(&pool, "default_model.")
        .await
        .expect("list");
    assert_eq!(
        defaults,
        vec![
            ("default_model.llm".into(), "gemma4-e2b-q4km".into()),
            ("default_model.tts".into(), "kokoro-82m".into()),
        ]
    );

    settings::delete_setting(&pool, "default_model.llm")
        .await
        .expect("delete");
    assert_eq!(
        settings::get_setting(&pool, "default_model.llm")
            .await
            .expect("get"),
        None
    );

    settings::set_setting(&pool, "kokoro-82m.note", "kokoro-82m")
        .await
        .expect("set");
    settings::delete_settings_with_value(&pool, "default_model.", "kokoro-82m")
        .await
        .expect("delete by value");
    assert_eq!(
        settings::get_setting(&pool, "default_model.tts")
            .await
            .expect("get"),
        None
    );
    assert_eq!(
        settings::get_setting(&pool, "kokoro-82m.note")
            .await
            .expect("get"),
        Some("kokoro-82m".into())
    );
    assert_eq!(
        settings::get_setting(&pool, "ui.theme").await.expect("get"),
        Some("dark".into())
    );
}

#[tokio::test]
async fn onboarding_flag_defaults_false_and_persists() {
    let (_dir, pool) = test_pool().await;
    assert_eq!(
        settings::get_setting(&pool, "onboarding_complete")
            .await
            .expect("get"),
        None
    );

    settings::set_setting(&pool, "onboarding_complete", "true")
        .await
        .expect("set");
    assert_eq!(
        settings::get_setting(&pool, "onboarding_complete")
            .await
            .expect("get"),
        Some("true".into())
    );
}

#[tokio::test]
async fn stage_plan_columns_persist_independently() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":10}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: v.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: Some(r#"{"model_id":"qwen","device":"cuda"}"#.into()),
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "QUEUED".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &render_job).await.expect("insert");

    let tts_plan = r#"{"model_id":"kokoro-82m","device":"cuda","tts_workers":2,"batch":8}"#;
    let align_plan = r#"{"model_id":"whisperx-small-int8","device":"cpu","align_workers":2}"#;
    job::set_job_tts_plan(&pool, &render_job.id, tts_plan)
        .await
        .expect("tts plan");
    job::set_job_align_plan(&pool, &render_job.id, align_plan)
        .await
        .expect("align plan");

    let stored = job::get_job(&pool, &render_job.id)
        .await
        .expect("get")
        .expect("job");
    assert_eq!(stored.tts_plan_json.as_deref(), Some(tts_plan));
    assert_eq!(stored.align_plan_json.as_deref(), Some(align_plan));
    assert_eq!(
        stored.resource_plan_json.as_deref(),
        Some(r#"{"model_id":"qwen","device":"cuda"}"#),
        "stage plans must never overwrite the script plan"
    );
    assert_eq!(stored.vision_plan_json, None);
}

async fn seed_book_and_job(pool: &SqlitePool) -> (String, String) {
    let b = sample_book();
    book::insert_book(pool, &b).await.expect("book");
    let v = sample_voice();
    voice::insert_voice_profile(pool, &v).await.expect("voice");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":2}"#.into(),
        mode: "teacher".into(),
        voice_profile_id: v.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "DONE".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &render_job).await.expect("job");
    (b.id, render_job.id)
}

#[tokio::test]
async fn deleting_a_job_keeps_its_export_playable() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/a.mp3",
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("record");

    let deleted = job::delete_job_if_settled(&pool, &job_id)
        .await
        .expect("delete job");
    assert!(deleted);

    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(rows.len(), 1, "the audio must outlive its queue entry");
    assert_eq!(rows[0].job_id, None, "job_id is cleared, not cascaded away");
    assert_eq!(rows[0].book_id.as_deref(), Some(book_id.as_str()));
}

#[tokio::test]
async fn deleting_a_book_leaves_no_unattributable_export_row() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/b.mp3",
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("record");

    let paths = export::paths_for_book(&pool, &book_id)
        .await
        .expect("paths");
    assert_eq!(paths, vec!["/tmp/b.mp3".to_string()]);
    assert!(export::delete_by_path(&pool, &paths[0])
        .await
        .expect("delete export"));
    book::delete_book(&pool, &book_id).await.expect("delete");

    assert_eq!(export::list_exports(&pool).await.expect("list"), vec![]);
}

#[tokio::test]
async fn deleting_a_book_only_nulls_the_link_on_an_export_it_could_not_remove() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/stuck.mp3",
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("record");

    book::delete_book(&pool, &book_id).await.expect("delete");

    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(
        rows.len(),
        1,
        "SET NULL, not CASCADE, is what keeps the row reachable"
    );
    assert_eq!(rows[0].book_id, None);
    assert_eq!(rows[0].path, "/tmp/stuck.mp3");
}

#[tokio::test]
async fn purging_a_book_refuses_a_path_outside_the_exports_dir_and_keeps_its_row() {
    let (dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");

    let inside = exports.join("keeper.mp3");
    std::fs::write(&inside, b"audio").expect("write");
    let outside = dir.path().join("escape.mp3");
    std::fs::write(&outside, b"precious").expect("write");
    for path in [&inside, &outside] {
        export::record(
            &pool,
            &book_id,
            &job_id,
            &path.to_string_lossy(),
            "mp3",
            "teacher",
            "digest",
        )
        .await
        .expect("record");
    }

    let outcome = export::purge_for_book(&pool, &exports, &book_id)
        .await
        .expect("purge");

    assert_eq!(outcome.deleted, 1);
    assert_eq!(outcome.failed, 1);
    assert!(!inside.exists(), "an export inside the dir is removed");
    assert!(outside.exists(), "deleting a book must not escape exports/");
    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, outside.to_string_lossy());
}

#[tokio::test]
async fn an_export_row_survives_book_deletion_if_it_was_not_collected_first() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/c.mp3",
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("record");

    book::delete_book(&pool, &book_id).await.expect("delete");

    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(rows.len(), 1, "cascade must never make a file invisible");
    assert_eq!(rows[0].book_id, None);
}

#[tokio::test]
async fn orphan_files_on_disk_are_adopted_once() {
    let (dir, pool) = test_pool().await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");
    // Named for a pre-layout export whose filename used to end in the mode it
    // was made with. The mode is no longer recoverable from the name, and for an
    // orphan the row that held it is missing by definition, so the adopted row
    // takes the documented default instead.
    let orphan = exports.join("Some Book - ch 1 - teacher.mp3");
    std::fs::write(&orphan, b"audio-bytes").expect("write");
    std::fs::write(exports.join("notes.txt"), b"ignore me").expect("write");

    let adopted = export::adopt_orphans(&pool, &exports).await.expect("adopt");
    assert_eq!(adopted, 1, "only playable media is adopted");

    let again = export::adopt_orphans(&pool, &exports).await.expect("adopt");
    assert_eq!(again, 0, "adoption must be idempotent");

    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bytes, 11);
    assert_eq!(
        rows[0].mode,
        export::ADOPTED_ORPHAN_MODE,
        "an orphan has no row to read a mode from, so it is recorded as the default rather \
         than guessed at from a filename that no longer states one"
    );
    assert_eq!(rows[0].book_id, None);
}

#[tokio::test]
async fn an_orphan_inside_a_cast_directory_is_adopted_and_its_book_linked_back() {
    let (dir, pool) = test_pool().await;
    let exports = dir.path().join("exports");
    let cast = exports.join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(&cast).expect("mkdir");
    let orphan = cast.join("Some Book - Ch 1-8.mp3");
    std::fs::write(&orphan, b"audio-bytes").expect("write");

    assert_eq!(
        export::adopt_orphans(&pool, &exports).await.expect("adopt"),
        1,
        "a cast file is a product like any other: leaving it unadopted would make it \
         invisible in the shelf and unreachable by every cleanup"
    );
    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, orphan.to_string_lossy());

    assert!(
        export::remove_within(&exports, &rows[0].path),
        "the adopted nested file is removable through the same containment-checked path \
         every other cleanup uses"
    );
    assert!(!orphan.exists(), "the nested file is removed");
    assert!(
        !cast.exists(),
        "and the emptied cast directory goes with it"
    );
}

#[tokio::test]
async fn a_symlinked_directory_of_mp3s_is_never_offered_as_an_export() {
    let (dir, pool) = test_pool().await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");
    let library = dir.path().join("My Music");
    std::fs::create_dir_all(&library).expect("mkdir");
    std::fs::write(library.join("01 - my song.mp3"), b"not a cast").expect("write");
    let product = exports
        .join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        .join("Some Book - Ch 1.mp3");
    std::fs::create_dir_all(product.parent().unwrap()).expect("mkdir");
    std::fs::write(&product, b"audio-bytes").expect("write");
    if !link_dir(&library, &exports.join("music")) {
        eprintln!("skipping: this platform refused to create a directory symlink");
        return;
    }

    assert_eq!(
        export::adopt_orphans(&pool, &exports).await.expect("adopt"),
        1,
        "the exports directory is the user's; a link inside it must not hand the app every \
         mp3 they keep elsewhere, which would put those files on the Audios shelf and in the \
         storage count as exports of this app"
    );
    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, product.to_string_lossy());
}

#[tokio::test]
async fn deleting_an_export_with_a_stale_partial_leaves_no_hollow_cast_directory() {
    let (dir, pool) = test_pool().await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let cast = exports.join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(&cast).expect("mkdir");
    let file = cast.join("Some Book - Ch 1.mp3");
    std::fs::write(&file, b"audio-bytes").expect("write");
    // A power loss mid-encode leaves the abandoned encode next to the export it
    // never replaced.
    let stale = export::partial_sibling(&file);
    std::fs::write(&stale, b"half an encode").expect("write");
    export::record(
        &pool,
        &book_id,
        &job_id,
        &file.to_string_lossy(),
        "mp3",
        "narrator",
        "digest",
    )
    .await
    .expect("record");

    assert!(
        export::remove_within(&exports, &file.to_string_lossy()),
        "the export is removable through the containment-checked path the delete command uses"
    );
    assert!(export::delete_by_path(&pool, &file.to_string_lossy())
        .await
        .expect("drop row"));
    assert!(
        !cast.exists(),
        "the abandoned sibling has to go with the export, or the directory stays occupied and \
         stays hollow forever"
    );

    assert_eq!(
        export::sweep_partials(&exports),
        0,
        "and the next launch's sweep finds nothing left to do"
    );
    assert!(exports.exists(), "the exports root survives");
}

#[tokio::test]
async fn purging_a_book_removes_its_nested_exports_and_their_directories() {
    let (dir, pool) = test_pool().await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let mut nested: Vec<String> = Vec::new();
    // Real `cast_key` names: the prune only removes a directory whose name is
    // one `orchestrator::render::cast_key` could have produced, so a fixture that
    // invents an 8-character name would assert a behaviour the app does not have.
    for (position, key) in [
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
    ]
    .iter()
    .enumerate()
    {
        let file = exports
            .join(key)
            .join(format!("Some Book - Ch {}.mp3", position + 1));
        std::fs::create_dir_all(file.parent().unwrap()).expect("mkdir");
        std::fs::write(&file, b"audio-bytes").expect("write");
        export::record(
            &pool,
            &book_id,
            &job_id,
            &file.to_string_lossy(),
            "mp3",
            "narrator",
            "digest",
        )
        .await
        .expect("record");
        nested.push(file.to_string_lossy().into_owned());
    }
    assert_eq!(nested.len(), 2);
    assert_eq!(export::list_exports(&pool).await.expect("list").len(), 2);

    let outcome = export::purge_for_book(&pool, &exports, &book_id)
        .await
        .expect("purge");
    assert_eq!(outcome.deleted, 2);
    assert_eq!(outcome.failed, 0);
    assert!(
        export::list_exports(&pool).await.expect("list").is_empty(),
        "the rows go with the bytes, or the shelf lists entries whose audio is gone"
    );
    for key in [
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
    ] {
        assert!(
            !exports.join(key).exists(),
            "{key} held nothing else and must not be left behind"
        );
    }
    assert!(exports.exists(), "the exports root survives");
}

#[tokio::test]
async fn recording_the_same_path_twice_updates_rather_than_duplicates() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/d.mp3",
        "mp3",
        "narrator",
        "digest",
    )
    .await
    .expect("first");
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/d.mp3",
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("second");

    let rows = export::list_exports(&pool).await.expect("list");
    assert_eq!(
        rows.len(),
        1,
        "a re-render overwrites the file, not adds one"
    );
    assert_eq!(rows[0].mode, "teacher");
}

#[tokio::test]
async fn re_recording_returns_the_surviving_row_id() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let first = export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/e.mp3",
        "mp3",
        "narrator",
        "digest",
    )
    .await
    .expect("first");
    let second = export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/e.mp3",
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("second");

    assert_eq!(
        first, second,
        "returning a freshly generated id on conflict points callers at a row that was \
         never inserted"
    );
    assert!(export::get_export(&pool, &second)
        .await
        .expect("get")
        .is_some());
}

#[tokio::test]
async fn purge_all_removes_every_file_and_row() {
    let (dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");

    let mut paths = Vec::new();
    for name in ["one.mp3", "two.mp3"] {
        let path = exports.join(name);
        std::fs::write(&path, b"audio").expect("write");
        let display = path.to_string_lossy().to_string();
        export::record(
            &pool, &book_id, &job_id, &display, "mp3", "teacher", "digest",
        )
        .await
        .expect("record");
        paths.push(path);
    }

    let outcome = export::purge_all(&pool, &exports).await.expect("purge");
    assert_eq!(outcome.deleted, 2);
    assert_eq!(outcome.failed, 0);
    assert!(paths.iter().all(|path| !path.exists()));
    assert_eq!(export::list_exports(&pool).await.expect("list"), vec![]);
}

#[tokio::test]
async fn purge_all_refuses_a_path_outside_the_exports_dir_and_keeps_its_row() {
    let (dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");

    let outside = dir.path().join("not-an-export.mp3");
    std::fs::write(&outside, b"precious").expect("write");
    export::record(
        &pool,
        &book_id,
        &job_id,
        &outside.to_string_lossy(),
        "mp3",
        "teacher",
        "digest",
    )
    .await
    .expect("record");

    let outcome = export::purge_all(&pool, &exports).await.expect("purge");
    assert_eq!(outcome.deleted, 0);
    assert_eq!(outcome.failed, 1);
    assert!(outside.exists(), "a bulk delete must not escape exports/");
    assert_eq!(
        export::list_exports(&pool).await.expect("list").len(),
        1,
        "a refused file keeps its row so it stays visible"
    );
}

#[tokio::test]
async fn purge_all_clears_a_row_whose_file_already_vanished() {
    let (dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).expect("mkdir");
    let missing = exports.join("gone.mp3").to_string_lossy().to_string();
    export::record(
        &pool, &book_id, &job_id, &missing, "mp3", "teacher", "digest",
    )
    .await
    .expect("record");

    let outcome = export::purge_all(&pool, &exports).await.expect("purge");
    assert_eq!(outcome.deleted, 1);
    assert_eq!(outcome.failed, 0);
    assert_eq!(export::list_exports(&pool).await.expect("list"), vec![]);
}

#[tokio::test]
async fn a_reingest_drops_the_render_hash_of_a_page_it_replaces() {
    let (_dir, pool) = test_pool().await;
    let (book_id, existing_id) = seed_reingest_fixture(&pool).await;
    let before = book::get_page(&pool, &book_id, 1)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(
        before.render_hash.as_deref(),
        Some("d".repeat(64).as_str()),
        "the fixture must start with a render hash, or the assertion below proves nothing"
    );

    reingest_one_page(&pool, &book_id).await;

    let stored = book::get_page(&pool, &book_id, 1)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(
        stored.id, existing_id,
        "the upsert updates the row in place, so anything keyed on the page id survives it"
    );
    assert_eq!(
        stored.render_hash, None,
        "the upsert's UPDATE arm is the common re-ingest path; leaving render_hash behind \
         serves a PNG rasterised under the previous pagination beside the new blocks"
    );
}

#[tokio::test]
async fn a_reingest_drops_the_geometry_of_a_page_it_replaces() {
    let (_dir, pool) = test_pool().await;
    let (book_id, existing_id) = seed_reingest_fixture(&pool).await;
    assert!(
        book::get_page_geometry(&pool, &existing_id)
            .await
            .expect("geometry")
            .is_some(),
        "positive control: the fixture must hold geometry before the re-ingest, or the \
         deletion below is indistinguishable from a row that was never written"
    );

    reingest_one_page(&pool, &book_id).await;

    let stored = book::get_page(&pool, &book_id, 1)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(stored.id, existing_id);
    assert!(
        book::get_page_geometry(&pool, &existing_id)
            .await
            .expect("geometry")
            .is_none(),
        "a re-ingest rewrites what the page contains while the ON CONFLICT arm preserves its \
         id, so geometry left behind is keyed to content that no longer exists and would be \
         classified fresh whenever the extractor identity happens not to have moved"
    );
}

async fn seed_reingest_fixture(pool: &SqlitePool) -> (String, String) {
    let mut b = sample_book();
    b.blocks_version = Some("gen-1".into());
    b.blocks_page_count = Some(1);
    b.page_count = Some(1);
    book::insert_book(pool, &b).await.expect("book");
    let existing = book::Page {
        id: new_id(),
        book_id: b.id.clone(),
        page_no: 1,
        width_pt: Some(612.0),
        height_pt: Some(792.0),
        render_hash: Some("d".repeat(64)),
        text_json: Some(r#"{"quality":{},"blocks":[1]}"#.into()),
        needs_upgrade: false,
    };
    book::insert_page(pool, &existing).await.expect("page");
    book::upsert_page_geometry(
        pool,
        &book::PageGeometry {
            page_id: existing.id.clone(),
            geometry_version: "v1|geom=1|pymupdf=1.28.0|reflow=612x792@11".into(),
            geometry_json: r#"{"v":1,"lines":[],"words":[]}"#.into(),
        },
    )
    .await
    .expect("geometry");
    (b.id, existing.id)
}

async fn reingest_one_page(pool: &SqlitePool, book_id: &str) {
    let replacement = book::Page {
        id: new_id(),
        book_id: book_id.to_string(),
        page_no: 1,
        width_pt: Some(612.0),
        height_pt: Some(792.0),
        render_hash: None,
        text_json: Some(r#"{"quality":{},"blocks":[2]}"#.into()),
        needs_upgrade: false,
    };
    book::replace_blocks(
        pool,
        book_id,
        &[replacement],
        &[],
        book::Generation {
            blocks_version: "gen-2",
            language: Some("en"),
            layout_analysed: None,
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");
}

#[tokio::test]
async fn replace_blocks_flips_the_generation_only_after_pages_land() {
    let (_dir, pool) = test_pool().await;
    let mut b = sample_book();
    b.blocks_version = None;
    b.blocks_page_count = None;
    b.page_count = Some(2);
    book::insert_book(&pool, &b).await.expect("book");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "One".into(),
        page_start: 1,
        page_end: 2,
        is_pseudo: false,
        word_count: Some(10),
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");

    let pages: Vec<book::Page> = (1..=2)
        .map(|page_no| book::Page {
            id: new_id(),
            book_id: b.id.clone(),
            page_no,
            width_pt: Some(612.0),
            height_pt: Some(792.0),
            render_hash: None,
            text_json: Some(format!(r#"{{"quality":{{}},"blocks":[{page_no}]}}"#)),
            needs_upgrade: page_no == 2,
        })
        .collect();
    let mut refreshed = chapter.clone();
    refreshed.word_count = Some(42);

    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &[refreshed],
        book::Generation {
            blocks_version: "gen-2",
            language: Some("fr"),
            layout_analysed: None,
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");

    let stored = book::get_book(&pool, &b.id)
        .await
        .expect("get")
        .expect("book");
    assert_eq!(stored.blocks_version.as_deref(), Some("gen-2"));
    assert_eq!(stored.blocks_page_count, Some(2));
    assert_eq!(stored.language.as_deref(), Some("fr"));

    let chapters = book::list_chapters(&pool, &b.id).await.expect("chapters");
    assert_eq!(chapters.len(), 1, "chapters are updated, never recreated");
    assert_eq!(
        chapters[0].id, chapter.id,
        "the id must survive re-extraction"
    );
    assert_eq!(chapters[0].word_count, Some(42));

    let page = book::get_page(&pool, &b.id, 2)
        .await
        .expect("get")
        .expect("page");
    assert!(page.needs_upgrade);
    assert_eq!(page.width_pt, Some(612.0));
}

#[tokio::test]
async fn a_shorter_re_extraction_republishes_the_page_count_and_drops_stale_pages() {
    let (_dir, pool) = test_pool().await;
    let mut b = sample_book();
    b.blocks_version = Some("gen-1".into());
    b.blocks_page_count = Some(3);
    b.page_count = Some(3);
    book::insert_book(&pool, &b).await.expect("book");
    for page_no in 1..=3 {
        book::insert_page(
            &pool,
            &book::Page {
                id: new_id(),
                book_id: b.id.clone(),
                page_no,
                width_pt: Some(612.0),
                height_pt: Some(792.0),
                render_hash: None,
                text_json: Some(r#"{"quality":{},"blocks":[]}"#.into()),
                needs_upgrade: false,
            },
        )
        .await
        .expect("page");
    }

    let pages: Vec<book::Page> = (1..=2)
        .map(|page_no| book::Page {
            id: new_id(),
            book_id: b.id.clone(),
            page_no,
            width_pt: Some(595.0),
            height_pt: Some(842.0),
            render_hash: None,
            text_json: Some(r#"{"quality":{},"blocks":[]}"#.into()),
            needs_upgrade: false,
        })
        .collect();

    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &[],
        book::Generation {
            blocks_version: "gen-2",
            language: None,
            layout_analysed: None,
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");

    let stored = book::get_book(&pool, &b.id)
        .await
        .expect("get")
        .expect("book");
    assert_eq!(
        stored.page_count,
        Some(2),
        "a reflow that changes the page count must republish page_count, or every later \
         job re-extracts the whole book and then fails"
    );
    assert_eq!(stored.blocks_page_count, Some(2));
    assert_eq!(
        stored.page_count, stored.blocks_page_count,
        "the freshness predicate compares these two, so they must agree after a backfill"
    );
    assert_eq!(
        book::list_pages(&pool, &b.id).await.expect("pages").len(),
        2,
        "pages beyond the new generation must not survive"
    );
    assert!(
        book::get_page(&pool, &b.id, 3)
            .await
            .expect("get")
            .is_none(),
        "page 3 belongs to the old generation"
    );
}

#[tokio::test]
async fn re_extraction_never_cascades_away_a_finished_jobs_segments() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed_book_and_job(&pool).await;
    let chapter = book::Chapter {
        id: new_id(),
        book_id: book_id.clone(),
        idx: 0,
        title: "One".into(),
        page_start: 1,
        page_end: 1,
        is_pseudo: false,
        word_count: Some(10),
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let chapter_id = chapter.id.clone();
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: job_id.clone(),
        chapter_id: chapter_id.clone(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "Already rendered.".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: Some("wav-hash".into()),
        duration_ms: Some(1200),
        original_text: None,
    };
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .expect("segment");

    let refreshed: Vec<book::Chapter> = book::list_chapters(&pool, &book_id)
        .await
        .expect("chapters")
        .into_iter()
        .map(|mut c| {
            c.word_count = Some(999);
            c
        })
        .collect();
    book::replace_blocks(
        &pool,
        &book_id,
        &[],
        &refreshed,
        book::Generation {
            blocks_version: "gen-9",
            language: None,
            layout_analysed: None,
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");

    let survived = job::list_segments(&pool, &job_id).await.expect("segments");
    assert_eq!(
        survived.len(),
        1,
        "chapter -> script_segment -> alignment cascades, so re-extraction must update \
         chapters in place rather than recreate them"
    );
    assert_eq!(survived[0].audio_hash.as_deref(), Some("wav-hash"));
}

#[tokio::test]
async fn storage_dirs_default_to_the_app_data_dir_when_no_override_is_stored() {
    let (dir, pool) = test_pool().await;
    let data_dir = dir.path().join("appdata");

    let dirs = paths::resolve(&pool, &data_dir).await.expect("resolve");

    assert_eq!(dirs.models_dir(), data_dir.join("models"));
    assert_eq!(dirs.exports_dir(), data_dir.join("exports"));
    assert_eq!(dirs.cache_dir, data_dir.join("cache"));
    assert!(dirs.models.is_default());
    assert!(dirs.exports.is_default());
    assert_eq!(dirs.models.configured, None);
    assert_eq!(dirs.exports.configured, None);
}

#[tokio::test]
async fn storage_dirs_read_each_override_from_its_own_setting_key() {
    let (dir, pool) = test_pool().await;
    let data_dir = dir.path().join("appdata");
    let models = dir.path().join("elsewhere-models");
    let exports = dir.path().join("elsewhere-exports");

    settings::set_setting(&pool, paths::MODELS_DIR_KEY, &models.to_string_lossy())
        .await
        .expect("set models");
    settings::set_setting(&pool, paths::EXPORTS_DIR_KEY, &exports.to_string_lossy())
        .await
        .expect("set exports");

    let dirs = paths::resolve(&pool, &data_dir).await.expect("resolve");

    assert_eq!(dirs.models_dir(), models);
    assert_eq!(dirs.exports_dir(), exports);
    assert_eq!(
        dirs.cache_dir,
        data_dir.join("cache"),
        "the cache stays under the app data dir; only models and exports are configurable"
    );
    assert!(!dirs.models.is_default());
    assert!(!dirs.exports.is_default());
}

#[tokio::test]
async fn a_models_override_does_not_move_the_exports_dir_and_vice_versa() {
    let (dir, pool) = test_pool().await;
    let data_dir = dir.path().join("appdata");
    let models = dir.path().join("only-models");

    settings::set_setting(&pool, paths::MODELS_DIR_KEY, &models.to_string_lossy())
        .await
        .expect("set models");

    let dirs = paths::resolve(&pool, &data_dir).await.expect("resolve");

    assert_eq!(dirs.models_dir(), models);
    assert_eq!(dirs.exports_dir(), data_dir.join("exports"));
}

#[tokio::test]
async fn an_unusable_stored_override_falls_back_to_the_default_and_says_why() {
    let (dir, pool) = test_pool().await;
    let data_dir = dir.path().join("appdata");

    settings::set_setting(&pool, paths::MODELS_DIR_KEY, "  ")
        .await
        .expect("set blank");
    settings::set_setting(&pool, paths::EXPORTS_DIR_KEY, "relative/exports")
        .await
        .expect("set relative");

    let dirs = paths::resolve(&pool, &data_dir).await.expect("resolve");

    assert_eq!(dirs.models_dir(), data_dir.join("models"));
    assert_eq!(dirs.exports_dir(), data_dir.join("exports"));
    assert_eq!(dirs.models.rejected, Some(paths::DirRejection::Empty));
    assert_eq!(
        dirs.exports.rejected,
        Some(paths::DirRejection::NotAbsolute)
    );
    assert_eq!(
        dirs.exports.configured.as_deref(),
        Some("relative/exports"),
        "the rejected value is reported so a hand-edited database is visible rather than silent"
    );
}

#[tokio::test]
async fn a_stored_override_survives_reopening_the_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db.sqlite");
    let data_dir = dir.path().join("appdata");
    let models = dir.path().join("persisted-models");

    let pool = storage::open_pool(&db).await.expect("open");
    settings::set_setting(&pool, paths::MODELS_DIR_KEY, &models.to_string_lossy())
        .await
        .expect("set");
    pool.close().await;

    let reopened = storage::open_pool(&db).await.expect("reopen");
    let dirs = paths::resolve(&reopened, &data_dir).await.expect("resolve");

    assert_eq!(dirs.models_dir(), models);
}

#[tokio::test]
async fn the_pipeline_original_survives_repeated_edits_and_one_revert() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "One".into(),
        page_start: 1,
        page_end: 4,
        is_pseudo: false,
        word_count: Some(10),
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let render_job = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":10}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: v.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "DONE".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &render_job).await.expect("job");
    let job_id = render_job.id.clone();
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: job_id.clone(),
        chapter_id: chapter.id.clone(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "The pipeline wrote this.".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: Some("aaaa".into()),
        duration_ms: Some(1200),
        original_text: None,
    };
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .expect("insert");

    assert!(job::edit_segment_text(&pool, &segment.id, "First edit.")
        .await
        .expect("first edit"));
    let after_first = job::list_segments(&pool, &job_id).await.unwrap();
    assert_eq!(after_first[0].text, "First edit.");
    assert_eq!(
        after_first[0].original_text.as_deref(),
        Some("The pipeline wrote this.")
    );
    assert_eq!(after_first[0].audio_hash, None);
    assert_eq!(after_first[0].duration_ms, None);

    assert!(job::edit_segment_text(&pool, &segment.id, "Second edit.")
        .await
        .expect("second edit"));
    let after_second = job::list_segments(&pool, &job_id).await.unwrap();
    assert_eq!(
        after_second[0].original_text.as_deref(),
        Some("The pipeline wrote this."),
        "original_text is write-once; the pipeline text must survive N edits"
    );

    assert!(job::revert_segment_text(&pool, &segment.id)
        .await
        .expect("revert"));
    let after_revert = job::list_segments(&pool, &job_id).await.unwrap();
    assert_eq!(after_revert[0].text, "The pipeline wrote this.");
    assert_eq!(after_revert[0].original_text, None);

    assert!(
        !job::revert_segment_text(&pool, &segment.id)
            .await
            .expect("second revert"),
        "reverting an unedited segment is a no-op, not an erasure"
    );
    assert!(
        !job::edit_segment_text(&pool, &segment.id, "The pipeline wrote this.")
            .await
            .expect("no-op edit"),
        "an edit that changes nothing must not burn a re-synthesis"
    );
}

#[tokio::test]
async fn a_book_with_a_live_cast_refuses_deletion_and_keeps_its_job() {
    for state in ["CREATED", "QUEUED", "SYNTHESIZING", "PAUSED", "RENDERING"] {
        let (_dir, pool) = test_pool().await;
        let b = sample_book();
        book::insert_book(&pool, &b).await.expect("book");
        let v = sample_voice();
        voice::insert_voice_profile(&pool, &v).await.expect("voice");
        let live = job_for(&b.id, &v.id, state);
        job::insert_job(&pool, &live).await.expect("job");

        let outcome = book::delete_book_unless_busy(&pool, &b.id, now_ms())
            .await
            .expect("delete");
        assert_eq!(outcome, Err(book::DeleteRefusal::JobActive), "{state}");
        assert!(
            book::get_book(&pool, &b.id).await.expect("get").is_some(),
            "{state}"
        );
        assert!(
            job::get_job(&pool, &live.id).await.expect("job").is_some(),
            "{state}: the cascade must not have run, or the export guard sees no live encode"
        );
    }
}

#[tokio::test]
async fn a_book_whose_casts_are_all_terminal_deletes() {
    for state in ["DONE", "CANCELLED", "FAILED"] {
        let (_dir, pool) = test_pool().await;
        let b = sample_book();
        book::insert_book(&pool, &b).await.expect("book");
        let v = sample_voice();
        voice::insert_voice_profile(&pool, &v).await.expect("voice");
        job::insert_job(&pool, &job_for(&b.id, &v.id, state))
            .await
            .expect("job");

        assert_eq!(
            book::delete_book_unless_busy(&pool, &b.id, now_ms())
                .await
                .expect("delete"),
            Ok(()),
            "{state}"
        );
        assert!(
            book::get_book(&pool, &b.id).await.expect("get").is_none(),
            "{state}"
        );
    }
}

#[tokio::test]
async fn deleting_a_missing_book_is_refused_as_missing() {
    let (_dir, pool) = test_pool().await;
    assert_eq!(
        book::delete_book_unless_busy(&pool, "no-such-book", now_ms())
            .await
            .expect("delete"),
        Err(book::DeleteRefusal::NoSuchBook)
    );
}

fn job_for(book_id: &str, voice_id: &str, state: &str) -> job::RenderJob {
    job::RenderJob {
        id: new_id(),
        book_id: book_id.to_string(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":10}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: voice_id.to_string(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: state.into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    }
}

#[tokio::test]
async fn only_one_extraction_claim_can_win_for_a_book() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");

    let first = book::claim_extraction(&pool, &b.id, "owner-a", now_ms())
        .await
        .expect("claim");
    assert!(first.is_ok(), "the first caller takes the lease");

    let second = book::claim_extraction(&pool, &b.id, "owner-a", now_ms())
        .await
        .expect("claim");
    assert_eq!(
        second,
        Err(book::ClaimRefusal::AlreadyExtracting),
        "a second pass must not start while one holds the book"
    );

    book::release_extraction(&pool, &b.id, "owner-a")
        .await
        .expect("release");
    assert!(book::claim_extraction(&pool, &b.id, "owner-a", now_ms())
        .await
        .expect("claim")
        .is_ok());
}

#[tokio::test]
async fn concurrent_claims_produce_exactly_one_winner() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");

    let mut handles = Vec::new();
    for n in 0..8 {
        let pool = pool.clone();
        let id = b.id.clone();
        handles.push(tokio::spawn(async move {
            book::claim_extraction(&pool, &id, &format!("owner-{n}"), now_ms()).await
        }));
    }
    let mut won = 0;
    for handle in handles {
        if handle.await.expect("join").expect("claim").is_ok() {
            won += 1;
        }
    }
    assert_eq!(won, 1, "eight simultaneous callers, one lease");
}

#[tokio::test]
async fn an_active_job_refuses_the_claim_and_a_settled_one_does_not() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");

    let running = job_for(&b.id, &v.id, "SCRIPTING");
    job::insert_job(&pool, &running).await.expect("insert");
    assert_eq!(
        book::claim_extraction(&pool, &b.id, "owner-a", now_ms())
            .await
            .expect("claim"),
        Err(book::ClaimRefusal::JobActive),
        "replacing blocks under a running job would rewrite what it is reading"
    );

    job::update_job_state(&pool, &running.id, "DONE")
        .await
        .expect("settle");
    assert!(book::claim_extraction(&pool, &b.id, "owner-a", now_ms())
        .await
        .expect("claim")
        .is_ok());
}

#[tokio::test]
async fn a_job_cannot_be_created_while_a_book_is_being_re_extracted() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");

    book::claim_extraction(&pool, &b.id, "owner-a", now_ms())
        .await
        .expect("claim")
        .expect("won");

    let blocked = job_for(&b.id, &v.id, "QUEUED");
    assert!(
        !job::insert_job_unless_extracting(&pool, &blocked, now_ms())
            .await
            .expect("insert"),
        "exclusion has to be symmetric or the job side races the extraction side"
    );
    assert_eq!(
        book::count_active_jobs(&pool, &b.id).await.expect("count"),
        0
    );

    book::release_extraction(&pool, &b.id, "owner-a")
        .await
        .expect("release");
    assert!(job::insert_job_unless_extracting(&pool, &blocked, now_ms())
        .await
        .expect("insert"));
}

#[tokio::test]
async fn a_claim_on_a_missing_book_is_refused_rather_than_silently_ignored() {
    let (_dir, pool) = test_pool().await;
    assert_eq!(
        book::claim_extraction(&pool, "no-such-book", "owner-a", now_ms())
            .await
            .expect("claim"),
        Err(book::ClaimRefusal::NoSuchBook)
    );
}

#[tokio::test]
async fn the_job_creation_path_stores_every_column_it_was_given() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");

    let mut record = job_for(&b.id, &v.id, "QUEUED");
    record.model_overrides_json = Some(r#"{"llm":"gemma4-e2b-q4km"}"#.into());
    record.resource_plan_json = Some(r#"{"plan_revision":2}"#.into());
    record.vision_plan_json = Some(r#"{"model_id":"gemma4-e2b-q4km"}"#.into());
    record.tts_plan_json = Some(r#"{"tts_workers":1}"#.into());
    record.align_plan_json = Some(r#"{"align_workers":1}"#.into());
    record.checkpoint_json = Some(r#"{"stage":"SCRIPTING"}"#.into());
    record.overnight = true;
    record.eta_seconds = Some(1234);
    record.report_json = Some(r#"{"verify":{"worst":0.2}}"#.into());
    record.output_paths_json = Some(r#"["/tmp/out.mp3"]"#.into());
    record.tts_seed = Some(4242);
    record.verify = true;

    assert!(
        job::insert_job_unless_extracting(&pool, &record, now_ms())
            .await
            .expect("insert"),
        "no live extraction, so the guarded insert is the create_job path"
    );

    let stored = job::get_job(&pool, &record.id)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        stored, record,
        "the only insert the app runs must persist every column of the row it was handed; \
         a column bound on one insert path and dropped on the other is how verify=1 became \
         verify=0 (#111)"
    );
}

#[tokio::test]
async fn a_lease_from_a_dead_holder_ages_out_instead_of_being_swept() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");

    let long_ago = now_ms() - book::LEASE_STALE_MS - 1;
    book::claim_extraction(&pool, &b.id, "dead-holder", long_ago)
        .await
        .expect("claim")
        .expect("won");
    assert!(
        !book::is_extracting(&pool, &b.id, now_ms())
            .await
            .expect("check"),
        "an unrefreshed lease is not a live one"
    );
    assert!(
        book::claim_extraction(&pool, &b.id, "new-holder", now_ms())
            .await
            .expect("claim")
            .is_ok(),
        "recovery must not need a restart"
    );
    book::release_extraction(&pool, &b.id, "new-holder")
        .await
        .expect("release");

    book::claim_extraction(&pool, &b.id, "dead-again", long_ago)
        .await
        .expect("claim")
        .expect("won");
    let v = sample_voice();
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    assert!(
        job::insert_job_unless_extracting(&pool, &job_for(&b.id, &v.id, "QUEUED"), now_ms())
            .await
            .expect("insert"),
        "with no start-up sweep left, a crashed pass that never released must not block \
         rendering this book forever"
    );
    assert_eq!(
        book::delete_book_unless_busy(&pool, &b.id, now_ms() - book::LEASE_STALE_MS * 2)
            .await
            .expect("delete"),
        Err(book::DeleteRefusal::ExtractionActive),
        "and the age bound is a bound, not an unconditional bypass"
    );
}

#[tokio::test]
async fn a_live_lease_is_not_stealable_by_a_second_instance() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    book::claim_extraction(&pool, &b.id, "instance-one", now_ms())
        .await
        .expect("claim")
        .expect("won");

    assert_eq!(
        book::claim_extraction(&pool, &b.id, "instance-two", now_ms())
            .await
            .expect("claim"),
        Err(book::ClaimRefusal::AlreadyExtracting),
        "a second app instance on the same database must not take a running pass's book"
    );
    assert!(
        !book::release_extraction(&pool, &b.id, "instance-two")
            .await
            .expect("release"),
        "nor release it: an unconditional release let one instance free another's lease"
    );
    assert!(book::is_extracting(&pool, &b.id, now_ms())
        .await
        .expect("check"));

    assert!(book::release_extraction(&pool, &b.id, "instance-one")
        .await
        .expect("release"));
    assert!(!book::is_extracting(&pool, &b.id, now_ms())
        .await
        .expect("check"));
}

#[tokio::test]
async fn a_refresh_keeps_a_long_pass_alive_and_stops_once_the_lease_moves_on() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let stale = now_ms() - book::LEASE_STALE_MS + 5_000;
    book::claim_extraction(&pool, &b.id, "holder", stale)
        .await
        .expect("claim")
        .expect("won");

    assert!(
        book::refresh_extraction(&pool, &b.id, "holder", now_ms())
            .await
            .expect("refresh"),
        "the holder keeps its own lease alive"
    );
    assert!(
        book::claim_extraction(&pool, &b.id, "other", now_ms())
            .await
            .expect("claim")
            .is_err(),
        "a refreshed lease never looks abandoned"
    );

    book::release_extraction(&pool, &b.id, "holder")
        .await
        .expect("release");
    assert!(
        !book::refresh_extraction(&pool, &b.id, "holder", now_ms())
            .await
            .expect("refresh"),
        "a heartbeat must stop once the lease is no longer ours, not resurrect it"
    );
}

#[tokio::test]
async fn blocks_and_the_layout_flag_commit_together() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let pages = vec![book::Page {
        id: new_id(),
        book_id: b.id.clone(),
        page_no: 1,
        width_pt: Some(612.0),
        height_pt: Some(792.0),
        render_hash: None,
        text_json: Some(r#"{"blocks":[]}"#.into()),
        needs_upgrade: false,
    }];

    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &[],
        book::Generation {
            blocks_version: "extract=2;roles=1",
            language: None,
            layout_analysed: Some(true),
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");
    let stored = book::get_book(&pool, &b.id)
        .await
        .expect("get")
        .expect("book");
    assert!(stored.layout_analysed, "one commit carries both");
    assert_eq!(stored.blocks_version.as_deref(), Some("extract=2;roles=1"));

    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &[],
        book::Generation {
            blocks_version: "extract=2;roles=1",
            language: None,
            layout_analysed: None,
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");
    assert!(
        book::get_book(&pool, &b.id)
            .await
            .expect("get")
            .expect("book")
            .layout_analysed,
        "a job's own backfill leaves the provenance it did not establish alone"
    );
}

fn chapter_at(book_id: &str, idx: i64, page_start: i64, page_end: i64) -> book::Chapter {
    book::Chapter {
        id: new_id(),
        book_id: book_id.to_string(),
        idx,
        title: format!("Chapter {idx}"),
        page_start,
        page_end,
        is_pseudo: false,
        word_count: Some(1000),
    }
}

fn page_at(book_id: &str, page_no: i64) -> book::Page {
    book::Page {
        id: new_id(),
        book_id: book_id.to_string(),
        page_no,
        width_pt: Some(612.0),
        height_pt: Some(792.0),
        render_hash: None,
        text_json: Some(r#"{"quality":{},"blocks":[]}"#.into()),
        needs_upgrade: false,
    }
}

async fn book_with_rendered_audio(pool: &SqlitePool) -> (book::Book, String, Vec<book::Chapter>) {
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(pool, &b).await.expect("book");
    voice::insert_voice_profile(pool, &v).await.expect("voice");
    let chapters: Vec<book::Chapter> = (0..2)
        .map(|idx| chapter_at(&b.id, idx, idx * 10 + 1, idx * 10 + 10))
        .collect();
    for chapter in &chapters {
        book::insert_chapter(pool, chapter).await.expect("chapter");
    }
    let done = job_for(&b.id, &v.id, "DONE");
    job::insert_job(pool, &done).await.expect("job");
    job::insert_segments(
        pool,
        &[job::ScriptSegment {
            id: new_id(),
            job_id: done.id.clone(),
            chapter_id: chapters[0].id.clone(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "hello".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        }],
    )
    .await
    .expect("segments");
    (b, done.id, chapters)
}

#[tokio::test]
async fn adopting_more_chapters_than_the_book_had_inserts_the_extra_ones() {
    let (_dir, pool) = test_pool().await;
    let (b, job_id, existing) = book_with_rendered_audio(&pool).await;

    let mut grown: Vec<book::Chapter> = existing.clone();
    grown.push(chapter_at(&b.id, 2, 21, 30));
    let pages: Vec<book::Page> = (1..=30).map(|n| page_at(&b.id, n)).collect();

    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &grown,
        book::Generation {
            blocks_version: "gen-2",
            language: None,
            layout_analysed: Some(true),
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");

    let stored = book::list_chapters(&pool, &b.id).await.expect("list");
    assert_eq!(
        stored.len(),
        3,
        "a deeper reading finds more chapters; the tail must not be left uncovered"
    );
    assert_eq!(stored[2].page_start, 21);
    assert_eq!(stored[2].page_end, 30);
    assert_eq!(
        job::list_segments(&pool, &job_id)
            .await
            .expect("segments")
            .len(),
        1,
        "the rendered job's segments survive"
    );
}

#[tokio::test]
async fn dropping_a_chapter_cascades_its_segments_which_is_why_adopt_is_gated() {
    let (_dir, pool) = test_pool().await;
    let (b, job_id, existing) = book_with_rendered_audio(&pool).await;
    let doomed = existing[1].id.clone();
    job::insert_segments(
        &pool,
        &[job::ScriptSegment {
            id: new_id(),
            job_id: job_id.clone(),
            chapter_id: doomed,
            idx: 1,
            page_no: 11,
            kind: "text".into(),
            figure_id: None,
            text: "on the chapter a shallower reading drops".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        }],
    )
    .await
    .expect("segments");
    assert_eq!(
        job::list_segments(&pool, &job_id)
            .await
            .expect("segments")
            .len(),
        2
    );

    let shrunk = vec![existing[0].clone()];
    let pages: Vec<book::Page> = (1..=12).map(|n| page_at(&b.id, n)).collect();
    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &shrunk,
        book::Generation {
            blocks_version: "gen-3",
            language: None,
            layout_analysed: Some(true),
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");

    assert_eq!(
        job::list_segments(&pool, &job_id)
            .await
            .expect("segments")
            .len(),
        1,
        "script_segment.chapter_id is ON DELETE CASCADE, so dropping a chapter destroys the \
         rendered work sitting on it. This is the hazard; the caller must never reach here \
         for a book that has jobs."
    );
}

#[tokio::test]
async fn a_rendered_book_keeps_every_chapter_so_nothing_is_repointed() {
    let (_dir, pool) = test_pool().await;
    let (b, job_id, existing) = book_with_rendered_audio(&pool).await;
    let before: Vec<(String, i64)> = existing
        .iter()
        .map(|chapter| (chapter.id.clone(), chapter.idx))
        .collect();

    let pinned = txtcaster_lib::library::pin_chapters(&existing, 12);
    let pages: Vec<book::Page> = (1..=12).map(|n| page_at(&b.id, n)).collect();
    book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &pinned,
        book::Generation {
            blocks_version: "gen-4",
            language: None,
            layout_analysed: Some(true),
            owner: None,
            now: now_ms(),
        },
    )
    .await
    .expect("replace");

    let after = book::list_chapters(&pool, &b.id).await.expect("list");
    assert_eq!(
        after
            .iter()
            .map(|chapter| (chapter.id.clone(), chapter.idx))
            .collect::<Vec<_>>(),
        before,
        "identity and position must both survive, or a saved scope resolves to other content"
    );
    assert!(
        after.iter().all(|chapter| chapter.page_end <= 12),
        "bounds are clamped into the pages that survived"
    );
    assert_eq!(
        job::list_segments(&pool, &job_id)
            .await
            .expect("segments")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_book_with_no_jobs_has_nothing_to_strand() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    for idx in 0..2 {
        book::insert_chapter(&pool, &chapter_at(&b.id, idx, idx * 10 + 1, idx * 10 + 10))
            .await
            .expect("chapter");
    }
    assert_eq!(
        book::count_jobs_for_book(&pool, &b.id)
            .await
            .expect("count"),
        0,
        "this is the predicate that lets a re-extraction adopt a new reading freely"
    );
}

#[tokio::test]
async fn dropping_the_lease_guard_gives_the_book_back() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    book::claim_extraction(&pool, &b.id, "panicking-pass", now_ms())
        .await
        .expect("claim")
        .expect("won");

    {
        let _guard =
            txtcaster_lib::library::ExtractionLease::hold(&pool, &b.id, "panicking-pass", None);
        assert!(book::is_extracting(&pool, &b.id, now_ms())
            .await
            .expect("check"));
    }

    for _ in 0..50 {
        if !book::is_extracting(&pool, &b.id, now_ms())
            .await
            .expect("check")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !book::is_extracting(&pool, &b.id, now_ms())
            .await
            .expect("check"),
        "the guard's Drop has to land the release; an unwind or a dropped future must not \
         leave the book un-extractable, un-deletable and un-jobbable"
    );
    assert!(book::claim_extraction(&pool, &b.id, "next-pass", now_ms())
        .await
        .expect("claim")
        .is_ok());
}

#[tokio::test]
async fn a_late_release_cannot_free_the_next_pass() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");

    book::claim_extraction(&pool, &b.id, "first", now_ms())
        .await
        .expect("claim")
        .expect("won");
    book::release_extraction(&pool, &b.id, "first")
        .await
        .expect("release");
    book::claim_extraction(&pool, &b.id, "second", now_ms())
        .await
        .expect("claim")
        .expect("won");

    assert!(
        !book::release_extraction(&pool, &b.id, "first")
            .await
            .expect("release"),
        "a straggling cleanup from the finished pass must not unlock the running one"
    );
    assert!(book::is_extracting(&pool, &b.id, now_ms())
        .await
        .expect("check"));
}

#[tokio::test]
async fn deleting_a_book_takes_its_export_rows_and_files_with_it() {
    let (dir, pool) = test_pool().await;
    let exports_dir = dir.path().join("exports");
    std::fs::create_dir_all(&exports_dir).expect("exports dir");
    let audio = exports_dir.join("whatever.mp3");
    std::fs::write(&audio, b"audio").expect("write audio");

    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    let done = job_for(&b.id, &v.id, "DONE");
    job::insert_job(&pool, &done).await.expect("job");
    export::record(
        &pool,
        &b.id,
        &done.id,
        audio.to_str().expect("path"),
        "mp3",
        "narrator",
        "digest",
    )
    .await
    .expect("export");

    let paths = export::paths_for_book(&pool, &b.id).await.expect("paths");
    assert_eq!(
        book::delete_book_unless_busy(&pool, &b.id, now_ms())
            .await
            .expect("delete"),
        Ok(())
    );
    assert_eq!(
        export::list_exports(&pool).await.expect("list").len(),
        1,
        "export.book_id is ON DELETE SET NULL, not cascade, so the row outlives the book"
    );

    export::purge_orphaned_paths(&pool, &exports_dir, paths)
        .await
        .expect("purge");

    assert!(
        export::list_exports(&pool).await.expect("list").is_empty(),
        "the Audios shelf would otherwise list a playable-looking entry with no file"
    );
    assert!(!audio.exists(), "and the file is gone too");
}

#[tokio::test]
async fn an_epub_imported_before_the_reflowed_column_is_backfilled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePool::connect(&url).await.expect("connect");

    for migration in storage::MIGRATOR
        .iter()
        .filter(|migration| migration.version <= 9)
    {
        sqlx::query(&migration.sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("migration {}: {error}", migration.version));
    }

    sqlx::query(
        "INSERT INTO book (id, title, source_path, format, content_hash, created_at)
         VALUES ('legacy', 'Old EPUB', '/books/old.epub', 'epub', 'hash', 0)",
    )
    .execute(&pool)
    .await
    .expect("insert a book as 0009 would have left it");
    let before: i64 = sqlx::query_scalar("SELECT reflowed FROM book WHERE id = 'legacy'")
        .fetch_one(&pool)
        .await
        .expect("read");
    assert_eq!(before, 0, "0009 defaults it to 0, which is the bug");

    let m0010 = storage::MIGRATOR
        .iter()
        .find(|migration| migration.version == 10)
        .expect("0010 exists");
    sqlx::query(&m0010.sql)
        .execute(&pool)
        .await
        .expect("apply 0010");

    let after: i64 = sqlx::query_scalar("SELECT reflowed FROM book WHERE id = 'legacy'")
        .fetch_one(&pool)
        .await
        .expect("read");
    assert_eq!(
        after, 1,
        "without 0010's backfill an existing EPUB is offered a layout pass that \
         structurally cannot set layout_pass_ran, so the menu item never goes away and \
         every attempt costs hours"
    );
}

#[tokio::test]
async fn a_resumed_holder_cannot_resurrect_a_lease_that_aged_out() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");

    let long_ago = now_ms() - book::LEASE_STALE_MS - 1;
    book::claim_extraction(&pool, &b.id, "suspended", long_ago)
        .await
        .expect("claim")
        .expect("won");

    assert!(
        job::insert_job_unless_extracting(&pool, &job_for(&b.id, &v.id, "SCRIPTING"), now_ms())
            .await
            .expect("insert"),
        "the aged lease reads as free to every other side, so the book is admitted elsewhere"
    );

    assert!(
        !book::refresh_extraction(&pool, &b.id, "suspended", now_ms())
            .await
            .expect("refresh"),
        "extracting_owner still names the suspended holder; refreshing on ownership alone \
         would resurrect a lease that has already been reissued and let two writers run"
    );

    let pages = vec![page_at(&b.id, 1)];
    let published = book::replace_blocks(
        &pool,
        &b.id,
        &pages,
        &[],
        book::Generation {
            blocks_version: "from-the-zombie",
            language: None,
            layout_analysed: Some(true),
            owner: Some("suspended"),
            now: now_ms(),
        },
    )
    .await;
    assert!(
        published.is_err(),
        "and a publish racing ahead of the resumed holder's first heartbeat is refused too"
    );
    let stored = book::get_book(&pool, &b.id)
        .await
        .expect("get")
        .expect("book");
    assert_ne!(stored.blocks_version.as_deref(), Some("from-the-zombie"));
}

#[tokio::test]
async fn a_publish_from_a_superseded_pass_is_refused_and_changes_nothing() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    book::insert_chapter(&pool, &chapter_at(&b.id, 0, 1, 10))
        .await
        .expect("chapter");
    for page_no in 1..=10 {
        book::insert_page(&pool, &page_at(&b.id, page_no))
            .await
            .expect("page");
    }

    book::claim_extraction(&pool, &b.id, "pass-a", now_ms())
        .await
        .expect("claim")
        .expect("won");
    book::release_extraction(&pool, &b.id, "pass-a")
        .await
        .expect("release");
    book::claim_extraction(&pool, &b.id, "pass-b", now_ms())
        .await
        .expect("claim")
        .expect("won");

    let before_version = book::get_book(&pool, &b.id)
        .await
        .expect("get")
        .expect("book")
        .blocks_version;
    let before_pages = book::list_pages(&pool, &b.id).await.expect("pages").len();
    let before_chapters = book::list_chapters(&pool, &b.id).await.expect("ch").len();

    let result = book::replace_blocks(
        &pool,
        &b.id,
        &[page_at(&b.id, 1)],
        &[],
        book::Generation {
            blocks_version: "stale-generation",
            language: Some("de"),
            layout_analysed: Some(true),
            owner: Some("pass-a"),
            now: now_ms(),
        },
    )
    .await;

    assert!(result.is_err(), "pass-a no longer holds the lease");
    let after = book::get_book(&pool, &b.id)
        .await
        .expect("get")
        .expect("book");
    assert_eq!(after.blocks_version, before_version);
    assert_ne!(after.language.as_deref(), Some("de"));
    assert_eq!(
        book::list_pages(&pool, &b.id).await.expect("pages").len(),
        before_pages,
        "the whole transaction has to roll back, not just the final update: the destructive \
         page delete runs before the fence is evaluated"
    );
    assert_eq!(
        book::list_chapters(&pool, &b.id).await.expect("ch").len(),
        before_chapters
    );
}

#[tokio::test]
async fn stealing_the_lease_cancels_the_pass_that_held_it() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");

    let almost_stale = now_ms() - book::LEASE_STALE_MS + 200;
    book::claim_extraction(&pool, &b.id, "holder", almost_stale)
        .await
        .expect("claim")
        .expect("won");

    let (handle, receiver) = txtcaster_lib::orchestrator::stage::control_channel();
    let lease = txtcaster_lib::library::ExtractionLease::hold(&pool, &b.id, "holder", Some(handle));

    book::release_extraction(&pool, &b.id, "holder")
        .await
        .expect("release");
    book::claim_extraction(&pool, &b.id, "thief", now_ms())
        .await
        .expect("claim")
        .expect("won");

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(book::LEASE_REFRESH_MS * 3);
    while receiver.interruption().is_none() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(
        receiver.interruption().is_some(),
        "the heartbeat has to stop the pass once its lease is gone, or the pass runs on and \
         only the publish fence catches it"
    );
    assert!(
        lease.was_lost(),
        "and the loss must be distinguishable from the user pressing Stop"
    );
}

#[tokio::test]
async fn a_fenced_publish_reports_a_code_the_ui_can_match_on() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    book::claim_extraction(&pool, &b.id, "holder", now_ms())
        .await
        .expect("claim")
        .expect("won");

    let error = book::replace_blocks(
        &pool,
        &b.id,
        &[page_at(&b.id, 1)],
        &[],
        book::Generation {
            blocks_version: "nope",
            language: None,
            layout_analysed: Some(true),
            owner: Some("someone-else"),
            now: now_ms(),
        },
    )
    .await
    .expect_err("fenced");

    let surfaced = match error {
        sqlx::Error::Protocol(message) => message,
        other => other.to_string(),
    };
    assert!(
        surfaced.starts_with("ERR_LEASE_LOST"),
        "the UI prefix-matches this; a Display wrapper round the code makes every fenced \
         publish fall through to the generic failure copy: {surfaced}"
    );
}

#[tokio::test]
async fn suppressed_block_round_trip_sorts_uncertain_first_and_cascades_with_its_job() {
    let (_dir, pool) = test_pool().await;
    let job_id = seeded_job(&pool).await;

    let rows = vec![
        job::SuppressedBlock {
            id: new_id(),
            job_id: job_id.clone(),
            page_no: 7,
            role: "header".into(),
            reason: "repetition".into(),
            certainty: "certain".into(),
            text: "RUNNING HEAD".into(),
            words: 2,
        },
        job::SuppressedBlock {
            id: new_id(),
            job_id: job_id.clone(),
            page_no: 3,
            role: "footer".into(),
            reason: "layout-label".into(),
            certainty: "uncertain".into(),
            text: "Reads like prose but the model called it furniture".into(),
            words: 9,
        },
    ];
    job::insert_suppressed_blocks(&pool, &rows)
        .await
        .expect("insert");

    let listed = job::list_suppressed_blocks(&pool, &job_id)
        .await
        .expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[0].certainty, "uncertain",
        "uncertain rows sort first: a model-vs-heuristic disagreement is the row a reader must check"
    );
    assert_eq!(listed[0], rows[1]);
    assert_eq!(listed[1], rows[0]);

    job::update_job_state(&pool, &job_id, "CANCELLED")
        .await
        .expect("settle");
    assert!(job::delete_job_if_settled(&pool, &job_id)
        .await
        .expect("delete"));
    assert_eq!(
        job::list_suppressed_blocks(&pool, &job_id)
            .await
            .expect("list after delete"),
        vec![],
        "the audit is job-scoped: deleting the job removes its suppression records"
    );
}

#[tokio::test]
async fn migration_0031_clears_every_dedicated_vision_model_reference() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    let v = sample_voice();
    book::insert_book(&pool, &b).await.expect("book");
    voice::insert_voice_profile(&pool, &v).await.expect("voice");
    settings::set_setting(&pool, "default_model.vlm", "qwen3-vl-4b-q4km")
        .await
        .expect("vlm default");
    settings::set_setting(&pool, "default_model.llm", "gemma4-e2b-q4km")
        .await
        .expect("llm default");
    let cloud = r#"{"kind":"connection","connection_id":"c","model":"m","provider":"gemini","endpoint":"e","adapter_rev":"gemini/1"}"#;
    let cases = [
        (r#"{"vlm":"qwen3-vl-4b-q4km","llm":"x"}"#, r#"{"llm":"x"}"#),
        (r#"{"vlm":null}"#, r#"{"explain_figures":false}"#),
        (r#"{"vision_binding":null}"#, r#"{"explain_figures":false}"#),
        (
            r#"{"vision_binding":{"kind":"local","model_id":"moondream-small"}}"#,
            r#"{}"#,
        ),
    ];
    let mut ids = Vec::new();
    for (before, _) in cases {
        let mut record = job_for(&b.id, &v.id, "QUEUED");
        record.model_overrides_json = Some(before.into());
        job::insert_job(&pool, &record).await.expect("job");
        ids.push(record.id);
    }
    let mut cloud_job = job_for(&b.id, &v.id, "QUEUED");
    cloud_job.model_overrides_json = Some(format!(r#"{{"vision_binding":{cloud}}}"#));
    job::insert_job(&pool, &cloud_job).await.expect("cloud job");

    let m0031 = storage::MIGRATOR
        .iter()
        .find(|migration| migration.version == 31)
        .expect("0031 exists");
    sqlx::raw_sql(&m0031.sql)
        .execute(&pool)
        .await
        .expect("apply 0031");

    for (id, (_, after)) in ids.iter().zip(cases) {
        let stored = job::get_job(&pool, id).await.expect("read").expect("job");
        let stored: serde_json::Value =
            serde_json::from_str(stored.model_overrides_json.as_deref().unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(after).unwrap();
        assert_eq!(stored, expected);
    }
    let stored = job::get_job(&pool, &cloud_job.id)
        .await
        .expect("read")
        .expect("job");
    let stored: serde_json::Value =
        serde_json::from_str(stored.model_overrides_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        stored["vision_binding"]["kind"], "connection",
        "a recorded cloud binding replays verbatim"
    );
    assert_eq!(
        settings::get_setting(&pool, "default_model.vlm")
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        settings::get_setting(&pool, "default_model.llm")
            .await
            .expect("read")
            .as_deref(),
        Some("gemma4-e2b-q4km")
    );
}

#[tokio::test]
async fn migration_0032_clears_purged_tts_defaults_and_unused_presets() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let profile = |id: &str, engine: &str, kind: &str| voice::VoiceProfile {
        id: id.into(),
        engine: engine.into(),
        kind: kind.into(),
        ..sample_voice()
    };
    let unused_preset = profile("preset-xtts-a", "xtts-v2", "preset");
    let used_preset = profile("preset-xtts-b", "xtts-v2", "preset");
    let cloned = profile("cloned-xtts", "xtts-v2", "cloned");
    let piper = profile("preset-piper", "piper-en-us-lessac-medium", "preset");
    let orpheus = profile("preset-orpheus", "orpheus-3b", "preset");
    let kokoro = profile("preset-kokoro", "kokoro-82m", "preset");
    for p in [
        &unused_preset,
        &used_preset,
        &cloned,
        &piper,
        &orpheus,
        &kokoro,
    ] {
        voice::insert_voice_profile(&pool, p).await.expect("voice");
    }
    job::insert_job(&pool, &job_for(&b.id, &used_preset.id, "DONE"))
        .await
        .expect("job");
    settings::set_setting(&pool, "default_model.tts", "xtts-v2")
        .await
        .expect("tts default");
    settings::set_setting(&pool, "default_model.llm", "gemma4-e2b-q4km")
        .await
        .expect("llm default");

    let m0032 = storage::MIGRATOR
        .iter()
        .find(|migration| migration.version == 32)
        .expect("0032 exists");
    sqlx::raw_sql(&m0032.sql)
        .execute(&pool)
        .await
        .expect("apply 0032");

    let mut left: Vec<String> = voice::list_voice_profiles(&pool)
        .await
        .expect("list")
        .into_iter()
        .map(|p| p.id)
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec!["cloned-xtts", "preset-kokoro", "preset-xtts-b"],
        "unused purged presets go; a preset a job still names and every cloned voice stay"
    );
    assert_eq!(
        settings::get_setting(&pool, "default_model.tts")
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        settings::get_setting(&pool, "default_model.llm")
            .await
            .expect("read")
            .as_deref(),
        Some("gemma4-e2b-q4km")
    );
}

#[tokio::test]
async fn migration_0033_drops_unused_kokoro_presets_outside_the_desktop_languages() {
    let (_dir, pool) = test_pool().await;
    let b = sample_book();
    book::insert_book(&pool, &b).await.expect("book");
    let profile = |id: &str, engine: &str, kind: &str, preset: &str| voice::VoiceProfile {
        id: id.into(),
        engine: engine.into(),
        kind: kind.into(),
        preset_voice_id: Some(preset.into()),
        ..sample_voice()
    };
    let rows = [
        profile("preset-kokoro-ef-dora", "kokoro-82m", "preset", "ef_dora"),
        profile("preset-kokoro-ff-siwis", "kokoro-82m", "preset", "ff_siwis"),
        profile("preset-kokoro-hm-psi", "kokoro-82m", "preset", "hm_psi"),
        profile("preset-kokoro-if-sara", "kokoro-82m", "preset", "if_sara"),
        profile("preset-kokoro-pm-alex", "kokoro-82m", "preset", "pm_alex"),
        profile("preset-kokoro-af-heart", "kokoro-82m", "preset", "af_heart"),
        profile("preset-kokoro-bf-emma", "kokoro-82m", "preset", "bf_emma"),
        profile("preset-kokoro-jf-alpha", "kokoro-82m", "preset", "jf_alpha"),
        profile("preset-kokoro-zm-yunxi", "kokoro-82m", "preset", "zm_yunxi"),
        profile(
            "preset-chatterbox-ef",
            "chatterbox-0.5b",
            "preset",
            "ef_dora",
        ),
        profile("cloned-kokoro-ef", "kokoro-82m", "cloned", "ef_dora"),
    ];
    for p in &rows {
        voice::insert_voice_profile(&pool, p).await.expect("voice");
    }
    job::insert_job(&pool, &job_for(&b.id, "preset-kokoro-pm-alex", "DONE"))
        .await
        .expect("job");

    let m0033 = storage::MIGRATOR
        .iter()
        .find(|migration| migration.version == 33)
        .expect("0033 exists");
    sqlx::raw_sql(&m0033.sql)
        .execute(&pool)
        .await
        .expect("apply 0033");

    let mut left: Vec<String> = voice::list_voice_profiles(&pool)
        .await
        .expect("list")
        .into_iter()
        .map(|p| p.id)
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec![
            "cloned-kokoro-ef",
            "preset-chatterbox-ef",
            "preset-kokoro-af-heart",
            "preset-kokoro-bf-emma",
            "preset-kokoro-jf-alpha",
            "preset-kokoro-pm-alex",
            "preset-kokoro-zm-yunxi",
        ],
        "unused es/fr/hi/it/pt presets go; a preset a job names and every other row stay"
    );
}

#[tokio::test]
async fn migration_0034_rerenders_pdf_pages_drawn_before_pdfium_was_serialized() {
    let (_dir, pool) = test_pool().await;
    let pdf = sample_book();
    let epub = book::Book {
        id: new_id(),
        format: "epub".into(),
        source_path: "/books/os.epub".into(),
        content_hash: "def456".into(),
        ..sample_book()
    };
    let mut page_ids = Vec::new();
    for b in [&pdf, &epub] {
        book::insert_book(&pool, b).await.expect("book");
        let page = book::Page {
            id: new_id(),
            book_id: b.id.clone(),
            page_no: 1,
            width_pt: Some(595.0),
            height_pt: Some(842.0),
            render_hash: None,
            text_json: None,
            needs_upgrade: false,
        };
        book::insert_page(&pool, &page).await.expect("page");
        book::set_page_render_hash(&pool, &page.id, "deadbeef")
            .await
            .expect("hash");
        page_ids.push(page.id);
    }

    let m0034 = storage::MIGRATOR
        .iter()
        .find(|migration| migration.version == 34)
        .expect("0034 exists");
    sqlx::raw_sql(&m0034.sql)
        .execute(&pool)
        .await
        .expect("apply 0034");

    let hash = |book_id: String| {
        let pool = pool.clone();
        async move {
            book::get_page(&pool, &book_id, 1)
                .await
                .expect("get")
                .expect("some")
                .render_hash
        }
    };
    assert_eq!(
        hash(pdf.id.clone()).await,
        None,
        "a PDF page drawn while PDFium ran on several threads may be blank and is drawn again"
    );
    assert_eq!(
        hash(epub.id.clone()).await.as_deref(),
        Some("deadbeef"),
        "EPUB pages never went through PDFium and keep their render"
    );
}
