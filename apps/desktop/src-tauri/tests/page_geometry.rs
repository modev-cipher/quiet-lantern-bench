#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use txtcaster_lib::page_render::{self, RenderPagesRequest};
use txtcaster_lib::reading::{self, GeometryFreshness, GeometrySlot, ReadingPage};
use txtcaster_lib::storage::{self, book, new_id, now_ms, SqlitePool};

const VERSION: &str = "v1|geom=1|pymupdf=1.28.0|reflow=612x792@11";
const OTHER_VERSION: &str = "v1|geom=1|pymupdf=1.29.0|reflow=612x792@11";

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

fn envelope(words: usize, lines: usize) -> String {
    let word_rows: Vec<String> = (0..words)
        .map(|i| {
            format!(
                "[10.0,{i}.0,40.0,{}.0,\"tok{i}\",{}]",
                i + 8,
                i % lines.max(1)
            )
        })
        .collect();
    let line_rows: Vec<String> = (0..lines)
        .map(|i| format!("[10.0,{i}.0,400.0,{}.0]", i + 8))
        .collect();
    format!(
        r#"{{"v":1,"pageWidthPt":612.0,"pageHeightPt":792.0,"lines":[{}],"words":[{}]}}"#,
        line_rows.join(","),
        word_rows.join(",")
    )
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

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

async fn spawn_sse_server(body: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let payload = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
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
                    payload.as_bytes(),
                )
                .await;
            });
        }
    });
    port
}

