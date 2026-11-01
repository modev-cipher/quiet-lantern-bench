//! B-0 gate fixtures for the Book Agent retrieval slice (feature `book-agent`).
//! Run: cargo test --features book-agent --test book_agent_retrieval
#![cfg(feature = "book-agent")]

use std::sync::Arc;

use txtcaster_lib::retrieval::contract::{
    self, HandleTable, OpenPassageResult, SearchBookResult, MAX_PAYLOAD_BYTES,
};
use txtcaster_lib::retrieval::{
    fixture, hook, BookFingerprint, Provenance, RetrievalError, RetrievalProvider, SearchOptions,
    SegmentRow, TantivyRetrieval,
};

fn fp(book: &str, job: &str) -> BookFingerprint {
    BookFingerprint::new(book, job, "hash-1", "blocks-1")
}

fn small_corpus() -> Vec<SegmentRow> {
    fixture::generate(0xB00C_A6E7)
        .into_iter()
        .filter(|r| r.idx % 4 == 0 || r.chapter == 9 || (7..=8).contains(&r.chapter))
        .collect()
}

fn built(dir: &std::path::Path) -> (TantivyRetrieval, BookFingerprint) {
    let provider = TantivyRetrieval::new(dir);
    let f = fp("book-a", "job-1");
    provider.index_book(&f, &small_corpus()).expect("build");
    (provider, f)
}

#[test]
fn unindexed_book_is_typed_missing_not_synthetic_hits() {
    let dir = tempfile::tempdir().unwrap();
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("never-indexed", "job-1");
    assert!(!provider.is_indexed(&f));
    match provider.search(&f, &["astrolabe".into()], &SearchOptions::default()) {
        Err(RetrievalError::IndexMissing) => {}
        other => panic!("expected IndexMissing, got {other:?}"),
    }
    // Contract layer maps it to the bounded retrieval_unavailable result.
    let mut handles = HandleTable::new(f);
    let result = contract::search_book(
        &provider,
        &mut handles,
        &["astrolabe".into()],
        &SearchOptions::default(),
    );
    match result {
        SearchBookResult::RetrievalUnavailable { code } => assert_eq!(code, "index_missing"),
        other => panic!("expected retrieval_unavailable, got {other:?}"),
    }
}

#[test]
fn off_book_question_returns_honest_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    let hits = provider
        .search(
            &f,
            &["mitochondria ribosome chlorophyll".into()],
            &SearchOptions::default(),
        )
        .expect("search");
    assert!(
        hits.is_empty(),
        "nonsense query must return zero hits, got {}",
        hits.len()
    );
}

#[test]
fn stale_fingerprint_is_refused_and_lazy_rebuild_detects_it() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    assert!(provider.is_indexed(&f));
    // Same book, new render/generation: stale, never best-effort served.
    let f2 = BookFingerprint::new("book-a", "job-2", "hash-1", "blocks-1");
    assert!(!provider.is_indexed(&f2));
    match provider.search(&f2, &["astrolabe".into()], &SearchOptions::default()) {
        Err(RetrievalError::IndexStale) => {}
        other => panic!("expected IndexStale, got {other:?}"),
    }
    // Rebuild under the new generation; the old one is now the stale one.
    provider.index_book(&f2, &small_corpus()).expect("rebuild");
    assert!(provider.is_indexed(&f2));
    assert!(!provider.is_indexed(&f));
    match provider.search(&f, &["astrolabe".into()], &SearchOptions::default()) {
        Err(RetrievalError::IndexStale) => {}
        other => panic!("expected IndexStale for retired generation, got {other:?}"),
    }
}

#[test]
fn interrupted_build_never_replaces_known_good_index() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    // Simulate a crashed build: a leftover temp dir beside the published index.
    let junk = dir
        .path()
        .join("book-a")
        .join("index")
        .join(".build-crashed");
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::write(junk.join("partial"), b"junk").unwrap();
    assert!(
        provider.is_indexed(&f),
        "published index survives temp junk"
    );
    assert!(!provider
        .search(&f, &["astrolabe".into()], &SearchOptions::default())
        .expect("search")
        .is_empty());
}

