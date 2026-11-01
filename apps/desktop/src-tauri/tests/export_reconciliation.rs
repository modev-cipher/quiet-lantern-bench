use txtcaster_lib::storage::{
    self, book, chapter_render, export, job, new_id, now_ms, voice, SqlitePool,
};

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

async fn seed(pool: &SqlitePool) -> (String, String) {
    let b = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(4),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: "hash".into(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &b).await.expect("book");
    for idx in 0..2 {
        let chapter = book::Chapter {
            id: format!("chap-{idx}"),
            book_id: b.id.clone(),
            idx,
            title: format!("Chapter {idx}"),
            page_start: 1 + idx * 2,
            page_end: 2 + idx * 2,
            is_pseudo: false,
            word_count: Some(10),
        };
        book::insert_chapter(pool, &chapter).await.expect("chapter");
    }
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
        .expect("voice");
    let record = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":4}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: profile.id,
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
    job::insert_job(pool, &record).await.expect("job");
    (b.id, record.id)
}

fn rows(job_id: &str, hashes: &[&str]) -> Vec<chapter_render::ChapterRender> {
    hashes
        .iter()
        .enumerate()
        .map(|(idx, hash)| chapter_render::ChapterRender {
            job_id: job_id.into(),
            chapter_id: format!("chap-{idx}"),
            idx: idx as i64,
            hash: (*hash).into(),
            artifact: chapter_render::ARTIFACT_MP3.into(),
            duration_ms: 1000,
            encoded_ms: Some(1038),
        })
        .collect()
}

#[tokio::test]
async fn an_export_is_fresh_only_while_it_matches_the_chapters_it_was_rendered_from() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    let published = rows(&job_id, &["h0", "h1"]);
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/fixture.mp3",
        "mp3",
        "narrator",
        &chapter_render::digest(&published),
    )
    .await
    .expect("record");

    let row = export::get_by_path(&pool, "/tmp/fixture.mp3")
        .await
        .expect("get")
        .expect("row");
    assert!(
        !export::is_stale(
            row.chapter_digest.as_deref(),
            &export::chapter_evidence(&pool, Some(&job_id))
                .await
                .expect("evidence")
        ),
        "an untouched job must not be asked to re-render"
    );

    let edited = rows(&job_id, &["h0", "h1-edited"]);
    chapter_render::replace_all(&pool, &job_id, &edited)
        .await
        .expect("rows");
    assert!(
        export::is_stale(
            row.chapter_digest.as_deref(),
            &export::chapter_evidence(&pool, Some(&job_id))
                .await
                .expect("evidence")
        ),
        "an edit that rebuilds a chapter must invalidate the export it no longer describes"
    );
}

#[tokio::test]
async fn an_export_that_never_recorded_its_chapters_cannot_be_proven_fresh() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    let published = rows(&job_id, &["h0", "h1"]);
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    sqlx::query(
        "INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, chapter_digest,
                             created_at)
         VALUES (?, ?, ?, ?, 0, 'mp3', 'narrator', NULL, ?)",
    )
    .bind(new_id())
    .bind(&book_id)
    .bind(&job_id)
    .bind("/tmp/pre-mechanism.mp3")
    .bind(now_ms())
    .execute(&pool)
    .await
    .expect("legacy export");

    let row = export::get_by_path(&pool, "/tmp/pre-mechanism.mp3")
        .await
        .expect("get")
        .expect("row");
    assert!(
        export::is_stale(
            row.chapter_digest.as_deref(),
            &export::chapter_evidence(&pool, Some(&job_id))
                .await
                .expect("evidence")
        ),
        "an export rendered before the digest existed carries unknown chapter markers, so it \
         must be offered for re-render rather than trusted"
    );
}

#[tokio::test]
async fn a_job_with_no_chapter_rows_is_never_called_stale() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    sqlx::query(
        "INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, chapter_digest,
                             created_at)
         VALUES (?, ?, ?, ?, 0, 'mp3', 'narrator', NULL, ?)",
    )
    .bind(new_id())
    .bind(&book_id)
    .bind(&job_id)
    .bind("/tmp/pre-a2.mp3")
    .bind(now_ms())
    .execute(&pool)
    .await
    .expect("legacy export");

    let row = export::get_by_path(&pool, "/tmp/pre-a2.mp3")
        .await
        .expect("get")
        .expect("row");
    assert!(
        !export::is_stale(
            row.chapter_digest.as_deref(),
            &export::chapter_evidence(&pool, Some(&job_id))
                .await
                .expect("evidence")
        ),
        "a job that predates chapter rows offers no evidence of staleness, and nagging every \
         library entry that predates A2 would train the user to ignore the warning"
    );
}

#[tokio::test]
async fn a_stale_export_has_no_playable_path_and_a_fresh_one_does() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    let published = rows(&job_id, &["h0", "h1"]);
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/playable.mp3",
        "mp3",
        "narrator",
        &chapter_render::digest(&published),
    )
    .await
    .expect("record");
    assert!(
        !export::is_path_stale(&pool, "/tmp/playable.mp3")
            .await
            .expect("check"),
        "a fresh export must stay playable"
    );
    assert!(
        !export::is_path_stale(&pool, "/tmp/not-an-export.mp3")
            .await
            .expect("check"),
        "a path with no export row is not this mechanism's business"
    );

    chapter_render::replace_all(&pool, &job_id, &rows(&job_id, &["h0", "h1-edited"]))
        .await
        .expect("rows");
    assert!(
        export::is_path_stale(&pool, "/tmp/playable.mp3")
            .await
            .expect("check"),
        "an export whose chapters moved must be refused, not served with markers that \
         disagree with its audio"
    );
}

