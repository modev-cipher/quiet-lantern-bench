//! Adversarial coverage for the desktop LAN sender: hostile-LAN attacks that
//! the contract tests in `lan_sender.rs` deliberately leave uncovered. Every
//! test here must pass against the source unmodified — no guard is weakened to
//! make an assertion green. The sabotage section proves each survival test is
//! load-bearing by gutting the guard it pins and observing the test go red.

#[path = "../src/test_support/lan_pin_client.rs"]
mod lan_pin_client;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use lan_pin_client::{raw_request, request, status_of, try_connect};
use rustls::version::{TLS12, TLS13};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use txtcaster_lib::lan_sender::server::{plan_range, RangePlan};
use txtcaster_lib::lan_sender::session::PairOutcome;
use txtcaster_lib::lan_sender::{proto, server, session};

const CHUNK: u64 = proto::MIN_CHUNK_SIZE as u64;

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

/// Boots a live TLS listener over a single-cast session. Each caller passes a
/// unique cast id + payload so the process-global hash cache never leaks meta
/// between tests.
async fn serving(
    cast_id: &str,
    bytes: &[u8],
) -> (
    tempfile::TempDir,
    Arc<session::SessionState>,
    server::ServerHandle,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("{cast_id}.mp3"));
    std::fs::write(&path, bytes).expect("write payload");
    let state = Arc::new(
        session::SessionState::new(
            vec![session::CastSource {
                cast_id: cast_id.into(),
                title: format!("Book {cast_id}"),
                path,
                cover_path: None,
                alignment_path: None,
                duration_seconds: Some(9.0),
            }],
            CHUNK,
            Arc::new(|_| {}),
        )
        .expect("session"),
    );
    let handle = server::start(state.clone()).await.expect("server");
    (dir, state, handle)
}

async fn pair_with(
    state: &session::SessionState,
    port: u16,
    fp: &str,
    device_name: &str,
) -> lan_pin_client::RawResponse {
    let secret = state.mint_secret();
    request(
        port,
        fp,
        "POST",
        "/transfer/v1/pair",
        &[
            ("x-pairing-secret", &secret),
            ("x-device-name", device_name),
        ],
    )
    .await
}

// ---------------------------------------------------------------------------
// Target 1 — TLS pinning + version floor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wrong_fingerprint_fails_handshake_with_no_http_fallback() {
    let (_dir, _state, handle) = serving("cst_pin", &payload(1000)).await;
    let port = handle.port();

    // A phone pinned to the wrong cert fingerprint must fail the TLS handshake
    // outright — never silently downgrade to plaintext or an unpinned exchange.
    let bogus = "0".repeat(64);
    let result = try_connect(port, &bogus, &[&TLS13]).await;
    assert!(
        result.is_err(),
        "a mismatched pin must abort the handshake, not connect"
    );

    // Control: the correct fingerprint still completes, so the rejection above
    // is the pin doing its job and not a dead listener.
    let ok = try_connect(port, handle.fingerprint(), &[&TLS13]).await;
    assert!(ok.is_ok(), "correct pin should handshake");

    handle.stop();
}

#[tokio::test]
async fn tls12_client_is_rejected_by_the_tls13_only_listener() {
    let (_dir, _state, handle) = serving("cst_tls", &payload(1000)).await;
    let port = handle.port();
    let fp = handle.fingerprint().to_string();

    // The server pins `with_protocol_versions(&[&TLS13])`. A client capped at
    // TLS1.2 (even with the correct fingerprint) has no shared version and must
    // fail the handshake — proving the version floor is load-bearing.
    let downgraded = try_connect(port, &fp, &[&TLS12]).await;
    assert!(
        downgraded.is_err(),
        "a TLS1.2-max client must not negotiate against a TLS1.3-only server"
    );

    let ok = try_connect(port, &fp, &[&TLS13]).await;
    assert!(ok.is_ok(), "a TLS1.3 client with the right pin connects");

    handle.stop();
}

