#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const READ_BUDGET: Duration = Duration::from_secs(5);

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (field, value) = line.split_once(':')?;
        if field.trim().eq_ignore_ascii_case(name) {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn header_values(head: &str, name: &str) -> Vec<String> {
    head.lines()
        .skip(1)
        .filter_map(|line| {
            let (field, value) = line.split_once(':')?;
            field
                .trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
        .collect()
}

fn split_head(raw: &str) -> (String, String) {
    match raw.split_once("\r\n\r\n") {
        Some((head, body)) => (head.to_string(), body.to_string()),
        None => (raw.to_string(), String::new()),
    }
}

async fn one_exchange(request: &[u8], serve: fn(TcpStream) -> ServeFuture) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf).await;
        serve(sock).await;
    });

    let mut client = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    client.write_all(request).await.expect("write request");
    let mut raw = Vec::new();
    tokio::time::timeout(READ_BUDGET, client.read_to_end(&mut raw))
        .await
        .expect("the writer must close the socket, so the client must reach EOF")
        .expect("read to EOF");
    String::from_utf8_lossy(&raw).to_string()
}

type ServeFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

#[tokio::test]
async fn a_written_response_tells_the_client_the_connection_will_not_be_reused() {
    let raw = one_exchange(
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
        |mut sock| {
            Box::pin(async move {
                mock_http::write_response(
                    &mut sock,
                    "200 OK",
                    "application/json",
                    b"{\"ok\":true}",
                )
                .await
                .expect("write response");
            })
        },
    )
    .await;

    let (head, body) = split_head(&raw);
    assert_eq!(
        header_value(&head, "connection").as_deref(),
        Some("close"),
        "a mock that frames with Content-Length and omits Connection: close promises keep-alive \
         reuse it does not honour, and a pooled client that writes a second request into the \
         closed socket gets a transport error hyper cannot retry: {head}"
    );
    assert_eq!(
        header_value(&head, "content-length").as_deref(),
        Some("11"),
        "the close header must not be bought by dropping the framing the readers parse: {head}"
    );
    assert_eq!(body, "{\"ok\":true}");
}

#[tokio::test]
async fn a_streaming_head_written_without_a_length_carries_the_same_promise() {
    let raw = one_exchange(
        b"POST /script HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        |mut sock| {
            Box::pin(async move {
                mock_http::write_head(
                    &mut sock,
                    "200 OK",
                    &[("Content-Type", "text/event-stream")],
                    None,
                )
                .await
                .expect("write head");
                sock.write_all(b"event: done\ndata: {}\n\n")
                    .await
                    .expect("write event");
            })
        },
    )
    .await;

    let (head, body) = split_head(&raw);
    assert_eq!(
        header_value(&head, "connection").as_deref(),
        Some("close"),
        "the streaming entry point is the same writer and must not be a second, honest-looking \
         way to tell the same lie: {head}"
    );
    assert_eq!(header_value(&head, "content-length"), None);
    assert_eq!(body, "event: done\ndata: {}\n\n");
}

#[tokio::test]
async fn a_caller_cannot_talk_the_writer_out_of_closing() {
    let raw = one_exchange(
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
        |mut sock| {
            Box::pin(async move {
                mock_http::write_head(
                    &mut sock,
                    "200 OK",
                    &[
                        ("Connection", "keep-alive"),
                        ("Content-Type", "application/json"),
                    ],
                    Some(2),
                )
                .await
                .expect("write head");
                sock.write_all(b"{}").await.expect("write body");
            })
        },
    )
    .await;

    let (head, body) = split_head(&raw);
    assert_eq!(
        header_values(&head, "connection"),
        vec!["close".to_string()],
        "the writer owns the connection semantics: a mock that asks for keep-alive must not get \
         it, and must not get two Connection headers whose meaning depends on which one a client \
         reads first: {head}"
    );
    assert_eq!(
        header_values(&head, "content-type"),
        vec!["application/json".to_string()],
        "the filter is for the one header the writer owns, not a licence to drop the rest: {head}"
    );
    assert_eq!(body, "{}");
}

const WRITER: &str = "src/test_support/mock_http.rs";
const SCAN: &str = "tests/mock_http_contract.rs";

fn status_line_needle() -> String {
    format!("HTTP/{}.{} ", 1, 1)
}

fn writer_call_needles() -> [String; 2] {
    [
        format!("write_response{}", '('),
        format!("write_head{}", '('),
    ]
}

const KEEP_ALIVE_HONEST_MOCKS: &[(&str, &str)] = &[];