#[tokio::test]
async fn a_single_chapter_job_whose_only_chapter_row_is_dropped_is_stale_not_amnestied() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    let published = rows(&job_id, &["h0"]);
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/one-chapter.mp3",
        "mp3",
        "narrator",
        &chapter_render::digest(&published),
    )
    .await
    .expect("record");
    assert!(!export::is_path_stale(&pool, "/tmp/one-chapter.mp3")
        .await
        .expect("check"));

    chapter_render::delete(&pool, &job_id, "chap-0")
        .await
        .expect("the editor drops the edited chapter's row");
    let evidence = export::chapter_evidence(&pool, Some(&job_id))
        .await
        .expect("evidence");
    assert_eq!(
        evidence,
        export::ChapterEvidence::NoRows,
        "an emptied chapter set is not the same thing as an export with no job"
    );
    assert!(
        export::is_path_stale(&pool, "/tmp/one-chapter.mp3")
            .await
            .expect("check"),
        "a single-chapter job is the whole book, so dropping its only row must invalidate \
         the export rather than fall through the pre-A2 amnesty and serve pre-edit audio"
    );

    let rebuilt = rows(&job_id, &["h0-edited"]);
    chapter_render::replace_all(&pool, &job_id, &rebuilt)
        .await
        .expect("rows");
    assert!(
        export::is_path_stale(&pool, "/tmp/one-chapter.mp3")
            .await
            .expect("check"),
        "re-encoding the chapter is not enough: the export still predates the new audio"
    );
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/one-chapter.mp3",
        "mp3",
        "narrator",
        &chapter_render::digest(&rebuilt),
    )
    .await
    .expect("re-record");
    assert!(
        !export::is_path_stale(&pool, "/tmp/one-chapter.mp3")
            .await
            .expect("check"),
        "a completed re-render republishes the digest and heals the export"
    );
}

#[tokio::test]
async fn an_export_whose_job_was_deleted_stays_serveable() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    let published = rows(&job_id, &["h0"]);
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/outlives-its-job.mp3",
        "mp3",
        "narrator",
        &chapter_render::digest(&published),
    )
    .await
    .expect("record");

    assert!(job::delete_job_if_settled(&pool, &job_id)
        .await
        .expect("delete job"));
    let row = export::get_by_path(&pool, "/tmp/outlives-its-job.mp3")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        row.job_id, None,
        "the export outlives its job through ON DELETE SET NULL"
    );
    assert_eq!(
        export::chapter_evidence(&pool, row.job_id.as_deref())
            .await
            .expect("evidence"),
        export::ChapterEvidence::Unattributed
    );
    assert!(
        !export::is_path_stale(&pool, "/tmp/outlives-its-job.mp3")
            .await
            .expect("check"),
        "deleting the queue record must not make finished audio unplayable"
    );
}

#[tokio::test]
async fn the_batch_evidence_agrees_with_the_single_lookup_including_the_empty_set() {
    let (_dir, pool) = test_pool().await;
    let (book_id, job_id) = seed(&pool).await;
    let published = rows(&job_id, &["h0", "h1"]);
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    export::record(
        &pool,
        &book_id,
        &job_id,
        "/tmp/batch.mp3",
        "mp3",
        "narrator",
        &chapter_render::digest(&published),
    )
    .await
    .expect("record");

    let ids = [job_id.as_str()];
    let batch = export::evidence_for_jobs(&pool, &ids).await.expect("batch");
    assert_eq!(
        batch.get(&job_id),
        Some(
            &export::chapter_evidence(&pool, Some(&job_id))
                .await
                .expect("single")
        ),
        "the list view and the play path must agree, or one surface serves what the other \
         refuses"
    );

    chapter_render::replace_all(&pool, &job_id, &[])
        .await
        .expect("empty the set");
    let batch = export::evidence_for_jobs(&pool, &ids).await.expect("batch");
    assert_eq!(
        batch.get(&job_id),
        Some(&export::ChapterEvidence::NoRows),
        "a job that lost every row must appear in the batch as NoRows, not be missing from \
         it and read as an unattributed orphan"
    );
    assert_eq!(
        batch.get(&job_id),
        Some(
            &export::chapter_evidence(&pool, Some(&job_id))
                .await
                .expect("single")
        )
    );
}

#[tokio::test]
async fn the_digest_follows_chapter_order_and_membership() {
    let base = chapter_render::digest(&rows("j", &["a", "b"]));
    assert_eq!(base, chapter_render::digest(&rows("j", &["a", "b"])));
    assert_ne!(base, chapter_render::digest(&rows("j", &["b", "a"])));
    assert_ne!(base, chapter_render::digest(&rows("j", &["a", "b", "c"])));
    assert_ne!(base, chapter_render::digest(&rows("j", &["a"])));

    let mut reordered = rows("j", &["a", "b"]);
    reordered.reverse();
    assert_eq!(
        base,
        chapter_render::digest(&reordered),
        "the digest is taken in export order, so row iteration order must not change it"
    );
}
