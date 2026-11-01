#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use txtcaster_lib::ingest_fallback::{FallbackSource, FALLBACK_ENABLED_KEY};
use txtcaster_lib::library::{self, ImportEvent};
use txtcaster_lib::storage::{self, book, SqlitePool};
use txtcaster_lib::voice::keys::KeyManager;

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn spawn_sse_server(body: String) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
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
                let _ = mock_http::write_response(
                    &mut sock,
                    "200 OK",
                    "text/event-stream",
                    body.as_bytes(),
                )
                .await;
            });
        }
    });
    port
}

fn manifest_json() -> String {
    serde_json::json!({
        "book_id": "ignored",
        "page_count": 4,
        "is_scanned": false,
        "path": "fast",
        "language": "en",
        "title": "Fixture Book Title",
        "author": "Fixture Author",
        "chapters": [
            {"idx": 0, "title": "Chapter 1", "page_start": 1, "page_end": 2,
             "word_count": 500, "is_pseudo": false},
            {"idx": 1, "title": "Chapter 2", "page_start": 3, "page_end": 4,
             "word_count": 480, "is_pseudo": false}
        ],
        "warnings": [],
        "blocks_version": "v1|extract=1|pymupdf=1.28.0|reflow=612x792@11|docling=none",
        "blocks_page_count": 4
    })
    .to_string()
}

fn page_blocks_json() -> String {
    serde_json::json!({
        "pages": (1..=4).map(|page_no| serde_json::json!({
            "page_no": page_no,
            "width_pt": 612.0,
            "height_pt": 792.0,
            "needs_upgrade": page_no == 2,
            "text_json": format!(
                r#"{{"quality":{{"source":"pymupdf","multi_column":false,"low_confidence":{}}},"blocks":[{{"bbox":[0,0,10,10],"text":"page {}","font_size":11.0,"reading_order":0,"column":0}}]}}"#,
                page_no == 2, page_no
            ),
        })).collect::<Vec<_>>()
    })
    .to_string()
}

fn write_fixture_pdf(dir: &std::path::Path) -> std::path::PathBuf {
    let source = dir.join("My Fixture.pdf");
    std::fs::write(&source, b"%PDF-1.4 fixture bytes").expect("write fixture");
    source
}

#[tokio::test]
async fn import_book_persists_book_chapters_pages_and_copies_source() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let body = sse_body(&[
        ("progress", r#"{"stage":"ingest","page":4,"total":4}"#),
        ("manifest", &manifest_json()),
    ]);
    let port = spawn_sse_server(body);
    let books_dir = dir.path().join("books");
    let client = reqwest::Client::new();

    let mut events = Vec::new();
    let result = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &FallbackSource::off(),
        |event| events.push(event),
    )
    .await
    .expect("import");

    let summary = result.book;
    assert!(result.warnings.is_empty());
    assert_eq!(summary.title, "Fixture Book Title");
    assert_eq!(summary.author.as_deref(), Some("Fixture Author"));
    assert_eq!(summary.page_count, Some(4));
    assert_eq!(summary.chapter_count, 2);
    assert!(!summary.has_pseudo_chapters);
    assert!(events.contains(&ImportEvent::Progress {
        page: 4,
        total: 4,
        stage: "ingest".to_string()
    }));
    assert!(events.contains(&ImportEvent::Done {
        book_id: summary.id.clone()
    }));

    let record = book::get_book(&pool, &summary.id)
        .await
        .expect("query")
        .expect("book row");
    assert_eq!(record.format, "pdf");
    assert_eq!(record.ingest_path.as_deref(), Some("fast"));
    let copied = books_dir.join(&summary.id).join("source.pdf");
    assert!(copied.is_file());
    assert_eq!(record.source_path, copied.to_string_lossy());

    let chapters = book::list_chapters(&pool, &summary.id)
        .await
        .expect("chapters");
    assert_eq!(
        chapters
            .iter()
            .map(|c| c.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Chapter 1", "Chapter 2"]
    );
    for page_no in 1..=4 {
        assert!(book::get_page(&pool, &summary.id, page_no)
            .await
            .expect("page query")
            .is_some());
    }
}

#[tokio::test]
async fn import_book_surfaces_manifest_warnings() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_json()).expect("json");
    manifest["warnings"] = serde_json::json!([
        "scanned_pdf_ocr: text extracted with OCR; accuracy may vary",
        "multi_column_upgraded: [2]"
    ]);
    let body = sse_body(&[("manifest", &manifest.to_string())]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let result = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &dir.path().join("books"),
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect("import");
    assert_eq!(
        result.warnings,
        vec![
            "scanned_pdf_ocr: text extracted with OCR; accuracy may vary".to_string(),
            "multi_column_upgraded: [2]".to_string(),
        ]
    );
}