// ---------------------------------------------------------------------------
// Target 2 — Secret brute force / lock boundary / no length oracle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nine_wrong_attempts_then_correct_secret_still_pairs() {
    let (_dir, state, handle) = serving("cst_nine", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let secret = state.mint_secret();

    // The lock trips at PAIR_FAILURE_LIMIT (10). Nine failures must leave the
    // door open — an off-by-one here would lock out a legitimate phone early.
    for _ in 0..9 {
        let wrong = request(
            port,
            &fp,
            "POST",
            "/transfer/v1/pair",
            &[("x-pairing-secret", "nope")],
        )
        .await;
        assert_eq!(wrong.status, 403);
    }
    assert!(!state.snapshot().pair_locked, "9 failures must not lock");

    let ok = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret)],
    )
    .await;
    assert_eq!(
        ok.status, 200,
        "the 10th attempt with the real secret pairs"
    );

    handle.stop();
}

#[tokio::test]
async fn tenth_wrong_attempt_locks_and_correct_secret_is_then_refused() {
    let (_dir, state, handle) = serving("cst_lock", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let secret = state.mint_secret();

    for _ in 0..10 {
        let wrong = request(
            port,
            &fp,
            "POST",
            "/transfer/v1/pair",
            &[("x-pairing-secret", "nope")],
        )
        .await;
        assert_eq!(wrong.status, 403);
    }
    assert!(
        state.snapshot().pair_locked,
        "10 failures must lock the slot"
    );

    // The 10th failure clears the stored secret, so even the genuine secret is
    // now refused until a fresh QR is minted.
    let locked = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret)],
    )
    .await;
    assert_eq!(locked.status, 403, "a locked slot rejects the real secret");

    handle.stop();
}

#[tokio::test]
async fn used_secret_replay_is_refused_without_burning_the_failure_budget() {
    let (_dir, state, handle) = serving("cst_replay", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let secret = state.mint_secret();

    let first = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret)],
    )
    .await;
    assert_eq!(first.status, 200);

    let mut last_replay = None;
    for _ in 0..25 {
        let replay = request(
            port,
            &fp,
            "POST",
            "/transfer/v1/pair",
            &[("x-pairing-secret", &secret)],
        )
        .await;
        assert_eq!(replay.status, 403, "a consumed secret never re-pairs");
        last_replay = Some(replay);
    }
    assert!(
        !state.snapshot().pair_locked,
        "consumed-secret replays must not walk the failure counter to lockout"
    );

    let wrong = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", "ffffffffffffffffffffffffffffffff")],
    )
    .await;
    let replay = last_replay.expect("replay response captured");
    assert_eq!(replay.status, wrong.status);
    assert_eq!(replay.headers, wrong.headers);
    assert_eq!(replay.body, wrong.body);

    handle.stop();
}

