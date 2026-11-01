#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use txtcaster_lib::page_render::{self, PageRender, RenderEvent, RenderPagesRequest};
use txtcaster_lib::storage::{self, book, cache, new_id, now_ms, SqlitePool};

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

fn sample_book(id: &str) -> book::Book {
    book::Book {
        id: id.into(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(3),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: "c".repeat(64),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    }
}

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn spawn_sse_server(body: String) -> u16 {
    let chunks = body.as_bytes().chunks(16).map(<[u8]>::to_vec).collect();
    spawn_sse_server_chunks(chunks)
}

fn spawn_sse_server_chunks(chunks: Vec<Vec<u8>>) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let chunks = chunks.clone();
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
                let total: usize = chunks.iter().map(Vec::len).sum();
                let _ = mock_http::write_head(
                    &mut sock,
                    "200 OK",
                    &[("Content-Type", "text/event-stream")],
                    Some(total),
                )
                .await;
                for chunk in &chunks {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    let _ = sock.write_all(chunk).await;
                    let _ = sock.flush().await;
                }
            });
        }
    });
    port
}

fn request() -> RenderPagesRequest {
    RenderPagesRequest {
        book_path: "/books/fixture.pdf".into(),
        content_hash: "c".repeat(64),
        pages: vec![1, 2],
        cache_dir: "/tmp/cache".into(),
        scale: None,
        with_geometry: false,
    }
}

async fn insert_pages(pool: &SqlitePool, book_id: &str, count: i64) -> Vec<String> {
    let mut ids = Vec::new();
    for page_no in 1..=count {
        let page = book::Page {
            id: new_id(),
            book_id: book_id.into(),
            page_no,
            width_pt: Some(612.0),
            height_pt: Some(792.0),
            render_hash: None,
            text_json: None,
            needs_upgrade: false,
        };
        book::insert_page(pool, &page).await.expect("insert page");
        ids.push(page.id);
    }
    ids
}

#[tokio::test]
async fn render_pages_persists_cache_entries_and_render_hashes() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-render");
    book::insert_book(&pool, &fixture).await.expect("book");
    insert_pages(&pool, &fixture.id, 2).await;

    let h1 = "1".repeat(64);
    let h2 = "2".repeat(64);
    let body = sse_body(&[
        ("progress", r#"{"stage": "render", "page": 1, "total": 2}"#),
        ("progress", r#"{"stage": "render", "page": 2, "total": 2}"#),
        (
            "result",
            &format!(
                r#"{{"pages": [{{"page_no": 1, "hash": "{h1}", "bytes": 100, "cached": false}}, {{"page_no": 2, "hash": "{h2}", "bytes": 200, "cached": true}}]}}"#
            ),
        ),
    ]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let mut events: Vec<RenderEvent> = Vec::new();
    let pages = page_render::render_pages(
        &pool,
        &client,
        port,
        "tok",
        &fixture.id,
        &request(),
        |event| events.push(event),
    )
    .await
    .expect("render");

    assert_eq!(
        pages,
        vec![
            PageRender {
                page_no: 1,
                hash: h1.clone(),
                bytes: 100,
                cached: false,
                geometry_version: None,
                geometry: None,
            },
            PageRender {
                page_no: 2,
                hash: h2.clone(),
                bytes: 200,
                cached: true,
                geometry_version: None,
                geometry: None,
            },
        ]
    );
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], RenderEvent::Progress { page: 1, total: 2 });
    assert!(matches!(events[2], RenderEvent::Done { .. }));

    let entry = cache::get_entry(&pool, &h1)
        .await
        .expect("get")
        .expect("entry");
    assert_eq!(entry.kind, "page_png");
    assert_eq!(entry.bytes, 100);
    let page = book::get_page(&pool, &fixture.id, 2)
        .await
        .expect("get page")
        .expect("page row");
    assert_eq!(page.render_hash, Some(h2));
}

#[tokio::test]
async fn render_pages_surfaces_engine_error() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-error");
    book::insert_book(&pool, &fixture).await.expect("book");

    let body = sse_body(&[(
        "error",
        r#"{"code": "ERR_BAD_REQUEST", "message": "pages out of range"}"#,
    )]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let result =
        page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request(), |_| {})
            .await;
    let error = result.expect_err("should fail");
    assert!(error.contains("ERR_BAD_REQUEST"));
    assert_eq!(cache::total_bytes(&pool).await.expect("total"), 0);
}

#[tokio::test]
async fn render_pages_surfaces_cancellation() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-cancel");
    book::insert_book(&pool, &fixture).await.expect("book");

    let body = sse_body(&[("cancelled", r#"{"job_id": "book-cancel"}"#)]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let result =
        page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request(), |_| {})
            .await;
    assert!(result.expect_err("should fail").contains("cancelled"));
}

#[tokio::test]
async fn render_pages_decodes_multibyte_utf8_split_across_chunks() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-utf8");
    book::insert_book(&pool, &fixture).await.expect("book");

    let body = sse_body(&[(
        "error",
        r#"{"code": "ERR_OPEN", "message": "cannot open Émile Zola — Œuvres"}"#,
    )]);
    let bytes = body.into_bytes();
    let split = bytes
        .iter()
        .position(|b| *b == 0xC3)
        .expect("multi-byte char in body")
        + 1;
    let chunks = vec![bytes[..split].to_vec(), bytes[split..].to_vec()];
    assert!(std::str::from_utf8(&chunks[0]).is_err());
    let port = spawn_sse_server_chunks(chunks);
    let client = reqwest::Client::new();

    let result =
        page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request(), |_| {})
            .await;
    let error = result.expect_err("should fail");
    assert!(error.contains("Émile Zola — Œuvres"), "got: {error}");
}

#[tokio::test]
async fn render_pages_surfaces_result_parse_error() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-badresult");
    book::insert_book(&pool, &fixture).await.expect("book");

    let body = sse_body(&[("result", r#"{"pages": [{"page_no": "not-a-number"}]}"#)]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let result =
        page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request(), |_| {})
            .await;
    let error = result.expect_err("should fail");
    assert!(error.contains("ERR_DESERIALIZE"), "got: {error}");
}

#[tokio::test]
async fn render_pages_fails_on_truncated_stream() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-trunc");
    book::insert_book(&pool, &fixture).await.expect("book");

    let body = sse_body(&[("progress", r#"{"stage": "render", "page": 1, "total": 2}"#)]);
    let port = spawn_sse_server(body);
    let client = reqwest::Client::new();

    let result =
        page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request(), |_| {})
            .await;
    assert!(result
        .expect_err("should fail")
        .contains("without a result"));
}

#[test]
fn the_render_request_lets_the_engine_own_the_raster_scale() {
    let body = serde_json::to_value(request()).expect("serialize");
    assert!(
        body.get("scale").is_none(),
        "the engine's DEFAULT_SCALE both rasterises the page and stamps geometry_version, so it \
         is the single authority. Sending a scale from Rust reintroduces the divergence where the \
         PNG is rendered at one scale and the freshness stamp describes another, and the stamp \
         then reports fresh for a raster it does not describe. Body was: {body}"
    );
}