#[test]
fn corrupt_published_index_is_typed_not_synthetic() {
    let dir = tempfile::tempdir().unwrap();
    let (_provider, f) = built(dir.path());
    std::fs::write(
        dir.path()
            .join("book-a")
            .join("index")
            .join("current")
            .join("fingerprint.json"),
        b"{not json",
    )
    .unwrap();
    let fresh = TantivyRetrieval::new(dir.path());
    assert!(!fresh.is_indexed(&f));
    match fresh.search(&f, &["astrolabe".into()], &SearchOptions::default()) {
        Err(RetrievalError::Corrupt) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[test]
fn deletion_cleans_the_index_and_lives_inside_the_book_dir() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    // Layout pin: the index lives under books/<book_id>/, so the existing
    // delete_book directory removal cleans it by construction.
    let index_dir = dir.path().join("book-a").join("index");
    assert!(index_dir.join("current").join("fingerprint.json").exists());
    provider.delete_index("book-a").expect("delete");
    assert!(!index_dir.exists());
    assert!(!provider.is_indexed(&f));
    provider.delete_index("book-a").expect("idempotent delete");
}

#[test]
fn chapter_scope_filters_before_ranking_and_matches_chapter_only_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let corpus = small_corpus();
    let provider = TantivyRetrieval::new(dir.path());
    let f_full = fp("book-full", "job-1");
    provider.index_book(&f_full, &corpus).expect("full build");
    let f_ch9 = fp("book-ch9", "job-1");
    let ch9_only: Vec<_> = corpus.iter().filter(|r| r.chapter == 9).cloned().collect();
    provider.index_book(&f_ch9, &ch9_only).expect("ch9 build");

    // "قياس" is common in chapter 9 but also appears code-switched elsewhere;
    // a post-top-k filter would starve the scoped result. Pre-rank scoping
    // must recover the same in-chapter hits as the chapter-only corpus.
    let opts_scoped = SearchOptions {
        chapter: Some(9),
        ..SearchOptions::default()
    };
    let scoped = provider
        .search(&f_full, &["قياس النجوم".into()], &opts_scoped)
        .expect("scoped");
    let chapter_only = provider
        .search(&f_ch9, &["قياس النجوم".into()], &SearchOptions::default())
        .expect("chapter-only");
    // No starvation: the scoped result is full-width and entirely in-chapter,
    // even though the global top ranks are dominated by out-of-chapter rows.
    assert_eq!(
        scoped.len(),
        5,
        "post-top-k filtering would starve this to fewer hits"
    );
    assert!(scoped.iter().all(|h| h.handle.chapter == 9));
    // Head agreement: BM25 collection statistics legitimately differ between
    // the two corpora, so near-tied tail ranks may reorder; the relevant head
    // must match the chapter-only corpus.
    let scoped_ids: Vec<u32> = scoped.iter().take(3).map(|h| h.handle.idx).collect();
    let solo_ids: Vec<u32> = chapter_only.iter().take(3).map(|h| h.handle.idx).collect();
    assert_eq!(
        scoped_ids, solo_ids,
        "scoped head must equal chapter-only corpus head"
    );
}

#[test]
fn unknown_chapter_scope_is_existence_checked() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    let opts = SearchOptions {
        chapter: Some(99),
        ..SearchOptions::default()
    };
    match provider.search(&f, &["astrolabe".into()], &opts) {
        Err(RetrievalError::UnknownChapter { chapter: 99 }) => {}
        other => panic!("expected UnknownChapter, got {other:?}"),
    }
}

#[test]
fn query_variant_contract_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    let opts = SearchOptions::default();
    for (queries, code) in [
        (vec![], "empty"),
        (vec!["a".into(); 5], "too_many"),
        (vec!["ok".into(), "   ".into()], "blank"),
        (vec!["x".repeat(300)], "too_long"),
    ] {
        match provider.search(&f, &queries, &opts) {
            Err(RetrievalError::InvalidQuery { code: c }) => assert_eq!(c, code),
            other => panic!("expected InvalidQuery:{code}, got {other:?}"),
        }
    }
    // Duplicates dedupe rather than double-count RRF contributions.
    let dup = provider
        .search(&f, &["astrolabe".into(), "astrolabe".into()], &opts)
        .expect("dup");
    let single = provider
        .search(&f, &["astrolabe".into()], &opts)
        .expect("single");
    assert_eq!(
        dup.iter().map(|h| h.handle.idx).collect::<Vec<_>>(),
        single.iter().map(|h| h.handle.idx).collect::<Vec<_>>()
    );
}