async fn spawn_slow_sse_server(body: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let payload = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
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
                let _ = mock_http::write_head(
                    &mut sock,
                    "200 OK",
                    &[("Content-Type", "text/event-stream")],
                    Some(payload.len()),
                )
                .await;
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let _ = sock.write_all(payload.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    port
}

fn page(page_no: i64, rendered: bool) -> ReadingPage {
    ReadingPage {
        page_no,
        width_pt: Some(612.0),
        height_pt: Some(792.0),
        image_path: rendered.then(|| format!("/cache/ab/{page_no}")),
    }
}

fn slot(page_no: i64, version: Option<&str>) -> GeometrySlot {
    GeometrySlot {
        page_no,
        page_id: format!("page-{page_no}"),
        version: version.map(str::to_string),
        words: if version.is_some() { 12 } else { 0 },
        lines: if version.is_some() { 3 } else { 0 },
        bytes: if version.is_some() { 512 } else { 0 },
    }
}

#[test]
fn a_stored_version_matching_the_engine_is_fresh() {
    assert_eq!(
        reading::classify_geometry(Some(VERSION), Some(VERSION)),
        GeometryFreshness::Fresh
    );
}

#[test]
fn a_stored_version_from_another_build_is_stale() {
    assert_eq!(
        reading::classify_geometry(Some(OTHER_VERSION), Some(VERSION)),
        GeometryFreshness::Stale
    );
}

#[test]
fn a_page_with_no_geometry_row_is_absent() {
    assert_eq!(
        reading::classify_geometry(None, Some(VERSION)),
        GeometryFreshness::Absent
    );
}

#[test]
fn geometry_is_unverified_when_the_engine_declares_no_version() {
    assert_eq!(
        reading::classify_geometry(Some(VERSION), None),
        GeometryFreshness::Unverified
    );
    assert!(
        !reading::classify_geometry(Some(VERSION), None).needs_fetch(),
        "with the engine down there is nothing to compare against and nothing to fetch from; \
         claiming staleness there would make the stale grep fire on every offline read"
    );
}

#[test]
fn a_rendered_page_whose_geometry_is_missing_is_still_ensured() {
    let pages = vec![page(4, true), page(5, true)];
    let geometry = vec![slot(4, Some(VERSION)), slot(5, None)];
    assert_eq!(
        reading::pages_to_ensure(&pages, &geometry, Some(VERSION)),
        vec![5],
        "a warm PNG with no geometry row must still reach the engine, or geometry is never \
         fetched for any page the feeder already rendered"
    );
}

#[test]
fn a_rendered_page_whose_geometry_is_stale_is_re_ensured() {
    let pages = vec![page(4, true)];
    let geometry = vec![slot(4, Some(OTHER_VERSION))];
    assert_eq!(
        reading::pages_to_ensure(&pages, &geometry, Some(VERSION)),
        vec![4]
    );
}

#[test]
fn a_fully_warm_page_asks_the_engine_for_nothing() {
    let pages = vec![page(4, true), page(5, true)];
    let geometry = vec![slot(4, Some(VERSION)), slot(5, Some(VERSION))];
    assert!(reading::pages_to_ensure(&pages, &geometry, Some(VERSION)).is_empty());
}

#[test]
fn a_missing_image_is_ensured_even_when_geometry_is_fresh() {
    let pages = vec![page(4, false)];
    let geometry = vec![slot(4, Some(VERSION))];
    assert_eq!(
        reading::pages_to_ensure(&pages, &geometry, Some(VERSION)),
        vec![4]
    );
}

#[test]
fn a_page_needing_both_is_requested_once() {
    let pages = vec![page(4, false)];
    let geometry = vec![slot(4, None)];
    assert_eq!(
        reading::pages_to_ensure(&pages, &geometry, Some(VERSION)),
        vec![4]
    );
}

#[test]
fn the_geometry_line_carries_every_field_the_log_review_greps() {
    let line = reading::format_geometry_line(
        "0123456789abcdef",
        25,
        314,
        29,
        10_412,
        Some(VERSION),
        GeometryFreshness::Fresh,
    );
    assert_eq!(
        line,
        format!(
            "page geometry: book=01234567 page=25 words=314 lines=29 bytes=10412 \
             version={VERSION} stale=false"
        )
    );
}

#[test]
fn every_freshness_value_has_a_pinned_literal() {
    let emitted: Vec<&str> = [
        GeometryFreshness::Fresh,
        GeometryFreshness::Stale,
        GeometryFreshness::Absent,
        GeometryFreshness::Unverified,
    ]
    .iter()
    .map(|state| state.as_str())
    .collect();
    assert_eq!(emitted, vec!["false", "true", "true", "unverified"]);
    let line = reading::format_geometry_line(
        "book0001",
        7,
        12,
        3,
        512,
        Some(VERSION),
        GeometryFreshness::Unverified,
    );
    assert!(
        line.ends_with("stale=unverified"),
        "the emitted literal is the contract for a value whose entire justification is that a \
         reviewer can grep it; docs, PR body and log must all say one word: {line}"
    );
}

#[test]
fn the_ensure_plan_line_is_written_in_plan_tense() {
    let line = reading::format_ensure_plan("0123456789abcdef", 3, 1);
    assert_eq!(
        line,
        "page ensure: book=01234567 pages=3 toRasterise=1 geometryOnly=2"
    );
    assert!(
        !line.contains("rasterised="),
        "this line is emitted before the render runs, so past tense would narrate work that \
         has not happened and may still fail: {line}"
    );
}

#[test]
fn an_absent_row_logs_stale_and_a_missing_version() {
    let line =
        reading::format_geometry_line("book0001", 7, 0, 0, 0, None, GeometryFreshness::Absent);
    assert!(line.contains("stale=true"), "{line}");
    assert!(line.contains("version=none"), "{line}");
    assert!(line.contains("words=0"), "{line}");
}

#[test]
fn counting_a_payload_reports_its_words_and_lines() {
    assert_eq!(reading::count_geometry(&envelope(12, 3)), (12, 3));
    assert_eq!(reading::count_geometry("not json"), (0, 0));
    assert_eq!(reading::count_geometry("{}"), (0, 0));
}

#[tokio::test]
async fn geometry_slots_report_the_stored_row() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-geom");
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 2).await;
    book::upsert_page_geometry(
        &pool,
        &book::PageGeometry {
            page_id: ids[0].clone(),
            geometry_version: VERSION.into(),
            geometry_json: envelope(9, 3),
        },
    )
    .await
    .expect("upsert");

    let slots = reading::geometry_slots(&pool, &fixture.id, &[1, 2])
        .await
        .expect("slots");
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].version.as_deref(), Some(VERSION));
    assert_eq!(slots[0].words, 9);
    assert_eq!(slots[0].lines, 3);
    assert!(slots[0].bytes > 0);
    assert_eq!(slots[1].version, None);
    assert_eq!(slots[1].words, 0);
}