#[tokio::test]
async fn near_miss_and_far_miss_secrets_are_both_refused() {
    let (_dir, state, handle) = serving("cst_oracle", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let secret = state.mint_secret();

    // digest_eq hashes both sides to fixed 32-byte SHA-256 digests before
    // comparing, so neither a same-length near-miss (last hex nibble flipped)
    // nor a wildly different-length guess can shorten or lengthen the compare.
    // Both are rejected identically; there is no length-correlated fast path.
    let mut near = secret.clone();
    let last = near.pop().expect("non-empty secret");
    near.push(if last == 'a' { 'b' } else { 'a' });
    assert_eq!(near.len(), secret.len());

    for guess in [near.as_str(), "", "x", &"z".repeat(4096)] {
        let response = request(
            port,
            &fp,
            "POST",
            "/transfer/v1/pair",
            &[("x-pairing-secret", guess)],
        )
        .await;
        assert_eq!(response.status, 403, "guess {:?} must be refused", guess);
    }

    handle.stop();
}

// ---------------------------------------------------------------------------
// Target 3 — Tokens / auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tokens_do_not_survive_a_fresh_session() {
    let (_dir, state, handle) = serving("cst_tok", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let paired = pair_with(&state, port, &fp, "Phone").await;
    assert_eq!(paired.status, 200);
    let token = paired.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    handle.stop();

    // Tokens live only in the session's in-memory set. A brand-new session (a
    // fresh QR / restart) has never seen the old token, so it is invalid.
    let fresh = session::SessionState::new(
        vec![session::CastSource {
            cast_id: "cst_tok".into(),
            title: "Book".into(),
            path: std::env::temp_dir().join("cst_tok_absent.mp3"),
            cover_path: None,
            alignment_path: None,
            duration_seconds: None,
        }],
        CHUNK,
        Arc::new(|_| {}),
    )
    .expect("session");
    assert!(
        !fresh.token_valid(&token),
        "a token minted by a prior session must not authorize a new one"
    );
}

#[tokio::test]
async fn every_protected_endpoint_rejects_bad_authorization() {
    let (_dir, state, handle) = serving("cst_auth", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let paired = pair_with(&state, port, &fp, "Phone").await;
    let token = paired.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let valid = format!("Bearer {token}");

    let endpoints = [
        "/transfer/v1/library",
        "/transfer/v1/casts/cst_auth/manifest",
        "/transfer/v1/casts/cst_auth/files/audio.mp3",
    ];
    // Bearer parsing is exact and case-sensitive: no header, an empty bearer, a
    // wrong token, a Basic scheme, and lowercase `bearer` all fail closed.
    let bad_headers: [&[(&str, &str)]; 5] = [
        &[],
        &[("authorization", "Bearer ")],
        &[("authorization", "Bearer deadbeefdeadbeef")],
        &[("authorization", "Basic dXNlcjpwYXNz")],
        &[("authorization", &format!("bearer {token}"))],
    ];

    for endpoint in endpoints {
        for headers in bad_headers.iter() {
            let response = request(port, &fp, "GET", endpoint, headers).await;
            assert_eq!(
                response.status, 401,
                "{endpoint} with {headers:?} must be unauthorized"
            );
        }
        // Control: the genuine bearer is accepted (not 401), proving the
        // endpoint is reachable and the rejections above are about auth alone.
        let ok = request(port, &fp, "GET", endpoint, &[("authorization", &valid)]).await;
        assert_ne!(ok.status, 401, "{endpoint} must accept the real token");
    }

    handle.stop();
}

#[tokio::test]
async fn garbage_pair_headers_are_handled_without_panic() {
    let (_dir, state, handle) = serving("cst_hdr", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    // Missing secret header → treated as the empty string → 403, never a panic
    // or an accidental 200.
    let missing = request(port, &fp, "POST", "/transfer/v1/pair", &[]).await;
    assert_eq!(missing.status, 403);

    // Oversized secret (64 KiB) → rejected without OOM; the listener stays up.
    let huge = "A".repeat(64 * 1024);
    let oversized = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &huge)],
    )
    .await;
    assert_eq!(oversized.status, 403);

    // A hostile device name (control chars + emoji + overlong) still pairs, but
    // sanitize_device_name strips it to printable ASCII so the roster stays
    // clean — no control bytes reach the UI.
    let secret = state.mint_secret();
    // Header-legal but hostile bytes: a tab (stripped as non-graphic), emoji
    // (non-ASCII, stripped), surrounding whitespace (trimmed), and an overlong
    // tail (truncated to the limit). CR/LF and C0 controls are excluded because
    // hyper rejects them at parse time before sanitize ever runs.
    let nasty = format!("  evil\tphone{}\u{1F4A5}  ", "z".repeat(500));
    let paired = request(
        port,
        &fp,
        "POST",
        "/transfer/v1/pair",
        &[("x-pairing-secret", &secret), ("x-device-name", &nasty)],
    )
    .await;
    assert_eq!(paired.status, 200);
    let names = state.snapshot().device_names;
    assert_eq!(names.len(), 1);
    assert!(
        names[0].chars().all(|c| c.is_ascii_graphic() || c == ' '),
        "device name must be sanitized to printable ASCII, got {:?}",
        names[0]
    );
    assert!(!names[0].contains('\r') && !names[0].contains('\n'));

    handle.stop();
}

// ---------------------------------------------------------------------------
// Target 4 — Malformed HTTP is shed; one stalled peer does not starve others
// ---------------------------------------------------------------------------

/// Proves the listener is still routing by driving an unauthenticated request
/// that must reach the router and come back 401 (not a dropped connection).
async fn still_serving(port: u16, fp: &str) -> bool {
    request(port, fp, "GET", "/transfer/v1/library", &[])
        .await
        .status
        == 401
}