#[test]
fn arabic_variant_law_singular_alone_misses_broken_plural_but_variants_recover() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    let opts = SearchOptions {
        top_k: 5,
        snippet_max_words: 25,
        chapter: None,
    };
    // Snowball Arabic is a light stemmer: the broken plural الأفلاك is NOT
    // reached from the singular فلك (proven spike miss). The charter's
    // mitigation is separate singular+plural variants; this fixture encodes
    // the expected variant pairs and proves the provider accepts and fuses them.
    let plural_hits = provider
        .search(&f, &["الأفلاك".into()], &opts)
        .expect("plural");
    assert!(
        plural_hits.iter().any(|h| h.snippet.contains("الأفلاك")),
        "plural variant must retrieve the plural form"
    );
    let both = provider
        .search(&f, &["فلك".into(), "الأفلاك".into()], &opts)
        .expect("variant pair");
    assert!(both.iter().any(|h| h.snippet.contains("الأفلاك")));
    // Verbal-noun + verb pair (قياس / يقيسون), same law.
    let verb_pair = provider
        .search(&f, &["قياس".into(), "يقيسون".into()], &opts)
        .expect("verb pair");
    assert!(!verb_pair.is_empty());
}

#[test]
fn code_switched_queries_hit_through_dual_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    let opts = SearchOptions {
        top_k: 5,
        snippet_max_words: 25,
        chapter: None,
    };
    for (query, target) in [
        ("kamal latitude instrument", "الكمال"),
        ("الكمال knotted cord", "الكمال"),
        ("Canopus سهيل southern", "سهيل"),
        ("قياس ارتفاع النجوم بالأسطرلاب", "الأسطرلاب"),
    ] {
        let hits = provider
            .search(&f, &[query.to_string()], &opts)
            .expect("cs");
        assert!(
            hits.iter().any(|h| h.snippet.contains(target)),
            "code-switched probe '{query}' missed '{target}'"
        );
    }
}

#[test]
fn needles_hit_at_rank_one() {
    let dir = tempfile::tempdir().unwrap();
    // Needles are planted in the full corpus; build it whole for this pin.
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("book-needles", "job-1");
    provider
        .index_book(&f, &fixture::generate(0xB00C_A6E7))
        .expect("build");
    for (term, _) in fixture::NEEDLES {
        let hits = provider
            .search(&f, &[term.to_string()], &SearchOptions::default())
            .expect("needle");
        assert!(
            hits.first()
                .map(|h| h.snippet.to_lowercase().contains(term))
                .unwrap_or(false),
            "needle '{term}' not at rank 1"
        );
    }
}

#[test]
fn snippet_caps_hold_under_adversarial_unicode() {
    let dir = tempfile::tempdir().unwrap();
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("book-adv", "job-1");
    // One giant no-whitespace token, RTL overrides, ZWJ runs, and a normal row.
    let hostile = "\u{202e}".to_string() + &"سلام\u{200d}".repeat(4000) + "astrolabe";
    let rows = vec![
        SegmentRow {
            chapter: 1,
            page: 1,
            idx: 0,
            text: hostile,
            provenance: Provenance::SourceFaithful,
        },
        SegmentRow {
            chapter: 1,
            page: 1,
            idx: 1,
            text: "the astrolabe survey ".repeat(400),
            provenance: Provenance::SourceFaithful,
        },
    ];
    provider.index_book(&f, &rows).expect("build");
    let hits = provider
        .search(&f, &["astrolabe".into()], &SearchOptions::default())
        .expect("search");
    for h in &hits {
        assert!(
            h.snippet.len() <= 512 + 8,
            "snippet byte cap violated: {}",
            h.snippet.len()
        );
        assert!(h.snippet.split_whitespace().count() <= 15 + 1);
    }
}

#[test]
fn tool_payload_is_bounded_and_exposes_only_opaque_handles_and_rank() {
    let dir = tempfile::tempdir().unwrap();
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("book-payload", "job-1");
    provider
        .index_book(&f, &fixture::generate(0xB00C_A6E7))
        .expect("build");
    let mut handles = HandleTable::new(f);
    let result = contract::search_book(
        &provider,
        &mut handles,
        &[
            "who refined sight-reduction procedures".into(),
            "sight reduction refinement history".into(),
            "navigators improving sight reduction".into(),
        ],
        &SearchOptions::default(),
    );
    let payload = contract::serialize_bounded(&result);
    assert!(
        payload.len() <= MAX_PAYLOAD_BYTES,
        "payload {}B exceeds {}B ceiling",
        payload.len(),
        MAX_PAYLOAD_BYTES
    );
    // No fused score and no raw idx coordinate crosses the tool boundary.
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let hits = json["hits"].as_array().expect("hits");
    assert!(!hits.is_empty());
    for (i, h) in hits.iter().enumerate() {
        assert!(h.get("score").is_none());
        assert!(h.get("idx").is_none());
        assert_eq!(h["rank"].as_u64().unwrap() as usize, i + 1);
        assert_eq!(
            h["handle"].as_str().unwrap().len(),
            32,
            "opaque 128-bit hex handle"
        );
    }
}