#[tokio::test]
async fn an_upsert_replaces_the_previous_generation_rather_than_accumulating() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-upsert");
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 1).await;
    for (version, words) in [(OTHER_VERSION, 4), (VERSION, 9)] {
        book::upsert_page_geometry(
            &pool,
            &book::PageGeometry {
                page_id: ids[0].clone(),
                geometry_version: version.into(),
                geometry_json: envelope(words, 2),
            },
        )
        .await
        .expect("upsert");
    }
    let stored = book::get_page_geometry(&pool, &ids[0])
        .await
        .expect("get")
        .expect("row");
    assert_eq!(stored.geometry_version, VERSION);
    assert_eq!(reading::count_geometry(&stored.geometry_json).0, 9);
}

#[tokio::test]
async fn deleting_a_page_takes_its_geometry_with_it() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-cascade");
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 1).await;
    book::upsert_page_geometry(
        &pool,
        &book::PageGeometry {
            page_id: ids[0].clone(),
            geometry_version: VERSION.into(),
            geometry_json: envelope(3, 1),
        },
    )
    .await
    .expect("upsert");
    sqlx::query("DELETE FROM page WHERE id = ?")
        .bind(&ids[0])
        .execute(&pool)
        .await
        .expect("delete page");
    assert!(
        book::get_page_geometry(&pool, &ids[0])
            .await
            .expect("get")
            .is_none(),
        "geometry must ride the page's own lifetime; a reflow that shrinks the page count \
         would otherwise leave rects for pages that no longer exist"
    );
}

#[tokio::test]
async fn a_render_response_carrying_geometry_persists_it_with_the_declared_version() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-persist");
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 1).await;
    let hash = "a".repeat(64);
    let body = sse_body(&[(
        "result",
        &format!(
            r#"{{"pages": [{{"page_no": 1, "hash": "{hash}", "bytes": 100, "cached": false, "geometry_version": "{VERSION}", "geometry": {}}}]}}"#,
            envelope(6, 2)
        ),
    )]);
    let port = spawn_sse_server(body).await;
    let client = reqwest::Client::new();
    let request = RenderPagesRequest {
        book_path: fixture.source_path.clone(),
        content_hash: fixture.content_hash.clone(),
        pages: vec![1],
        cache_dir: "/tmp/cache".into(),
        scale: None,
        with_geometry: true,
    };
    page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request, |_| {})
        .await
        .expect("render");

    let stored = book::get_page_geometry(&pool, &ids[0])
        .await
        .expect("get")
        .expect("geometry row");
    assert_eq!(stored.geometry_version, VERSION);
    assert_eq!(reading::count_geometry(&stored.geometry_json), (6, 2));
}

#[tokio::test]
async fn a_render_response_without_geometry_writes_no_row() {
    let (_dir, pool) = test_pool().await;
    let fixture = sample_book("book-nogeom");
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 1).await;
    let hash = "b".repeat(64);
    let body = sse_body(&[(
        "result",
        &format!(
            r#"{{"pages": [{{"page_no": 1, "hash": "{hash}", "bytes": 100, "cached": false}}]}}"#
        ),
    )]);
    let port = spawn_sse_server(body).await;
    let client = reqwest::Client::new();
    let request = RenderPagesRequest {
        book_path: fixture.source_path.clone(),
        content_hash: fixture.content_hash.clone(),
        pages: vec![1],
        cache_dir: "/tmp/cache".into(),
        scale: None,
        with_geometry: false,
    };
    page_render::render_pages(&pool, &client, port, "tok", &fixture.id, &request, |_| {})
        .await
        .expect("render");
    assert!(book::get_page_geometry(&pool, &ids[0])
        .await
        .expect("get")
        .is_none());
}