#[tokio::test]
async fn malformed_requests_are_shed_and_the_listener_survives() {
    let (_dir, _state, handle) = serving("cst_bad", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    // Garbage request line: hyper answers 4xx or closes; either way the process
    // must not panic and the listener must keep serving.
    let garbage = raw_request(
        port,
        &fp,
        b"GLORP /nonsense HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
    )
    .await;
    if let Some(status) = status_of(&garbage) {
        assert!(status >= 400, "garbage request line should not 2xx/3xx");
    }
    assert!(
        still_serving(port, &fp).await,
        "listener died after garbage"
    );

    // Oversized header block (80 KiB single value): rejected/closed, no OOM.
    let mut oversized =
        Vec::from(&b"GET /transfer/v1/library HTTP/1.1\r\nhost: x\r\nx-flood: "[..]);
    oversized.extend(std::iter::repeat(b'A').take(80 * 1024));
    oversized.extend_from_slice(b"\r\nconnection: close\r\n\r\n");
    let flooded = raw_request(port, &fp, &oversized).await;
    if let Some(status) = status_of(&flooded) {
        assert!(status >= 400, "oversized header should not 2xx/3xx");
    }
    assert!(still_serving(port, &fp).await, "listener died after flood");

    // A path outside the transfer namespace is a clean 404.
    let stray = request(port, &fp, "GET", "/etc/passwd", &[]).await;
    assert_eq!(stray.status, 404);

    handle.stop();
}

#[tokio::test]
async fn one_stalled_connection_does_not_block_other_peers() {
    let (_dir, _state, handle) = serving("cst_stall", &payload(1000)).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    // Open a pinned connection, complete the TLS handshake, then send only a
    // partial request head and never finish it. There is no handshake/header
    // read timeout (owner finding: an unbounded number of these parks one tokio
    // task each), so this connection sits open. The liveness contract we CAN
    // prove: it must not stop a well-behaved peer from being served.
    let mut stalled = try_connect(port, &fp, &[&TLS13])
        .await
        .expect("stall handshake");
    stalled
        .write_all(b"GET /transfer/v1/library HTTP/1.1\r\nhost: x\r\n")
        .await
        .expect("partial write");
    stalled.flush().await.expect("flush partial");

    // A separate connection is served immediately despite the parked one.
    assert!(
        still_serving(port, &fp).await,
        "a stalled peer must not starve a healthy one"
    );

    drop(stalled);
    handle.stop();
}

// ---------------------------------------------------------------------------
// Target 5 — Peer gate classifier (reject public, admit local)
// ---------------------------------------------------------------------------

#[test]
fn peer_allowed_rejects_public_and_admits_local() {
    // Rejected: unspecified, broadcast, CGNAT (Rust's is_private excludes it),
    // TEST-NET, public v4, unspecified v6, public v6, a v4-mapped public
    // address (canonicalization must unwrap before classifying), and the
    // deprecated site-local fec0::/10.
    let rejected = [
        "0.0.0.0",
        "255.255.255.255",
        "100.64.0.1",
        "192.0.2.1",
        "8.8.8.8",
        "::",
        "2001:4860:4860::8888",
        "::ffff:8.8.8.8",
        "fec0::1",
    ];
    for ip in rejected {
        let parsed: IpAddr = ip.parse().expect("ip");
        assert!(
            !session::peer_allowed(&parsed),
            "{ip} is a non-local peer and must be refused"
        );
    }

    // Admitted: loopback, RFC1918 (all three blocks), v4 link-local, v6
    // loopback, ULA, v6 link-local, and a v4-mapped private address.
    let admitted = [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.5",
        "169.254.1.1",
        "::1",
        "fc00::1",
        "fe80::1",
        "::ffff:10.0.0.1",
    ];
    for ip in admitted {
        let parsed: IpAddr = ip.parse().expect("ip");
        assert!(
            session::peer_allowed(&parsed),
            "{ip} is a local peer and must be admitted"
        );
    }
}

// ---------------------------------------------------------------------------
// Target 6 — Range planning edges, forged If-Range, bounded range storm
// ---------------------------------------------------------------------------

#[test]
fn plan_range_edge_battery() {
    // Malformed starts and RFC-legal-but-unsupported shapes are all Invalid.
    for spec in [
        "bytes=18446744073709551616-", // u64 overflow start
        "bytes= 0-10",                 // leading whitespace in start
        "bytes=-50",                   // suffix range (rejected by design)
        "bytes=100-",                  // start == size
    ] {
        assert!(
            matches!(plan_range(Some(spec), None, "e", 100), RangePlan::Invalid),
            "{spec} should be Invalid"
        );
    }

    // FINDING (minor spec leniency): Rust's u64 FromStr accepts a leading '+',
    // so `bytes=+5-10` is parsed as a normal partial rather than rejected. It
    // stays in-bounds and is harmless, but a strict RFC 7233 parser would 416.
    // Pinned so the behavior is at least intentional and cannot silently drift.
    assert!(matches!(
        plan_range(Some("bytes=+5-10"), None, "e", 100),
        RangePlan::Partial(5, 10)
    ));

    // Last single byte is a valid partial.
    assert!(matches!(
        plan_range(Some("bytes=99-"), None, "e", 100),
        RangePlan::Partial(99, 99)
    ));
    // An end past EOF clamps to size-1 rather than erroring.
    assert!(matches!(
        plan_range(Some("bytes=0-999999"), None, "e", 100),
        RangePlan::Partial(0, 99)
    ));
    // A zero-length file cannot satisfy an open-ended range.
    assert!(matches!(
        plan_range(Some("bytes=0-"), None, "e", 0),
        RangePlan::Invalid
    ));
}

#[tokio::test]
async fn forged_if_range_restarts_full_and_current_etag_out_of_bounds_is_416() {
    let bytes = payload(200_000);
    let (_dir, state, handle) = serving("cst_ifrange", &bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair_with(&state, port, &fp, "Phone").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let bearer = format!("Bearer {token}");

    let manifest = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_ifrange/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    let etag = manifest.json()["etag"].as_str().expect("etag").to_string();

    // A forged If-Range that does not match the current etag forces a full
    // restart from byte 0, and the response advertises the CURRENT etag — the
    // server never echoes the attacker-supplied validator.
    let forged = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_ifrange/files/audio.mp3",
        &[
            ("authorization", &bearer),
            ("range", "bytes=100-200"),
            ("if-range", "etag-deadbeef00000000"),
        ],
    )
    .await;
    assert_eq!(forged.status, 200, "stale If-Range must restart in full");
    assert_eq!(forged.body, bytes, "full restart returns the whole file");
    assert_eq!(forged.header("etag"), Some(etag.as_str()));

    // A matching If-Range with an out-of-bounds range is a clean 416 — it must
    // not silently fall back to the full body.
    let unsat = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_ifrange/files/audio.mp3",
        &[
            ("authorization", &bearer),
            ("range", "bytes=999999-"),
            ("if-range", &etag),
        ],
    )
    .await;
    assert_eq!(unsat.status, 416);
    assert_eq!(
        unsat.header("content-range"),
        Some(format!("bytes */{}", bytes.len()).as_str())
    );

    handle.stop();
}