// 42/24 counts the VA5 composed-rig chat mock (src/voice/channel/composed.rs),
// whose scripted SSE server serves its head through the shared writer, the
// S-1/S-2/S-3 adapter loopback fixtures (providers/adapters/{gemini,
// openai_compat,anthropic,cartesia_tts,openai_tts}.rs), the S-1d wired-path
// cloud/seam mocks in tests/scripting_stage.rs, and the S-5 blocker-5
// auth-gated rotation mock (401 + 200 heads) in the same file. 48/26 = the 44/25
// this scan measures on main at 793efbe0 + the UPD-1 updater fixture origin
// (src/updater/tests.rs: 200 body, 404, 302 head, and the slow-stream head for
// the cancel pin — 4 calls, all through the shared writer). 51/28 adds the
// engine-install fetch fixture (src/engine_install/fetch.rs).
const EXPECTED_WRITER_CALLS: usize = 51;
const EXPECTED_WRITER_CALL_FILES: usize = 28;
const SCAN_ANCHORS: &[&str] = &[
    "src/page_render.rs",
    "src/orchestrator/stream.rs",
    "src/voice/keys/probe.rs",
    "src/voice/channel/fixture.rs",
    "tests/page_render.rs",
    "tests/scripting_stage.rs",
];

struct Scan {
    files: Vec<String>,
    status_lines: Vec<(String, usize)>,
    writer_calls: Vec<(String, usize)>,
}

fn crate_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect(dir: &std::path::Path, root: &std::path::Path, into: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|err| panic!("read {dir:?}: {err}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(&path, root, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).expect("under the crate root");
            into.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn scan() -> Scan {
    let root = crate_root();
    let mut files = Vec::new();
    for tree in ["src", "tests"] {
        collect(&root.join(tree), &root, &mut files);
    }
    files.sort();

    let status_line = status_line_needle();
    let needles = writer_call_needles();
    let mut status_lines = Vec::new();
    let mut writer_calls = Vec::new();
    for name in &files {
        let body = std::fs::read_to_string(root.join(name)).expect("read source");
        let sites = body.matches(status_line.as_str()).count();
        if sites > 0 {
            status_lines.push((name.clone(), sites));
        }
        if name == WRITER {
            continue;
        }
        let calls: usize = needles
            .iter()
            .map(|call| body.matches(call.as_str()).count())
            .sum();
        if calls > 0 {
            writer_calls.push((name.clone(), calls));
        }
    }
    Scan {
        files,
        status_lines,
        writer_calls,
    }
}

#[test]
fn the_scan_can_see_the_source_it_claims_to_pin() {
    let found = scan();
    for anchor in SCAN_ANCHORS {
        assert!(
            found.files.contains(&anchor.to_string()),
            "the walk missed {anchor}; a scan that silently reads no files passes every \
             assertion below it and pins nothing. Files seen: {}",
            found.files.len()
        );
    }
    assert_eq!(
        found
            .status_lines
            .iter()
            .find(|(name, _)| name == WRITER)
            .map(|(_, sites)| *sites),
        Some(1),
        "the shared writer's own status line is the one literal this scan must find; not \
         finding it means the scan reads names and not contents"
    );
    assert!(
        !found.status_lines.iter().any(|(name, _)| name == SCAN),
        "this scan assembles the literal it forbids at runtime; spelling it out verbatim here \
         would make the guard its own only offender and hide every real one behind it"
    );
}

#[test]
fn no_mock_writes_its_own_status_line_outside_the_shared_writer() {
    let found = scan();
    let exempt: Vec<&str> = KEEP_ALIVE_HONEST_MOCKS
        .iter()
        .map(|(path, _)| *path)
        .collect();
    let offenders: Vec<&(String, usize)> = found
        .status_lines
        .iter()
        .filter(|(name, _)| name != WRITER && !exempt.contains(&name.as_str()))
        .collect();

    assert!(
        offenders.is_empty(),
        "a hand-written HTTP/1.1-shaped status line is how the keep-alive lie returns: a mock that \
         frames with Content-Length, omits Connection: close and then closes the socket \
         promises a reuse it will not honour, and a pooled reqwest client that dispatches a \
         second request into that socket gets a transport error hyper cannot retry. Route it \
         through {WRITER} or add it to KEEP_ALIVE_HONEST_MOCKS with a reason. Offenders: \
         {offenders:?}"
    );
}

#[test]
fn every_mock_that_was_migrated_still_reaches_the_shared_writer() {
    let found = scan();
    let calls: usize = found.writer_calls.iter().map(|(_, count)| count).sum();

    assert_eq!(
        (calls, found.writer_calls.len()),
        (EXPECTED_WRITER_CALLS, EXPECTED_WRITER_CALL_FILES),
        "the status-line scan can only see a mock that writes its own head; a mock deleted, \
         or moved to a socket write the scan cannot recognise, shows up here as a drop in the \
         pinned count. Update both numbers deliberately in the same diff that changes the \
         mocks. Seen: {:?}",
        found.writer_calls
    );
}

#[test]
fn an_exemption_names_a_real_file_and_says_why() {
    let root = crate_root();
    for (path, reason) in KEEP_ALIVE_HONEST_MOCKS {
        assert!(
            root.join(path).is_file(),
            "exemption {path} names no file; a rotted exemption silently widens the scan"
        );
        assert!(
            reason.len() > 20,
            "exemption {path} must record why its mock honours keep-alive honestly, not just \
             that it is excused"
        );
    }
}