#[tokio::test]
async fn geometry_that_lands_after_a_reingest_is_discarded_rather_than_resurrected() {
    let (_dir, pool) = test_pool().await;
    let mut fixture = sample_book("book-fence");
    fixture.blocks_version = Some("gen-1".into());
    fixture.blocks_page_count = Some(1);
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 1).await;

    let hash = "e".repeat(64);
    let body = sse_body(&[(
        "result",
        &format!(
            r#"{{"pages": [{{"page_no": 1, "hash": "{hash}", "bytes": 100, "cached": false, "geometry_version": "{VERSION}", "geometry": {}}}]}}"#,
            envelope(6, 2)
        ),
    )]);
    let port = spawn_slow_sse_server(body).await;
    let client = reqwest::Client::new();
    let request = RenderPagesRequest {
        book_path: fixture.source_path.clone(),
        content_hash: fixture.content_hash.clone(),
        pages: vec![1],
        cache_dir: "/tmp/cache".into(),
        scale: None,
        with_geometry: true,
    };
    let fence = page_render::GenerationFence {
        blocks_version: Some("gen-1".into()),
    };

    let render = page_render::render_pages_fenced(
        &pool,
        &client,
        port,
        "tok",
        page_render::PersistTarget::fenced(&fixture.id, &fence),
        &request,
        |_| {},
    );

    let reingest = async {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let replacement = book::Page {
            id: new_id(),
            book_id: fixture.id.clone(),
            page_no: 1,
            width_pt: Some(612.0),
            height_pt: Some(792.0),
            render_hash: None,
            text_json: Some(r#"{"quality":{},"blocks":[9]}"#.into()),
            needs_upgrade: false,
        };
        book::replace_blocks(
            &pool,
            &fixture.id,
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
        .expect("reingest");
    };

    let (rendered, ()) = tokio::join!(render, reingest);
    rendered.expect("render");

    assert!(
        book::get_page_geometry(&pool, &ids[0])
            .await
            .expect("geometry")
            .is_none(),
        "the re-ingest deleted this page's geometry while the fetch was in flight; a write-back \
         with no generation fence resurrects a row the DELETE just removed, and because the \
         extractor identity never moved it then reads Fresh forever for content that no longer \
         exists — the same defect the DELETE closes, reached by timing instead of by storage"
    );
    let page_1 = book::get_page(&pool, &fixture.id, 1)
        .await
        .expect("page")
        .expect("page 1 exists");
    assert!(
        page_1.render_hash.is_none(),
        "render hash must be None when generation fence moved"
    );
}

#[tokio::test]
async fn geometry_still_lands_when_the_generation_did_not_move() {
    let (_dir, pool) = test_pool().await;
    let mut fixture = sample_book("book-fence-ok");
    fixture.blocks_version = Some("gen-1".into());
    book::insert_book(&pool, &fixture).await.expect("book");
    let ids = insert_pages(&pool, &fixture.id, 1).await;
    let hash = "f".repeat(64);
    let body = sse_body(&[(
        "result",
        &format!(
            r#"{{"pages": [{{"page_no": 1, "hash": "{hash}", "bytes": 100, "cached": false, "geometry_version": "{VERSION}", "geometry": {}}}]}}"#,
            envelope(6, 2)
        ),
    )]);
    let port = spawn_sse_server(body).await;
    let client = reqwest::Client::new();
    let request = RenderPagesRequest {
        book_path: fixture.source_path.clone(),
        content_hash: fixture.content_hash.clone(),
        pages: vec![1],
        cache_dir: "/tmp/cache".into(),
        scale: None,
        with_geometry: true,
    };
    let fence = page_render::GenerationFence {
        blocks_version: Some("gen-1".into()),
    };
    page_render::render_pages_fenced(
        &pool,
        &client,
        port,
        "tok",
        page_render::PersistTarget::fenced(&fixture.id, &fence),
        &request,
        |_| {},
    )
    .await
    .expect("render");
    assert!(
        book::get_page_geometry(&pool, &ids[0])
            .await
            .expect("geometry")
            .is_some(),
        "positive control: the fence must pass the ordinary case, or the test above passes \
         because nothing is ever written"
    );
}