#[tokio::test]
async fn concurrent_range_storm_reassembles_byte_exact_without_underflow() {
    let bytes = payload(320_000);
    let (_dir, state, handle) = serving("cst_storm", &bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair_with(&state, port, &fp, "Phone").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let bearer = format!("Bearer {token}");

    let manifest = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_storm/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    let etag = manifest.json()["etag"].as_str().expect("etag").to_string();

    // Fire 32 non-overlapping chunk ranges concurrently. Each must come back
    // byte-exact; reassembly reproduces the whole file; the aggregate progress
    // counter is capped at the total (never overflowing past the file size).
    let total = bytes.len() as u64;
    let slice = total.div_ceil(32);
    let mut tasks = Vec::new();
    for i in 0..32u64 {
        let start = i * slice;
        if start >= total {
            break;
        }
        let end = (start + slice - 1).min(total - 1);
        let (fp, bearer, etag) = (fp.clone(), bearer.clone(), etag.clone());
        tasks.push(tokio::spawn(async move {
            let response = request(
                port,
                &fp,
                "GET",
                "/transfer/v1/casts/cst_storm/files/audio.mp3",
                &[
                    ("authorization", &bearer),
                    ("range", &format!("bytes={start}-{end}")),
                    ("if-range", &etag),
                ],
            )
            .await;
            assert_eq!(response.status, 206);
            (start, response.body)
        }));
    }

    let mut reassembled = vec![0u8; bytes.len()];
    for task in tasks {
        let (start, body) = task.await.expect("range task");
        reassembled[start as usize..start as usize + body.len()].copy_from_slice(&body);
    }
    assert_eq!(
        reassembled, bytes,
        "concurrent ranges must reassemble exactly"
    );

    let cast = &state.snapshot().casts[0];
    assert!(
        cast.bytes_sent <= cast.bytes_total,
        "aggregate progress must never exceed the file size (got {} of {})",
        cast.bytes_sent,
        cast.bytes_total
    );

    handle.stop();
}

// ---------------------------------------------------------------------------
// Target 7 — Mid-transfer chaos
// ---------------------------------------------------------------------------

#[tokio::test]
async fn file_replaced_between_manifest_and_fetch_yields_a_new_etag_and_body() {
    let first = payload(120_000);
    let (dir, state, handle) = serving("cst_swap", &first).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair_with(&state, port, &fp, "Phone").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let bearer = format!("Bearer {token}");

    let manifest = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_swap/manifest",
        &[("authorization", &bearer)],
    )
    .await;
    let old_etag = manifest.json()["etag"].as_str().expect("etag").to_string();

    // Replace the file on disk with different content AND a different size, then
    // bump its mtime so ensure_meta's (size, mtime) key misses the cache and
    // recomputes. A phone still holding the old etag via If-Range gets a clean
    // full restart against the new bytes.
    let second = payload(90_000);
    let path = dir.path().join("cst_swap.mp3");
    std::fs::write(&path, &second).expect("replace file");
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("reopen")
        .set_modified(later)
        .expect("bump mtime");

    let refetch = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_swap/files/audio.mp3",
        &[
            ("authorization", &bearer),
            ("range", "bytes=0-1000"),
            ("if-range", &old_etag),
        ],
    )
    .await;
    assert_eq!(refetch.status, 200, "stale etag after a swap restarts full");
    assert_eq!(
        refetch.body, second,
        "the new bytes are served, not the old"
    );
    assert_ne!(
        refetch.header("etag"),
        Some(old_etag.as_str()),
        "the etag must change when the file changes"
    );

    handle.stop();
}

