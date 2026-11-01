use std::path::PathBuf;

use txtcaster_lib::media_url_for;
use txtcaster_lib::storage::{
    self, book, chapter_render, export, job, new_id, now_ms, voice, SqlitePool,
};
use txtcaster_lib::STALE_EXPORT_MESSAGE;

async fn seed(pool: &SqlitePool) -> (String, String) {
    let b = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(2),
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
    let chapter = book::Chapter {
        id: "chap-0".into(),
        book_id: b.id.clone(),
        idx: 0,
        title: "Chapter 0".into(),
        page_start: 1,
        page_end: 2,
        is_pseudo: false,
        word_count: Some(10),
    };
    book::insert_chapter(pool, &chapter).await.expect("chapter");
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
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":2}"#.into(),
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

fn chapter_rows(job_id: &str, hash: &str) -> Vec<chapter_render::ChapterRender> {
    vec![chapter_render::ChapterRender {
        job_id: job_id.into(),
        chapter_id: "chap-0".into(),
        idx: 0,
        hash: hash.into(),
        artifact: chapter_render::ARTIFACT_MP3.into(),
        duration_ms: 1000,
        encoded_ms: Some(1038),
    }]
}

#[tokio::test]
async fn the_media_url_is_refused_for_a_stale_export_and_granted_for_a_fresh_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let (book_id, job_id) = seed(&pool).await;

    let exports_dir = dir.path().join("exports");
    std::fs::create_dir_all(&exports_dir).expect("exports dir");
    let file = exports_dir.join("fixture.mp3");
    std::fs::write(&file, b"not really an mp3, but a real file").expect("write");
    let path = file.to_string_lossy().to_string();
    let roots: Vec<PathBuf> = vec![exports_dir.clone()];

    let published = chapter_rows(&job_id, "h0");
    chapter_render::replace_all(&pool, &job_id, &published)
        .await
        .expect("rows");
    export::record(
        &pool,
        &book_id,
        &job_id,
        &path,
        "mp3",
        "narrator",
        &chapter_render::digest(&published),
    )
    .await
    .expect("record");

    let server = txtcaster_lib::media_server::start("test-token".into())
        .await
        .expect("media server");

    let url = media_url_for(&pool, &server, &roots, &path)
        .await
        .expect("a fresh export must be playable");
    assert!(url.contains("/media/"), "unexpected url: {url}");

    chapter_render::replace_all(&pool, &job_id, &chapter_rows(&job_id, "h0-edited"))
        .await
        .expect("rows");
    let refused = media_url_for(&pool, &server, &roots, &path)
        .await
        .expect_err("a stale export must not get a url");
    assert_eq!(
        refused, STALE_EXPORT_MESSAGE,
        "the refusal must tell the user what to do, not leak an internal error"
    );

    let response = reqwest::get(&url).await.expect("request the old url");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a url handed out before the edit must stop resolving, or the session cache keeps \
         serving the audio the refusal exists to withhold"
    );
}

#[tokio::test]
async fn a_path_that_is_not_an_export_is_still_served() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).expect("dir");
    let file = media.join("preview.wav");
    std::fs::write(&file, b"riff").expect("write");
    let server = txtcaster_lib::media_server::start("test-token-2".into())
        .await
        .expect("media server");
    let url = media_url_for(
        &pool,
        &server,
        std::slice::from_ref(&media),
        &file.to_string_lossy(),
    )
    .await
    .expect("reconciliation must only govern exports");
    assert!(url.contains("/media/"));
}
