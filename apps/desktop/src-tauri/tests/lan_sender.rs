#[path = "../src/test_support/lan_pin_client.rs"]
mod lan_pin_client;

use std::sync::Arc;
use std::time::Duration;

use lan_pin_client::{
    chunked_request, open_ended_chunked_request, request, request_with_body, status_of,
    try_connect, RawResponse,
};
use sha2::{Digest, Sha256};
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use txtcaster_lib::lan_sender::{proto, server, session};

const CHUNK: u64 = proto::MIN_CHUNK_SIZE as u64;

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

/// Boots a live TLS listener over a single-cast session, with the production
/// pre-auth deadlines.
async fn serving(
    bytes: &[u8],
) -> (
    tempfile::TempDir,
    Arc<session::SessionState>,
    server::ServerHandle,
) {
    serving_with(bytes, |_| {}).await
}

/// The same, with the session's pre-auth deadlines tunable: production values
/// are 12 s / 10 s, which no test can afford to wait for.
async fn serving_with(
    bytes: &[u8],
    tune: impl FnOnce(&mut session::SessionState),
) -> (
    tempfile::TempDir,
    Arc<session::SessionState>,
    server::ServerHandle,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("book.mp3");
    std::fs::write(&path, bytes).expect("write payload");
    let mut state = session::SessionState::new(
        vec![session::CastSource {
            cast_id: "cst_int".into(),
            title: "Integration Book".into(),
            path,
            cover_path: None,
            alignment_path: None,
            duration_seconds: Some(12.5),
        }],
        CHUNK,
        Arc::new(|_| {}),
    )
    .expect("session");
    tune(&mut state);
    let state = Arc::new(state);
    let handle = server::start(state.clone()).await.expect("server");
    (dir, state, handle)
}

async fn serving_with_alignment(
    bytes: &[u8],
    alignment_bytes: &[u8],
) -> (
    tempfile::TempDir,
    Arc<session::SessionState>,
    server::ServerHandle,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("book.mp3");
    std::fs::write(&path, bytes).expect("write payload");
    let align_path = dir.path().join("alignment.json");
    std::fs::write(&align_path, alignment_bytes).expect("write alignment");
    let state = Arc::new(
        session::SessionState::new(
            vec![session::CastSource {
                cast_id: "cst_int".into(),
                title: "Integration Book".into(),
                path,
                cover_path: None,
                alignment_path: Some(align_path),
                duration_seconds: Some(12.5),
            }],
            CHUNK,
            Arc::new(|_| {}),
        )
        .expect("session"),
    );
    let handle = server::start(state.clone()).await.expect("server");
    (dir, state, handle)
}

async fn pair(state: &session::SessionState, port: u16, fingerprint: &str) -> String {
    let secret = state.mint_secret();
    let response = request(
        port,
        fingerprint,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret)],
    )
    .await;
    assert_eq!(response.status, 200);
    response.json()["deviceToken"]
        .as_str()
        .expect("device token")
        .to_string()
}

#[tokio::test]
async fn pairing_is_single_use_and_gates_every_endpoint() {
    let (_dir, state, handle) = serving(&payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    let unauth = request(port, &fp, "GET", "/transfer/v1/library", &[]).await;
    assert_eq!(unauth.status, 401);

    let wrong = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", "not-the-secret")],
    )
    .await;
    assert_eq!(wrong.status, 403);
    assert_eq!(wrong.json()["error"], "secret rejected");

    let secret = state.mint_secret();
    let ok = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret)],
    )
    .await;
    assert_eq!(ok.status, 200);
    let token = ok.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();

    let replay = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret)],
    )
    .await;
    assert_eq!(replay.status, 403);

    let bearer = format!("Bearer {token}");
    let library = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/library",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(library.status, 200);
    let cast = &library.json()["casts"][0];
    assert_eq!(cast["castId"], "cst_int");
    assert_eq!(cast["title"], "Integration Book");
    assert_eq!(cast["durationSeconds"], 12.5);
    assert_eq!(cast["totalSize"], 1000);

    handle.stop();
}