#[test]
fn open_passage_resolves_only_live_handles_and_refuses_generated_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("book-open", "job-1");
    provider
        .index_book(&f, &fixture::generate(0xB00C_A6E7))
        .expect("build");
    let mut handles = HandleTable::new(f.clone());

    // Unknown token: refused, never remapped (T10).
    match contract::open_passage(&provider, &handles, "deadbeefdeadbeefdeadbeefdeadbeef") {
        OpenPassageResult::Invalid { code } => assert_eq!(code, "unknown_handle"),
        other => panic!("expected unknown_handle, got {other:?}"),
    }

    // A real hit opens a bounded, chapter-local window.
    let result = contract::search_book(
        &provider,
        &mut handles,
        &["zerbadar".into()],
        &SearchOptions::default(),
    );
    let SearchBookResult::Ok { hits } = result else {
        panic!("expected hits")
    };
    let token = hits[0].handle.clone();
    match contract::open_passage(&provider, &handles, &token) {
        OpenPassageResult::Ok { text, .. } => {
            assert!(text.to_lowercase().contains("zerbadar"));
            assert!(text.len() <= 64 * 1024);
            assert!(text.split_whitespace().count() <= contract::DEFAULT_WINDOW_WORDS + 8);
        }
        other => panic!("expected window, got {other:?}"),
    }

    // The generated needle is retrievable but refused as an evidence anchor (T9).
    let gen = contract::search_book(
        &provider,
        &mut handles,
        &["velgorath".into()],
        &SearchOptions::default(),
    );
    let SearchBookResult::Ok { hits: gen_hits } = gen else {
        panic!("expected generated hit")
    };
    assert!(
        !gen_hits.is_empty(),
        "generated prose stays retrievable metadata"
    );
    match contract::open_passage(&provider, &handles, &gen_hits[0].handle) {
        OpenPassageResult::Invalid { code } => assert_eq!(code, "generated_not_evidence"),
        other => panic!("generated hit must not open as evidence, got {other:?}"),
    }
}

#[test]
fn parent_window_stops_at_generated_prose_and_chapter_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("book-window", "job-1");
    let rows = vec![
        SegmentRow {
            chapter: 1,
            page: 1,
            idx: 0,
            text: "chapter one tail".into(),
            provenance: Provenance::SourceFaithful,
        },
        SegmentRow {
            chapter: 2,
            page: 2,
            idx: 1,
            text: "generated intro".into(),
            provenance: Provenance::Generated,
        },
        SegmentRow {
            chapter: 2,
            page: 2,
            idx: 2,
            text: "before target".into(),
            provenance: Provenance::SourceFaithful,
        },
        SegmentRow {
            chapter: 2,
            page: 2,
            idx: 3,
            text: "the unique targetword sentence".into(),
            provenance: Provenance::SourceFaithful,
        },
        SegmentRow {
            chapter: 2,
            page: 2,
            idx: 4,
            text: "after target".into(),
            provenance: Provenance::SourceFaithful,
        },
        SegmentRow {
            chapter: 3,
            page: 3,
            idx: 5,
            text: "chapter three head".into(),
            provenance: Provenance::SourceFaithful,
        },
    ];
    provider.index_book(&f, &rows).expect("build");
    let hits = provider
        .search(&f, &["targetword".into()], &SearchOptions::default())
        .expect("search");
    let window = provider
        .parent_window(&f, &hits[0].handle, 1200)
        .expect("window");
    assert!(window.contains("targetword"));
    assert!(window.contains("before target") && window.contains("after target"));
    assert!(
        !window.contains("generated intro"),
        "window must not include generated prose"
    );
    assert!(!window.contains("chapter one tail") && !window.contains("chapter three head"));
}