#[tokio::test]
async fn import_book_rejects_duplicate_content_hash() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let body = sse_body(&[("manifest", &manifest_json())]);
    let port = spawn_sse_server(body);
    let books_dir = dir.path().join("books");
    let client = reqwest::Client::new();

    library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect("first import");
    let error = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect_err("duplicate import");
    assert!(error.starts_with("ERR_DUPLICATE"), "{error}");
}

#[tokio::test]
async fn import_book_surfaces_drm_error_and_cleans_up() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let body = sse_body(&[(
        "error",
        r#"{"code":"ERR_DRM","message":"document is encrypted (DRM) and cannot be ingested"}"#,
    )]);
    let port = spawn_sse_server(body);
    let books_dir = dir.path().join("books");
    let client = reqwest::Client::new();

    let error = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect_err("drm import");
    assert!(error.starts_with("ERR_DRM"), "{error}");
    assert!(book::list_books(&pool).await.expect("list").is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(&books_dir)
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "book dir not cleaned up");
}

#[tokio::test]
async fn import_book_falls_back_to_filename_title() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_json()).expect("json");
    manifest["title"] = serde_json::Value::Null;
    manifest["author"] = serde_json::Value::Null;
    let body = sse_body(&[("manifest", &manifest.to_string())]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let summary = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &dir.path().join("books"),
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect("import")
    .book;
    assert_eq!(summary.title, "My Fixture");
    assert_eq!(summary.author, None);
}

#[tokio::test]
async fn attach_cover_copies_from_cache_and_updates_row() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let body = sse_body(&[("manifest", &manifest_json())]);
    let port = spawn_sse_server(body);
    let books_dir = dir.path().join("books");
    let client = reqwest::Client::new();
    let summary = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect("import")
    .book;

    let cache_dir = dir.path().join("cache");
    let hash = "ab".repeat(32);
    let shard_dir = cache_dir.join(&hash[..2]);
    std::fs::create_dir_all(&shard_dir).expect("shard dir");
    std::fs::write(shard_dir.join(&hash), b"png bytes").expect("cache entry");

    let cover_path = library::attach_cover(&pool, &cache_dir, &books_dir, &summary.id, &hash)
        .await
        .expect("cover");
    assert!(std::path::Path::new(&cover_path).is_file());
    let record = book::get_book(&pool, &summary.id)
        .await
        .expect("query")
        .expect("book row");
    assert_eq!(record.cover_path.as_deref(), Some(cover_path.as_str()));
}

#[tokio::test]
async fn import_book_persists_canonical_blocks_and_the_generation_stamp() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let body = sse_body(&[
        ("page_blocks", &page_blocks_json()),
        ("manifest", &manifest_json()),
    ]);
    let port = spawn_sse_server(body);
    let books_dir = dir.path().join("books");
    let client = reqwest::Client::new();

    let result = library::import_book(
        &pool,
        &client,
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &FallbackSource::off(),
        |_| {},
    )
    .await
    .expect("import");

    let stored = book::get_book(&pool, &result.book.id)
        .await
        .expect("get")
        .expect("book");
    assert_eq!(stored.blocks_page_count, Some(4));
    assert!(stored
        .blocks_version
        .as_deref()
        .expect("version")
        .contains("reflow="));

    for page_no in 1..=4 {
        let page = book::get_page(&pool, &result.book.id, page_no)
            .await
            .expect("get page")
            .expect("page row");
        let text = page.text_json.expect("blocks must reach the database");
        assert!(text.contains(&format!("page {page_no}")));
        assert!(!text.contains("\"role\""), "roles are never stored");
        assert_eq!(page.width_pt, Some(612.0));
        assert_eq!(page.needs_upgrade, page_no == 2);
    }
}

type CapturedBodies = Arc<Mutex<Vec<String>>>;

fn spawn_capturing_sse_server(body: String) -> (u16, CapturedBodies) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let captured: CapturedBodies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                let mut header_end = None;
                let mut content_length = 0usize;
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let head = String::from_utf8_lossy(&req[..pos]).to_string();
                            content_length = head
                                .lines()
                                .filter_map(|line| line.split_once(':'))
                                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .and_then(|(_, value)| value.trim().parse().ok())
                                .unwrap_or(0);
                        }
                    }
                    if header_end.is_some_and(|end| req.len() >= end + content_length) {
                        break;
                    }
                }
                let end = header_end.unwrap_or(req.len());
                sink.lock()
                    .expect("lock")
                    .push(String::from_utf8_lossy(&req[end..]).to_string());
                let _ = mock_http::write_response(
                    &mut sock,
                    "200 OK",
                    "text/event-stream",
                    body.as_bytes(),
                )
                .await;
            });
        }
    });
    (port, captured)
}