#[tokio::test]
async fn manifest_validates_and_matches_the_bytes_on_disk() {
    let bytes = payload(200_000);
    let (_dir, state, handle) = serving(&bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    let response = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_int/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(response.status, 200);
    let manifest: proto::Manifest = serde_json::from_slice(&response.body).expect("manifest");
    assert!(manifest.validate().is_ok(), "{:?}", manifest.validate());
    assert_eq!(manifest.cast_id, "cst_int");
    let file = &manifest.files[0];
    assert_eq!(file.path, "audio.mp3");
    assert_eq!(file.size, bytes.len() as u64);
    assert_eq!(file.sha256, hex::encode(Sha256::digest(&bytes)));
    assert_eq!(file.chunk_hashes.len() as u64, file.total_chunks());
    assert_eq!(
        file.chunk_hashes[0],
        hex::encode(Sha256::digest(&bytes[..CHUNK as usize]))
    );
    assert!(manifest.etag.starts_with("etag-"));

    handle.stop();
}

async fn fetch_range(port: u16, fp: &str, bearer: &str, etag: &str, range: String) -> RawResponse {
    request(
        port,
        fp,
        "GET",
        "/transfer/v1/casts/cst_int/files/audio.mp3",
        &[
            ("authorization", bearer),
            ("range", &range),
            ("if-range", etag),
        ],
    )
    .await
}

#[tokio::test]
async fn ranged_fetches_reassemble_byte_exact_and_report_progress() {
    let bytes = payload(300_000);
    let (_dir, state, handle) = serving(&bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    let manifest = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_int/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    let etag = manifest.json()["etag"].as_str().expect("etag").to_string();

    let mut reassembled = Vec::new();
    let mut start = 0u64;
    while start < bytes.len() as u64 {
        let end = (start + CHUNK - 1).min(bytes.len() as u64 - 1);
        let part = fetch_range(port, &fp, &bearer, &etag, format!("bytes={start}-{end}")).await;
        assert_eq!(part.status, 206);
        assert_eq!(part.header("etag"), Some(etag.as_str()));
        assert_eq!(
            part.header("content-type"),
            Some("application/octet-stream")
        );
        assert_eq!(
            part.header("content-range").expect("content-range"),
            format!("bytes {start}-{end}/{}", bytes.len())
        );
        assert_eq!(
            part.header("content-length").expect("content-length"),
            format!("{}", end - start + 1)
        );
        reassembled.extend_from_slice(&part.body);
        start = end + 1;
    }
    assert_eq!(reassembled, bytes);

    let mut complete = false;
    for _ in 0..50 {
        if state.snapshot().casts[0].sent_complete {
            complete = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(complete, "final range should mark the cast sent");

    handle.stop();
}

#[tokio::test]
async fn stale_if_range_restarts_with_the_full_body() {
    let bytes = payload(150_000);
    let (_dir, state, handle) = serving(&bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    let response = fetch_range(
        port,
        &fp,
        &bearer,
        "etag-somethingstale",
        "bytes=100000-".into(),
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, bytes);
    assert_eq!(
        response.header("content-length").expect("content-length"),
        format!("{}", bytes.len())
    );

    handle.stop();
}

#[tokio::test]
async fn out_of_bounds_ranges_and_unknown_paths_are_refused() {
    let bytes = payload(1000);
    let (_dir, state, handle) = serving(&bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    let manifest = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_int/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    let etag = manifest.json()["etag"].as_str().expect("etag").to_string();

    let beyond = fetch_range(port, &fp, &bearer, &etag, "bytes=999999-".into()).await;
    assert_eq!(beyond.status, 416);
    assert_eq!(beyond.header("content-range"), Some("bytes */1000"));

    let unknown_cast = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_nope/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(unknown_cast.status, 404);

    let unknown_file = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_int/files/audio.opus",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(unknown_file.status, 404);

    handle.stop();
}

// --- Pins for the 2026-08-31 mid-transfer death (desktop dropped TLS without
// close_notify during the final range, then refused the retry). Root cause:
// completion was recorded when frames were queued, not delivered, and stop()
// dropped the in-flight hyper connection instead of draining it. ---

/// Large enough that the writer task and every buffer between it and the
/// client (frame channel + hyper + TLS + loopback socket buffers) cannot hold
/// the whole body while the client has only read the first slice.
const IN_FLIGHT_BYTES: usize = 32 * 1024 * 1024;

#[tokio::test]
async fn stop_mid_download_drains_the_final_range_with_close_notify() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let bytes = payload(IN_FLIGHT_BYTES);
    let (_dir, state, handle) = serving(&bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;

    let mut tls = lan_pin_client::try_connect(port, &fp, &[&rustls::version::TLS13])
        .await
        .expect("handshake");
    let head = format!(
        "GET /transfer/v1/casts/cst_int/files/audio.mp3 HTTP/1.1\r\n\
         host: txtcast.local\r\nauthorization: Bearer {token}\r\n\
         range: bytes=0-\r\nconnection: close\r\ncontent-length: 0\r\n\r\n"
    );
    tls.write_all(head.as_bytes()).await.expect("write request");

    // Read only the first slice so the rest of the body is still in flight.
    let mut received = vec![0u8; 64 * 1024];
    let mut got = 0;
    while got < received.len() {
        let n = tls.read(&mut received[got..]).await.expect("read head");
        assert!(n > 0, "server closed before the first slice arrived");
        got += n;
    }
    assert!(
        !state.snapshot().casts[0].sent_complete,
        "sent_complete flipped while megabytes were still undelivered"
    );

    // The death shape: stopping here used to drop the connection future,
    // truncating the body and skipping close_notify.
    handle.stop();

    let mut rest = Vec::new();
    tls.read_to_end(&mut rest)
        .await
        .expect("stream must end with a clean TLS close_notify after stop");
    received.extend_from_slice(&rest);

    let split = received
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header terminator");
    let status: u16 = String::from_utf8_lossy(&received[..split])
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status");
    assert_eq!(status, 206);
    assert_eq!(
        &received[split + 4..],
        &bytes[..],
        "drained body must be byte-exact"
    );
    assert!(state.snapshot().casts[0].sent_complete);

    // The listener itself must be gone: a stopped sender releases the port.
    let mut refused = false;
    for _ in 0..100 {
        if lan_pin_client::try_connect(port, &fp, &[&rustls::version::TLS13])
            .await
            .is_err()
        {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(refused, "stopped sender still accepting connections");
}

async fn serving_many(
    sizes: &[usize],
) -> (
    tempfile::TempDir,
    Vec<Vec<u8>>,
    Arc<session::SessionState>,
    server::ServerHandle,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sources = Vec::new();
    let mut bodies = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        let body = payload(*size);
        let path = dir.path().join(format!("book{i}.mp3"));
        std::fs::write(&path, &body).expect("write payload");
        bodies.push(body);
        sources.push(session::CastSource {
            cast_id: format!("cst_b{i}"),
            title: format!("Book {i}"),
            path,
            cover_path: None,
            alignment_path: None,
            duration_seconds: None,
        });
    }
    let state =
        Arc::new(session::SessionState::new(sources, CHUNK, Arc::new(|_| {})).expect("session"));
    let handle = server::start(state.clone()).await.expect("server");
    (dir, bodies, state, handle)
}

#[tokio::test]
async fn multi_book_sequential_transfer_completes_byte_verified() {
    let sizes = [100_000, 130_000, 90_000, 300_000];
    let (_dir, bodies, state, handle) = serving_many(&sizes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    for (i, body) in bodies.iter().enumerate() {
        assert_ne!(
            state.snapshot().phase,
            "done",
            "phase reported done before book {i} was transferred"
        );
        let manifest = request(
            port,
            &fp,
            "GET",
            &format!("/transfer/v1/casts/cst_b{i}/manifest"),
            &[("authorization", &bearer)],
        )
        .await;
        let etag = manifest.json()["etag"].as_str().expect("etag").to_string();
        let mut reassembled = Vec::new();
        let mut start = 0u64;
        while start < body.len() as u64 {
            let end = (start + CHUNK - 1).min(body.len() as u64 - 1);
            let part = request(
                port,
                &fp,
                "GET",
                &format!("/transfer/v1/casts/cst_b{i}/files/audio.mp3"),
                &[
                    ("authorization", &bearer),
                    ("range", &format!("bytes={start}-{end}")),
                    ("if-range", &etag),
                ],
            )
            .await;
            assert_eq!(part.status, 206);
            reassembled.extend_from_slice(&part.body);
            start = end + 1;
        }
        assert_eq!(
            hex::encode(Sha256::digest(&reassembled)),
            hex::encode(Sha256::digest(body)),
            "book {i} bytes must survive the wire"
        );
        let mut complete = false;
        for _ in 0..100 {
            if state.snapshot().casts[i].sent_complete {
                complete = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(complete, "book {i} never marked sent");
    }
    assert_eq!(state.snapshot().phase, "done");

    handle.stop();
}

#[tokio::test]
async fn receipt_endpoint_auth_oversized_and_idempotency() {
    let raw = payload(5000);
    let (_dir, state, handle) = serving(&raw).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    // 1. Missing or invalid auth
    let unauth = request_with_body(port, &fp, "POST", "/transfer/v1/receipt", &[], b"{}").await;
    assert_eq!(unauth.status, 401);

    let bad_auth = request_with_body(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", "Bearer tok_invalid")],
        b"{}",
    )
    .await;
    assert_eq!(bad_auth.status, 401);

    // 2. Oversized payload (> 64 KiB)
    let huge = vec![b'a'; 70 * 1024];
    let oversized = request_with_body(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", &bearer)],
        &huge,
    )
    .await;
    assert_eq!(oversized.status, 413);

    // 3. Malformed JSON
    let bad_json = request_with_body(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", &bearer)],
        b"not json",
    )
    .await;
    assert_eq!(bad_json.status, 400);

    // 4. Unknown cast
    let unknown_receipt = serde_json::json!({
        "castId": "cst_unknown",
        "files": [{
            "path": "audio.mp3",
            "sha256": hex::encode(Sha256::digest(&raw)),
            "action": "reused"
        }]
    });
    let unknown_bytes = serde_json::to_vec(&unknown_receipt).unwrap();
    let not_found = request_with_body(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", &bearer)],
        &unknown_bytes,
    )
    .await;
    assert_eq!(not_found.status, 404);

    // 5. Valid receipt flips phase to done without served ranges
    assert_eq!(state.snapshot().phase, "paired");
    let valid_receipt = serde_json::json!({
        "castId": "cst_int",
        "files": [{
            "path": "audio.mp3",
            "sha256": hex::encode(Sha256::digest(&raw)),
            "action": "reused"
        }]
    });
    let valid_bytes = serde_json::to_vec(&valid_receipt).unwrap();
    let ok = request_with_body(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", &bearer)],
        &valid_bytes,
    )
    .await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.json()["status"], "ok");
    assert!(state.snapshot().casts[0].sent_complete);
    assert_eq!(state.snapshot().phase, "done");

    // 6. Double-receipt call is idempotent
    let ok2 = request_with_body(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", &bearer)],
        &valid_bytes,
    )
    .await;
    assert_eq!(ok2.status, 200);
    assert_eq!(ok2.json()["status"], "ok");
    assert!(state.snapshot().casts[0].sent_complete);
    assert_eq!(state.snapshot().phase, "done");

    handle.stop();
}

// ---------------------------------------------------------------------------
// Pre-auth resource bounds: deadlines, admission control, backoff
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_peer_that_stalls_in_the_tls_handshake_is_dropped_after_the_deadline() {
    let raw = payload(5000);
    let (_dir, _state, handle) = serving_with(&raw, |state| {
        state.handshake_deadline = Duration::from_millis(150);
    })
    .await;
    let port = handle.port();

    // Connect and send nothing at all — no ClientHello. Without the deadline
    // this parked one task, one socket and one hyper read buffer until the peer
    // chose to hang up.
    let mut stalled = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(5), stalled.read(&mut buf))
        .await
        .expect("a stalled handshake must be dropped, not parked");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "a stalled handshake must be closed, not answered"
    );

    handle.stop();
}

#[tokio::test]
async fn a_peer_that_sends_half_a_header_is_dropped_by_the_header_deadline() {
    let raw = payload(5000);
    let (_dir, _state, handle) = serving_with(&raw, |state| {
        state.header_read_timeout = Duration::from_millis(200);
    })
    .await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    let mut tls = try_connect(port, &fp, &[&rustls::version::TLS13])
        .await
        .expect("handshake");
    // A request head that never terminates.
    tls.write_all(b"GET /transfer/v1/library HTTP/1.1\r\nhost: txtcast.local\r\n")
        .await
        .expect("write half a head");

    let started = std::time::Instant::now();
    let mut seen = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut seen))
        .await
        .expect("half a header must be dropped by the deadline, not parked");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the header deadline, not the test timeout, must end the connection"
    );
    assert_ne!(
        status_of(&seen),
        Some(200),
        "a half head must never be served"
    );

    handle.stop();
}

#[tokio::test]
async fn with_the_header_deadline_configured_a_normal_request_is_still_served() {
    // Guards the TokioTimer/header_read_timeout pairing: hyper panics on
    // connection setup when the timeout is configured without a timer, so a
    // missing `.timer(..)` surfaces here as a request that is never answered.
    let raw = payload(5000);
    let (_dir, state, handle) = serving(&raw).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    let library = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/library",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(library.status, 200);
    assert_eq!(library.json()["casts"].as_array().map(Vec::len), Some(1));

    handle.stop();
}

#[tokio::test]
async fn at_capacity_connections_are_shed_and_permits_come_back() {
    let raw = payload(5000);
    let (_dir, state, handle) = serving_with(&raw, |state| {
        // Hold parked handshakes open for the whole test, so the permits really
        // are unavailable while we probe the ceiling.
        state.handshake_deadline = Duration::from_secs(10);
    })
    .await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    // Twice the ceiling, so every accept past the cap is shed no matter how the
    // backlog drains. Raw TCP only: each connection parks in the handshake.
    let mut parked = Vec::new();
    for _ in 0..(2 * server::MAX_CONCURRENT_CONNECTIONS) {
        parked.push(
            TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("park a connection"),
        );
    }
    for _ in 0..40 {
        if state.snapshot().shed_connections > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        state.snapshot().shed_connections > 0,
        "connections past the ceiling must be shed and counted for support"
    );

    // Releasing the parked peers must hand their permits back (RAII): a real
    // client is served again with no restart and no leaked capacity.
    drop(parked);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");
    let library = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/library",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(
        library.status, 200,
        "permits must come back once the shed peers disconnect"
    );

    handle.stop();
}

#[tokio::test]
async fn chunked_receipt_over_the_cap_is_refused_while_the_peer_is_still_sending() {
    let raw = payload(5000);
    let (_dir, state, handle) = serving(&raw).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    // The cap used to be read from `content-length`, so a peer framing its body
    // with `transfer-encoding: chunked` skipped the early refusal and was
    // buffered whole before the check. 128 KiB in 32 KiB chunks with the final
    // zero-length chunk withheld: a reader bounded at the cap answers 413 while
    // the peer has not finished, a reader that collects first waits forever.
    let body = vec![b'{'; 128 * 1024];
    let answered = tokio::time::timeout(
        Duration::from_secs(5),
        open_ended_chunked_request(
            port,
            &fp,
            "POST",
            "/transfer/v1/receipt",
            &[("authorization", &bearer)],
            &body,
            32 * 1024,
        ),
    )
    .await
    .expect("an over-cap chunked receipt must be answered, not parked");

    assert_eq!(
        status_of(&answered),
        Some(413),
        "an over-cap chunked receipt must be refused with 413"
    );

    handle.stop();
}

#[tokio::test]
async fn chunked_receipt_under_the_cap_still_reads_the_body() {
    let raw = payload(5000);
    let (_dir, state, handle) = serving(&raw).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    // The same framing, complete and under the cap: bounding the read must not
    // turn a legal chunked request into a 413 — malformed JSON is still 400.
    let answered = chunked_request(
        port,
        &fp,
        "POST",
        "/transfer/v1/receipt",
        &[("authorization", &bearer)],
        b"not json",
        4,
    )
    .await;
    assert_eq!(status_of(&answered), Some(400));

    handle.stop();
}

#[tokio::test]
async fn multifile_cast_audio_complete_with_alignment_pending_does_not_flip_to_done() {
    let raw = payload(5000);
    let align_bytes = b"{\"segments\":[]}";
    let (_dir, state, handle) = serving_with_alignment(&raw, align_bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair(&state, port, &fp).await;
    let bearer = format!("Bearer {token}");

    assert_eq!(state.snapshot().phase, "paired");

    // 1. Request and complete the audio file
    let audio_resp = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_int/files/audio.mp3",
        &[
            ("authorization", &bearer),
            ("range", &format!("bytes=0-{}", raw.len() - 1)),
        ],
    )
    .await;
    assert_eq!(audio_resp.status, 206);
    assert_eq!(audio_resp.body.len(), raw.len());

    // Wait briefly for drain unfolding to complete
    for _ in 0..50 {
        if state.snapshot().casts[0].bytes_sent >= raw.len() as u64 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Audio is complete, but alignment is still pending: sent_complete must be false and phase must NOT be done
    let snap = state.snapshot();
    assert!(
        !snap.casts[0].sent_complete,
        "cast must not be sent_complete while alignment is pending"
    );
    assert_ne!(
        snap.phase, "done",
        "phase must not be done while alignment is pending"
    );
    assert_eq!(snap.phase, "sending");

    // 2. Request and complete the alignment file
    let align_resp = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_int/files/alignment.json",
        &[
            ("authorization", &bearer),
            ("range", &format!("bytes=0-{}", align_bytes.len() - 1)),
        ],
    )
    .await;
    assert_eq!(align_resp.status, 206);
    assert_eq!(align_resp.body.len(), align_bytes.len());

    for _ in 0..50 {
        if state.snapshot().casts[0].sent_complete {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Now all files have completed: sent_complete is true and phase is done
    let snap_done = state.snapshot();
    assert!(
        snap_done.casts[0].sent_complete,
        "cast must be sent_complete when all files complete"
    );
    assert_eq!(snap_done.phase, "done");

    handle.stop();
}

#[test]
fn app_shutdown_stops_the_lan_sender() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("lib source");
    let start = source.find("async fn shutdown_app").expect("shutdown_app");
    let end = source[start..]
        .find("\n}\n")
        .map(|offset| start + offset)
        .unwrap_or(source.len());
    let body = &source[start..end];
    assert!(
        body.contains("stop_lan_sender("),
        "shutdown must run the extracted LAN stop; the webview's React cleanup never runs on a \
         webview crash or hard reload, so without this the listener and its non-expiring \
         tokens keep serving exports until process exit"
    );
    let stopper_start = source
        .find("pub fn stop_lan_sender")
        .expect("stop_lan_sender");
    let stopper_end = source[stopper_start..]
        .find("\n}\n")
        .map(|offset| stopper_start + offset)
        .unwrap_or(source.len());
    let stopper = &source[stopper_start..stopper_end];
    assert!(
        stopper.contains("LanSenderHandle") && stopper.contains("server.stop()"),
        "the pinned stop body must fetch the managed handle AND stop its server; a lone type \
         token survives deleting the stop itself"
    );
}

#[tokio::test]
async fn stop_lan_sender_stops_the_managed_listener() {
    let (_dir, session, server) = serving(&payload(1000)).await;
    let port = server.port();
    let fp = server.fingerprint().to_string();

    try_connect(port, &fp, &[&rustls::version::TLS13])
        .await
        .expect("listener serves before the stop");

    let app = tauri::test::mock_app();
    let sender = session::LanSenderHandle::default();
    *sender.active() = Some(session::ActiveSend {
        session,
        server,
        export_ids: std::collections::HashSet::new(),
        hosts: Vec::new(),
    });
    app.manage(sender);

    txtcaster_lib::stop_lan_sender(app.handle());

    assert!(
        app.state::<session::LanSenderHandle>().active().is_none(),
        "the stop must take the active send out of the handle; a leftover entry would advertise \
         a dead server to a later send"
    );
    let mut refused = false;
    for _ in 0..100 {
        if try_connect(port, &fp, &[&rustls::version::TLS13])
            .await
            .is_err()
        {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        refused,
        "stop_lan_sender must stop the listener held by the managed handle; new connections \
         must be refused"
    );
}