#[tokio::test]
async fn deleting_the_file_before_fetch_404s_and_the_server_survives() {
    let bytes = payload(50_000);
    let (dir, state, handle) = serving("cst_del", &bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair_with(&state, port, &fp, "Phone").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let bearer = format!("Bearer {token}");

    // Delete the backing file before any fetch touches it. ensure_meta's
    // fs::metadata fails, flips `missing`, and the file endpoint 404s — the
    // listener must keep serving other requests.
    let path = dir.path().join("cst_del.mp3");
    std::fs::remove_file(&path).expect("delete file");

    let gone = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/casts/cst_del/files/audio.mp3",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(gone.status, 404, "a deleted file must 404, not crash");
    assert!(
        state.snapshot().casts[0].missing,
        "the cast must be marked missing after a failed metadata read"
    );

    // Library still answers — the process is alive.
    let library = request(
        port,
        &fp,
        "GET",
        "/transfer/v1/library",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(library.status, 200);

    handle.stop();
}

#[tokio::test]
async fn record_range_complete_only_marks_the_final_byte() {
    // The shrink-mid-stream branch (server.rs stream_file emits an io::Error
    // "file shrank while sending" and returns BEFORE record_range_complete) is
    // safe only because a non-final range never marks the cast complete. Racing
    // a real truncation mid-stream is inherently flaky, so the deterministic
    // invariant it leans on is pinned here directly: completion fires only when
    // the range ends exactly on the last byte.
    let bytes = payload(4000);
    let (_dir, state, handle) = serving("cst_shrink", &bytes).await;
    let entry = state.cast("cst_shrink").expect("entry");
    let size = bytes.len() as u64;

    state.record_range_complete(&entry, entry.wire_path, size - 2, size);
    assert!(
        !state.snapshot().casts[0].sent_complete,
        "a range short of EOF must not mark the cast complete"
    );

    state.record_range_complete(&entry, entry.wire_path, size - 1, size);
    assert!(
        state.snapshot().casts[0].sent_complete,
        "only the final byte marks the cast complete"
    );

    handle.stop();
}

#[tokio::test]
async fn hash_cache_returns_stale_meta_on_size_and_mtime_collision() {
    // FINDING (owner): ensure_meta keys meta on (path, size, modified_ms). Two
    // different contents of identical length written within the same millisecond
    // — or with a spoofed mtime — collide, and the phone is handed the FIRST
    // content's hashes for the SECOND content's bytes. This proves the hole.
    let content_a = payload(8000);
    let (dir, state, _handle) = serving("cst_stale", &content_a).await;
    let entry = state.cast("cst_stale").expect("entry");
    let path = dir.path().join("cst_stale.mp3");

    let meta_a = state.ensure_meta(&entry).await.expect("meta a");
    let pinned_mtime = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");

    // Different bytes, identical length, forced-identical mtime.
    let content_b: Vec<u8> = content_a.iter().map(|b| b ^ 0xFF).collect();
    assert_eq!(content_a.len(), content_b.len());
    assert_ne!(content_a, content_b);
    std::fs::write(&path, &content_b).expect("write b");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("reopen")
        .set_modified(pinned_mtime)
        .expect("pin mtime");

    let meta_b = state.ensure_meta(&entry).await.expect("meta b");
    let true_b_hash = hex::encode(Sha256::digest(&content_b));
    assert_eq!(
        meta_b.sha256, meta_a.sha256,
        "stale cache returns content A's hash for content B"
    );
    assert_ne!(
        meta_b.sha256, true_b_hash,
        "the served hash does not match the bytes now on disk (the hole)"
    );
}

#[tokio::test]
async fn ninth_device_gets_a_token_but_is_absent_from_the_roster() {
    // FINDING (owner): MAX_PAIRED_DEVICES caps the device_names roster at 8, but
    // try_pair still mints a valid token for the 9th+ phone. So a 9th phone can
    // fetch, yet paired_devices/device_names undercount it and the phase math
    // that keys off paired count is skewed. Pin both halves.
    let (_dir, state, _handle) = serving("cst_many", &payload(1000)).await;

    let mut tokens = Vec::new();
    for i in 0..9 {
        let secret = state.mint_secret();
        match state.try_pair(&secret, &format!("Phone{i}")) {
            PairOutcome::Token(token) => tokens.push(token),
            PairOutcome::Rejected => panic!("device {i} should pair"),
        }
    }

    assert_eq!(tokens.len(), 9, "all nine phones receive tokens");
    for token in &tokens {
        assert!(state.token_valid(token), "every issued token is valid");
    }
    let snapshot = state.snapshot();
    assert_eq!(
        snapshot.device_names.len(),
        8,
        "the roster is capped at MAX_PAIRED_DEVICES"
    );
    assert_eq!(snapshot.paired_devices, 8, "paired count clamps at 8");
}

#[tokio::test]
async fn two_phones_fetch_the_same_cast_concurrently_byte_exact() {
    let bytes = payload(200_000);
    let (_dir, state, handle) = serving("cst_two", &bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());

    let token_a = pair_with(&state, port, &fp, "PhoneA").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let token_b = pair_with(&state, port, &fp, "PhoneB").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();

    let (fp_a, fp_b) = (fp.clone(), fp.clone());
    let fetch = |fp: String, token: String| async move {
        let bearer = format!("Bearer {token}");
        request(
            port,
            &fp,
            "GET",
            "/transfer/v1/casts/cst_two/files/audio.mp3",
            &[("authorization", &bearer)],
        )
        .await
    };
    let (a, b) = tokio::join!(fetch(fp_a, token_a), fetch(fp_b, token_b));
    assert_eq!(a.status, 200);
    assert_eq!(b.status, 200);
    assert_eq!(a.body, bytes, "phone A receives exact bytes");
    assert_eq!(b.body, bytes, "phone B receives exact bytes");

    handle.stop();
}

#[tokio::test]
async fn stop_tears_down_the_listener_and_refuses_new_connections() {
    let bytes = payload(400_000);
    let (_dir, state, handle) = serving("cst_stop", &bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair_with(&state, port, &fp, "Phone").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();

    // Kick off a large fetch, then stop the server out from under it. The
    // spawned stream task's tx.send fails and it returns without panicking; the
    // whole process staying alive to finish this test is the no-panic proof.
    let (fp_task, bearer) = (fp.clone(), format!("Bearer {token}"));
    let inflight = tokio::spawn(async move {
        let _ = request(
            port,
            &fp_task,
            "GET",
            "/transfer/v1/casts/cst_stop/files/audio.mp3",
            &[("authorization", &bearer)],
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.stop();
    inflight.abort();
    let _ = inflight.await;

    // Once stopped, the listener socket is gone: new connects are refused.
    let mut refused = false;
    for _ in 0..50 {
        if try_connect(port, &fp, &[&TLS13]).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(refused, "a stopped listener must refuse new connections");
}

// ---------------------------------------------------------------------------
// Target 9 — Containment anchors (sabotage targets 9c)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_file_name_is_refused_by_the_wire_path_check() {
    let bytes = payload(1000);
    let (_dir, state, handle) = serving("cst_wire", &bytes).await;
    let (port, fp) = (handle.port(), handle.fingerprint().to_string());
    let token = pair_with(&state, port, &fp, "Phone").await.json()["deviceToken"]
        .as_str()
        .expect("token")
        .to_string();
    let bearer = format!("Bearer {token}");

    // handle_file rejects any file name other than the cast's own wire_path.
    // Gutting `if file_name != entry.wire_path` (sabotage 9c server anchor)
    // would let an arbitrary name stream the bytes — this must stay 404.
    for name in ["audio.opus", "..%2fsecret", "audio.mp3.bak", "AUDIO.MP3"] {
        let response = request(
            port,
            &fp,
            "GET",
            &format!("/transfer/v1/casts/cst_wire/files/{name}"),
            &[("authorization", &bearer)],
        )
        .await;
        assert_eq!(response.status, 404, "file name {name:?} must 404");
    }

    handle.stop();
}

#[test]
fn resolve_within_contains_path_traversal_escapes() {
    // The DB-side containment behind `lan_send_start`'s cast list. Gutting the
    // `starts_with(root)` check (sabotage 9c DB anchor) would turn the escape
    // assertion below from Err into Some — the traversal would resolve.
    let base = tempfile::tempdir().expect("tempdir");
    let exports = base.path().join("exports");
    std::fs::create_dir(&exports).expect("exports dir");

    let inside = exports.join("book.mp3");
    std::fs::write(&inside, b"audio").expect("inside file");
    let outside = base.path().join("secret.txt");
    std::fs::write(&outside, b"password").expect("outside file");

    // An existing in-dir file resolves to Some(canonical path inside root).
    let resolved =
        txtcaster_lib::storage::export::resolve_within(&exports, inside.to_str().expect("utf8"))
            .expect("no error for in-dir file");
    assert!(resolved.is_some(), "an in-dir file must resolve");

    // An existing file OUTSIDE the exports root is refused with an error, not
    // silently resolved — this is the containment guard.
    let escaped =
        txtcaster_lib::storage::export::resolve_within(&exports, outside.to_str().expect("utf8"));
    assert!(
        escaped.is_err(),
        "a file outside the exports root must be refused, got {escaped:?}"
    );

    // A non-existent name is a benign None (becomes a `missing` cast, not a
    // traversal) — the exists()-first check.
    let ghost = txtcaster_lib::storage::export::resolve_within(&exports, "/no/such/ghost.mp3")
        .expect("no error for non-existent path");
    assert!(ghost.is_none(), "a non-existent path resolves to None");
}