const PIN_KEY: &str = "AIzaSy-import-pin-material-0123456789abcdef";

async fn keyed_source(pool: &SqlitePool, enabled: bool) -> FallbackSource {
    let keys = Arc::new(KeyManager::session_only_for_tests());
    keys.add_key(pool, "gemini", None, PIN_KEY.to_string(), None)
        .await
        .expect("add key");
    if enabled {
        storage::settings::set_setting(pool, FALLBACK_ENABLED_KEY, "true")
            .await
            .expect("enable");
    }
    FallbackSource::from_manager(keys)
}

fn ingest_body(bodies: &CapturedBodies) -> serde_json::Value {
    let bodies = bodies.lock().expect("lock").clone();
    let raw = bodies
        .iter()
        .find(|body| body.contains("\"book_path\""))
        .unwrap_or_else(|| panic!("no /ingest body captured: {bodies:?}"));
    serde_json::from_str(raw).expect("ingest body is json")
}

fn assert_tree_free_of(dir: &std::path::Path, needle: &str) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            assert_tree_free_of(&path, needle);
        } else {
            let bytes = std::fs::read(&path).expect("read file");
            assert!(
                !String::from_utf8_lossy(&bytes).contains(needle),
                "{} carries key material",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn enabled_stored_key_reaches_the_wire_and_no_persisted_surface_carries_it() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let (port, bodies) = spawn_capturing_sse_server(sse_body(&[("manifest", &manifest_json())]));
    let books_dir = dir.path().join("books");
    let fallback = keyed_source(&pool, true).await;
    let mut events = Vec::new();
    let result = library::import_book(
        &pool,
        &reqwest::Client::new(),
        port,
        "tok",
        &books_dir,
        &source,
        None,
        true,
        &fallback,
        |event| events.push(event),
    )
    .await
    .expect("import");

    let ingest = ingest_body(&bodies);
    assert_eq!(ingest["fallback"]["google_key"], PIN_KEY, "{ingest}");
    assert!(ingest["fallback"].get("openai_key").is_none(), "{ingest}");

    let row = book::get_book(&pool, &result.book.id)
        .await
        .expect("book query")
        .expect("book row");
    assert!(
        !format!("{row:?}").contains(PIN_KEY),
        "book row carries key material"
    );
    let summary = serde_json::to_string(&result).expect("result json");
    assert!(!summary.contains(PIN_KEY), "{summary}");
    let emitted = serde_json::to_string(&events).expect("events json");
    assert!(!emitted.contains(PIN_KEY), "{emitted}");
    assert_tree_free_of(&books_dir, PIN_KEY);
}

#[tokio::test]
async fn toggle_off_keeps_a_stored_key_off_the_wire() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let (port, bodies) = spawn_capturing_sse_server(sse_body(&[("manifest", &manifest_json())]));
    let fallback = keyed_source(&pool, false).await;
    library::import_book(
        &pool,
        &reqwest::Client::new(),
        port,
        "tok",
        &dir.path().join("books"),
        &source,
        None,
        true,
        &fallback,
        |_| {},
    )
    .await
    .expect("import");

    let ingest = ingest_body(&bodies);
    assert!(ingest.get("fallback").is_none(), "{ingest}");
    assert!(!ingest.to_string().contains(PIN_KEY), "{ingest}");
}

#[tokio::test]
async fn no_stored_key_means_no_fallback_block_even_when_enabled() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let (port, bodies) = spawn_capturing_sse_server(sse_body(&[("manifest", &manifest_json())]));
    storage::settings::set_setting(&pool, FALLBACK_ENABLED_KEY, "true")
        .await
        .expect("enable");
    let fallback = FallbackSource::from_manager(Arc::new(KeyManager::session_only_for_tests()));
    library::import_book(
        &pool,
        &reqwest::Client::new(),
        port,
        "tok",
        &dir.path().join("books"),
        &source,
        None,
        true,
        &fallback,
        |_| {},
    )
    .await
    .expect("import");

    let ingest = ingest_body(&bodies);
    assert!(ingest.get("fallback").is_none(), "{ingest}");
}

#[tokio::test]
async fn a_failed_import_error_carries_no_key_material() {
    let (dir, pool) = test_pool().await;
    let source = write_fixture_pdf(dir.path());
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = closed.local_addr().expect("addr").port();
    drop(closed);
    let fallback = keyed_source(&pool, true).await;
    let error = library::import_book(
        &pool,
        &reqwest::Client::new(),
        port,
        "tok",
        &dir.path().join("books"),
        &source,
        None,
        true,
        &fallback,
        |_| {},
    )
    .await
    .expect_err("nothing listens on the port");

    assert!(!error.contains(PIN_KEY), "{error}");
    assert!(!error.is_empty());
}