#[tokio::test]
async fn hook_builds_from_real_script_segment_rows_and_is_lazy() {
    use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice};

    let dir = tempfile::tempdir().unwrap();
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");

    let mut b = book::Book {
        id: new_id(),
        title: "Navigation".into(),
        author: None,
        source_path: "/books/nav.pdf".into(),
        format: "pdf".into(),
        page_count: Some(10),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: None,
        cover_path: None,
        content_hash: "hash-nav".into(),
        blocks_version: Some("blocks-2".into()),
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    b.id = "hook-book".into();
    book::insert_book(&pool, &b).await.expect("book");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 3,
        title: "Latitude".into(),
        page_start: 1,
        page_end: 10,
        is_pseudo: false,
        word_count: None,
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let vp = voice::VoiceProfile {
        id: new_id(),
        name: "V".into(),
        engine: "kokoro-82m".into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_heart".into()),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(&pool, &vp)
        .await
        .expect("voice");
    let job_id = new_id();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: b.id.clone(),
        scope_json: "{}".into(),
        mode: "narrator".into(),
        voice_profile_id: vp.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "scripting".into(),
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
    job::insert_job(&pool, &record).await.expect("job");
    let segments: Vec<job::ScriptSegment> = [
        ("text", "The kamal fixed latitude with a knotted cord."),
        ("heading", "Measuring the pole star"),
        (
            "chapter_intro",
            "This generated intro summarizes the chapter.",
        ),
    ]
    .iter()
    .enumerate()
    .map(|(i, (kind, text))| job::ScriptSegment {
        id: new_id(),
        job_id: job_id.clone(),
        chapter_id: chapter.id.clone(),
        idx: i as i64,
        page_no: 1,
        kind: (*kind).into(),
        figure_id: None,
        text: (*text).into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: None,
        duration_ms: None,
        original_text: None,
    })
    .collect();
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let books_root = dir.path().join("books");
    let provider = Arc::new(TantivyRetrieval::new(&books_root));
    let (fingerprint, stats) = hook::build_for_job(&pool, provider.clone(), &job_id)
        .await
        .expect("build");
    assert_eq!(stats.segments, 3);
    assert_eq!(fingerprint.book_id, "hook-book");
    assert_eq!(fingerprint.blocks_version, "blocks-2");
    assert!(books_root
        .join("hook-book")
        .join("index")
        .join("current")
        .join("fingerprint.json")
        .exists());

    // Search finds real text; chapter label comes from chapter.idx.
    let hits = provider
        .search(
            &fingerprint,
            &["kamal knotted cord".into()],
            &SearchOptions::default(),
        )
        .expect("search");
    assert!(hits[0].snippet.contains("kamal"));
    assert_eq!(hits[0].handle.chapter, 3);
    assert_eq!(hits[0].provenance, Provenance::SourceFaithful);
    // The generated chapter_intro is classed Generated.
    let gen = provider
        .search(
            &fingerprint,
            &["generated intro summarizes".into()],
            &SearchOptions::default(),
        )
        .expect("gen");
    assert_eq!(gen[0].provenance, Provenance::Generated);

    // Lazy path: fresh index short-circuits; unknown job is typed.
    let (_, rebuilt) = hook::ensure_indexed(&pool, provider.clone(), &job_id)
        .await
        .expect("ensure");
    assert!(rebuilt.is_none(), "fresh index must not rebuild");
    assert!(matches!(
        hook::ensure_indexed(&pool, provider, "no-such-job").await,
        Err(hook::HookError::UnknownJob)
    ));
}

#[tokio::test]
async fn teacher_mode_rows_are_generated_never_evidence() {
    use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice};

    let dir = tempfile::tempdir().unwrap();
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");

    let b = book::Book {
        id: "teacher-book".into(),
        title: "Navigation".into(),
        author: None,
        source_path: "/books/nav.pdf".into(),
        format: "pdf".into(),
        page_count: Some(10),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: None,
        cover_path: None,
        content_hash: "hash-teach".into(),
        blocks_version: Some("blocks-2".into()),
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(&pool, &b).await.expect("book");
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 1,
        title: "Latitude".into(),
        page_start: 1,
        page_end: 10,
        is_pseudo: false,
        word_count: None,
    };
    book::insert_chapter(&pool, &chapter)
        .await
        .expect("chapter");
    let vp = voice::VoiceProfile {
        id: new_id(),
        name: "V".into(),
        engine: "kokoro-82m".into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_heart".into()),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(&pool, &vp)
        .await
        .expect("voice");
    let job_id = new_id();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: b.id.clone(),
        scope_json: "{}".into(),
        mode: "teacher".into(),
        voice_profile_id: vp.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "scripting".into(),
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
    job::insert_job(&pool, &record).await.expect("job");
    // In teacher mode even unedited kind='text' rows are LLM explanation.
    let segments: Vec<job::ScriptSegment> = [
        ("text", "The lecturer explains the kamal in modern terms."),
        ("heading", "Measuring the pole star"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (kind, text))| job::ScriptSegment {
        id: new_id(),
        job_id: job_id.clone(),
        chapter_id: chapter.id.clone(),
        idx: i as i64,
        page_no: 1,
        kind: (*kind).into(),
        figure_id: None,
        text: (*text).into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: None,
        duration_ms: None,
        original_text: None,
    })
    .collect();
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let books_root = dir.path().join("books");
    let provider = Arc::new(TantivyRetrieval::new(&books_root));
    let (fingerprint, _) = hook::build_for_job(&pool, provider.clone(), &job_id)
        .await
        .expect("build");
    let hits = provider
        .search(
            &fingerprint,
            &["kamal lecturer explains".into()],
            &SearchOptions::default(),
        )
        .expect("search");
    assert!(!hits.is_empty(), "teacher prose stays retrievable metadata");
    for h in &hits {
        assert_eq!(
            h.provenance,
            Provenance::Generated,
            "every teacher-mode row must be Generated (T9)"
        );
    }
    // And it can never anchor an evidence window.
    assert!(matches!(
        provider.parent_window(&fingerprint, &hits[0].handle, 1200),
        Err(RetrievalError::GeneratedNotEvidence)
    ));
}

#[test]
fn parent_window_caps_an_oversized_center_segment() {
    let dir = tempfile::tempdir().unwrap();
    let provider = TantivyRetrieval::new(dir.path());
    let f = fp("book-bigseg", "job-1");
    // One 200k-word segment and one adversarial no-whitespace 200KB segment.
    let rows = vec![
        SegmentRow {
            chapter: 1,
            page: 1,
            idx: 0,
            text: "colossus paragraph ".repeat(100_000),
            provenance: Provenance::SourceFaithful,
        },
        SegmentRow {
            chapter: 2,
            page: 2,
            idx: 1,
            text: "gluttonwall ".to_string() + &"سلام".repeat(50_000),
            provenance: Provenance::SourceFaithful,
        },
    ];
    provider.index_book(&f, &rows).expect("build");
    let opts = SearchOptions::default();
    let big = provider
        .search(&f, &["colossus".into()], &opts)
        .expect("s1");
    let w = provider
        .parent_window(&f, &big[0].handle, 1200)
        .expect("window 1");
    assert!(w.split_whitespace().count() <= 1200);
    assert!(w.len() <= 64 * 1024);
    let adv = provider
        .search(&f, &["gluttonwall".into()], &opts)
        .expect("s2");
    let w2 = provider
        .parent_window(&f, &adv[0].handle, 1200)
        .expect("window 2");
    assert!(
        w2.len() <= 64 * 1024,
        "byte backstop must hold with no whitespace"
    );
    // Zero-word windows are a typed contract violation, not a panic.
    assert!(matches!(
        provider.parent_window(&f, &big[0].handle, 0),
        Err(RetrievalError::InvalidQuery { .. })
    ));
}

#[test]
fn out_of_range_search_options_are_typed_not_panics() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, f) = built(dir.path());
    for opts in [
        SearchOptions {
            top_k: 0,
            ..SearchOptions::default()
        },
        SearchOptions {
            top_k: 50,
            ..SearchOptions::default()
        },
        SearchOptions {
            snippet_max_words: 0,
            ..SearchOptions::default()
        },
        SearchOptions {
            snippet_max_words: 100,
            ..SearchOptions::default()
        },
    ] {
        assert!(matches!(
            provider.search(&f, &["astrolabe".into()], &opts),
            Err(RetrievalError::InvalidQuery { .. })
        ));
    }
}

#[test]
fn serialize_bounded_never_exceeds_the_ceiling_even_wordless() {
    // Five hits whose snippets are single 512-byte tokens: the trimmer must
    // reduce (halving, then dropping hits) until the ceiling holds.
    let hits = (0..5)
        .map(|i| contract::ToolHit {
            handle: format!("{:032x}", i),
            chapter: 1,
            page: 1,
            rank: i + 1,
            snippet: "x".repeat(512),
        })
        .collect();
    let result = SearchBookResult::Ok { hits };
    let payload = contract::serialize_bounded(&result);
    assert!(
        payload.len() <= MAX_PAYLOAD_BYTES,
        "wordless payload {}B exceeds ceiling",
        payload.len()
    );
    let json: serde_json::Value = serde_json::from_str(&payload).expect("valid json");
    assert_eq!(json["status"], "ok");
}
