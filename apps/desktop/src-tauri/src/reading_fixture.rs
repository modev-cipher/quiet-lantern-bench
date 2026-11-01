use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::storage::{book, chapter_render, export, job, now_ms, voice, SqlitePool};
use crate::words::{Timebase, WordsJson};

pub const SEED_ENV: &str = "TXTCASTER_SEED_READING_FIXTURE";

#[derive(Debug, Deserialize)]
pub struct SeedWord {
    pub w: String,
    pub s: i64,
    pub e: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedSegment {
    pub segment_id: String,
    pub page_no: i64,
    pub duration_ms: i64,
    pub measured_onset_ms: i64,
    pub words: Vec<SeedWord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedChapter {
    pub chapter_id: String,
    pub idx: i64,
    pub title: String,
    pub artifact: String,
    pub encoded_ms: i64,
    pub segments: Vec<SeedSegment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Seed {
    pub book_id: String,
    pub job_id: String,
    pub title: String,
    pub book: String,
    pub sample_shape: String,
    pub chapters: Vec<SeedChapter>,
}

pub fn load(path: &Path) -> Result<Seed, String> {
    let body = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "cannot read reading fixture seed {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_str(&body).map_err(|error| {
        format!(
            "cannot parse reading fixture seed {}: {error}",
            path.display()
        )
    })
}

fn export_path(exports_dir: &Path, seed: &Seed, source: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(exports_dir)
        .map_err(|error| format!("cannot create exports dir: {error}"))?;
    // The seed file is data, and `book_id` becomes a filename: `resolve_within`
    // cannot be used because the file does not exist yet, but
    // `resolve_new_file_within` is the same containment rule for a write, and it
    // refuses a `book_id` that is not a single path component. Without it a
    // `bookId` of `../escaped` lands the copy outside the exports root —
    // `Path::join` honours `..` — which is the one export write that had no
    // containment check at all.
    let target = export::resolve_new_file_within(exports_dir, &format!("{}.mp3", seed.book_id))?;
    std::fs::copy(source, &target)
        .map_err(|error| format!("cannot place fixture export: {error}"))?;
    Ok(target)
}

pub async fn seed(
    pool: &SqlitePool,
    seed_path: &Path,
    exports_dir: &Path,
) -> Result<PathBuf, String> {
    let seed = load(seed_path)?;
    let root = seed_path
        .parent()
        .ok_or_else(|| "the seed file has no directory".to_string())?;
    let source = root.join(&seed.book);
    let placed = export_path(exports_dir, &seed, &source)?;

    let total_pages = seed.chapters.len() as i64;
    let record = book::Book {
        id: seed.book_id.clone(),
        title: seed.title.clone(),
        author: Some("Reading Clock".into()),
        source_path: source.to_string_lossy().into_owned(),
        format: "pdf".into(),
        page_count: Some(total_pages),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: format!("{}-hash", seed.book_id),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &record)
        .await
        .map_err(|error| error.to_string())?;

    voice::insert_voice_profile(
        pool,
        &voice::VoiceProfile {
            id: "voice-reading-clock".into(),
            name: "Fixture".into(),
            engine: "kokoro-82m".into(),
            kind: "preset".into(),
            preset_voice_id: Some("af_heart".into()),
            reference_wav_path: None,
            params_json: None,
            created_at: now_ms(),
        },
    )
    .await
    .map_err(|error| error.to_string())?;

    job::insert_job(
        pool,
        &job::RenderJob {
            id: seed.job_id.clone(),
            book_id: seed.book_id.clone(),
            scope_json: format!(r#"{{"kind":"pages","page_start":1,"page_end":{total_pages}}}"#),
            mode: "narrator".into(),
            voice_profile_id: "voice-reading-clock".into(),
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
            output_paths_json: Some(
                serde_json::to_string(&[placed.to_string_lossy().into_owned()])
                    .map_err(|error| error.to_string())?,
            ),
            tts_seed: None,
            verify: false,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    )
    .await
    .map_err(|error| error.to_string())?;

    let mut segments = Vec::new();
    let mut rows = Vec::new();
    let mut index = 0i64;
    for chapter in &seed.chapters {
        book::insert_chapter(
            pool,
            &book::Chapter {
                id: chapter.chapter_id.clone(),
                book_id: seed.book_id.clone(),
                idx: chapter.idx,
                title: chapter.title.clone(),
                page_start: chapter.idx + 1,
                page_end: chapter.idx + 1,
                is_pseudo: false,
                word_count: Some(chapter.segments.len() as i64 * 3),
            },
        )
        .await
        .map_err(|error| error.to_string())?;

        for segment in &chapter.segments {
            segments.push(job::ScriptSegment {
                id: segment.segment_id.clone(),
                job_id: seed.job_id.clone(),
                chapter_id: chapter.chapter_id.clone(),
                idx: index,
                page_no: segment.page_no,
                kind: "text".into(),
                figure_id: None,
                text: segment
                    .words
                    .iter()
                    .map(|word| word.w.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                source_span_json: None,
                flags_json: None,
                audio_hash: Some(format!("hash-{}", segment.segment_id)),
                duration_ms: Some(segment.duration_ms),
                original_text: None,
            });
            index += 1;
        }

        rows.push(chapter_render::ChapterRender {
            job_id: seed.job_id.clone(),
            chapter_id: chapter.chapter_id.clone(),
            idx: chapter.idx,
            hash: format!("hash-{}", chapter.chapter_id),
            artifact: chapter_render::ARTIFACT_MP3.into(),
            duration_ms: chapter.encoded_ms,
            encoded_ms: Some(chapter.encoded_ms),
        });
    }

    job::insert_segments(pool, &segments)
        .await
        .map_err(|error| error.to_string())?;

    for chapter in &seed.chapters {
        for segment in &chapter.segments {
            let words: Vec<serde_json::Value> = segment
                .words
                .iter()
                .map(|word| serde_json::json!({"w": word.w, "s": word.s, "e": word.e}))
                .collect();
            job::upsert_alignment(
                pool,
                &job::Alignment {
                    segment_id: segment.segment_id.clone(),
                    source: "whisperx".into(),
                    words_json: WordsJson::declare(&words, Timebase::Trimmed)
                        .map_err(|error| error.to_string())?,
                    wer: Some(0.0),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        }
    }

    chapter_render::replace_all(pool, &seed.job_id, &rows)
        .await
        .map_err(|error| error.to_string())?;

    export::record(
        pool,
        &seed.book_id,
        &seed.job_id,
        &placed.to_string_lossy(),
        "mp3",
        "narrator",
        &chapter_render::digest(&rows),
    )
    .await
    .map_err(|error| error.to_string())?;

    tracing::info!(
        "reading fixture seeded: {} chapters, {} segments, sample shape: {}",
        seed.chapters.len(),
        segments.len(),
        seed.sample_shape
    );
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_json(book_id: &str) -> String {
        serde_json::json!({
            "bookId": book_id,
            "jobId": "job-fixture",
            "title": "Timeline Fixture",
            "book": "fixture.mp3",
            "sampleShape": "unit-test",
            "chapters": [
                {
                    "chapterId": "ch-a",
                    "idx": 0,
                    "title": "Alpha",
                    "artifact": "mp3",
                    "encodedMs": 3000,
                    "segments": [
                        {
                            "segmentId": "a-0",
                            "pageNo": 1,
                            "durationMs": 1000,
                            "measuredOnsetMs": 0,
                            "words": [{ "w": "hello", "s": 0, "e": 250 }],
                        },
                    ],
                },
            ],
        })
        .to_string()
    }

    /// The seed file is data and `bookId` becomes a filename, so a `..` in it
    /// must be refused rather than joined: `Path::join` honours `..`, and this
    /// write had no containment check at all.
    #[tokio::test]
    async fn a_book_id_that_escapes_the_exports_root_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        let seed_path = dir.path().join("seed.json");
        std::fs::write(&seed_path, seed_json("../escaped")).unwrap();
        std::fs::write(dir.path().join("fixture.mp3"), b"not really mp3").unwrap();

        let error = seed(&pool, &seed_path, &exports)
            .await
            .expect_err("a `..` in the book id must not be joined onto the exports root");

        assert!(
            error.contains("single path component"),
            "unexpected error: {error}"
        );
        assert!(
            !dir.path().join("escaped.mp3").exists(),
            "the write must be refused, not performed: the unchecked name resolved to \
             `<exports>/../escaped.mp3`, which is inside the data directory and outside the \
             exports root — `Path::join` honours `..`, and this write had no containment \
             check at all"
        );
        assert!(
            !exports.join("../escaped.mp3").exists(),
            "nothing at all was written"
        );
    }

    #[tokio::test]
    async fn an_ordinary_book_id_still_seeds_inside_the_exports_root() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        let seed_path = dir.path().join("seed.json");
        std::fs::write(&seed_path, seed_json("book-fixture")).unwrap();
        std::fs::write(dir.path().join("fixture.mp3"), b"not really mp3").unwrap();

        let placed = seed(&pool, &seed_path, &exports).await.expect("seeds");

        assert_eq!(
            placed.parent().unwrap(),
            exports.canonicalize().unwrap(),
            "the fixture export lands in the exports root, the same place every other export \
             does"
        );
        assert_eq!(placed.file_name().unwrap(), "book-fixture.mp3");
        let row = export::get_by_path(&pool, &placed.to_string_lossy())
            .await
            .unwrap()
            .expect("the export has a row, so adoption cannot pick it up unattributed");
        assert_eq!(row.book_id.as_deref(), Some("book-fixture"));
    }
}
