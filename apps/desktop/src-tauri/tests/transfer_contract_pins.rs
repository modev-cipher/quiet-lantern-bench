//! Cross-repo contract pin (desktop side). Every file under
//! tests/fixtures/transfer_contract is byte-identical to the copy in
//! cast-client (src-tauri/tests/fixtures/transfer_contract). This test parses
//! txtCaster-mobile (src-tauri/tests/fixtures/transfer_contract). This test parses
//! each golden with the real lan_sender code, replays the negatives against a
//! live sender, and checks the set against SHA256SUMS — so wire drift on
//! either side fails a test instead of breaking pairing in the field.
#[path = "../src/test_support/lan_pin_client.rs"]
mod lan_pin_client;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use lan_pin_client::{request, RawResponse};
use serde_json::Value;
use sha2::{Digest, Sha256};
use txtcaster_lib::lan_sender::{proto, server, session};

const FIXTURE_FILES: usize = 13;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transfer_contract")
}

#[test]
fn fixture_set_matches_its_sha256_manifest() {
    let dir = fixtures();
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).expect("SHA256SUMS");
    let mut listed = BTreeSet::new();
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (sha, name) = line.split_once("  ").expect("<sha>  <name>");
        let got = hex::encode(Sha256::digest(std::fs::read(dir.join(name)).expect(name)));
        assert_eq!(got, sha, "fixture {name} drifted from SHA256SUMS");
        listed.insert(name.to_string());
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("fixture dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n != "SHA256SUMS")
        .collect();
    assert_eq!(on_disk, listed, "fixture dir and SHA256SUMS disagree");
    assert_eq!(
        listed.len(),
        FIXTURE_FILES,
        "fixture count changed; update both repos"
    );
}

fn load(name: &str) -> Value {
    let bytes = std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn u64_of(v: &Value) -> u64 {
    v.as_u64()
        .unwrap_or_else(|| panic!("expected integer, got {v}"))
}

fn str_of(v: &Value) -> &str {
    v.as_str()
        .unwrap_or_else(|| panic!("expected string, got {v}"))
}

/// The deterministic audio bytes every fixture hash derives from (source.json).
fn source_bytes(src: &Value) -> Vec<u8> {
    assert_eq!(
        src["generator"], "byte[i] = ((i*31+7) ^ (i>>10)) & 255",
        "generator changed"
    );
    (0..u64_of(&src["length"]) as usize)
        .map(|i| (((i * 31 + 7) ^ (i >> 10)) & 255) as u8)
        .collect()
}

struct Live {
    _dir: tempfile::TempDir,
    state: Arc<session::SessionState>,
    server: server::ServerHandle,
}

/// Serves the fixture cast with the real sender, exactly as lib.rs wires it.
async fn serve_fixture_cast(src: &Value) -> Live {
    let dir = tempfile::tempdir().expect("tempdir");
    let audio = dir.path().join(str_of(&src["sourceFileName"]));
    std::fs::write(&audio, source_bytes(src)).expect("write audio");
    let state = Arc::new(
        session::SessionState::new(
            vec![session::CastSource {
                cast_id: str_of(&src["castId"]).into(),
                title: str_of(&src["title"]).into(),
                path: audio,
                cover_path: None,
                alignment_path: None,
                duration_seconds: src["durationSeconds"].as_f64(),
            }],
            u64_of(&src["chunkSize"]),
            Arc::new(|_| {}),
        )
        .expect("session"),
    );
    let server = server::start(state.clone()).await.expect("server");
    Live {
        _dir: dir,
        state,
        server,
    }
}

impl Live {
    async fn pair(&self, secret: &str) -> RawResponse {
        let port = self.server.port();
        let headers = [("x-pairing-secret", secret)];
        request(
            port,
            self.server.fingerprint(),
            "POST",
            "/transfer/v1/pair",
            &headers,
        )
        .await
    }

    async fn get(&self, token: &str, path: &str, extra: &[(&str, &str)]) -> RawResponse {
        let bearer = format!("Bearer {token}");
        let mut headers = vec![("authorization", bearer.as_str())];
        headers.extend_from_slice(extra);
        request(
            self.server.port(),
            self.server.fingerprint(),
            "GET",
            path,
            &headers,
        )
        .await
    }
}

#[test]
fn m1_qr_payload_golden_is_exactly_what_the_sender_emits() {
    let m1 = load("m1_qr_payload.json");
    let hosts: Vec<String> = m1["h"]
        .as_array()
        .expect("h")
        .iter()
        .map(|h| str_of(h).to_string())
        .collect();
    let emitted = session::qr_payload(&hosts, str_of(&m1["f"]), str_of(&m1["s"]));
    let emitted: Value = serde_json::from_str(&emitted).expect("qr json");
    assert_eq!(emitted, m1, "QR payload drifted from the frozen golden");
    assert_eq!(m1["v"], u64::from(proto::PROTOCOL_VERSION));
}

#[tokio::test]
async fn m2_m3_m4_goldens_match_the_live_sender_byte_for_byte() {
    let src = load("source.json");
    let live = serve_fixture_cast(&src).await;
    let secret = live.state.mint_secret();
    let paired = live.pair(&secret).await;
    assert_eq!(paired.status, 200);
    let m2 = load("m2_pair_response.json");
    let golden: proto::PairResponse = serde_json::from_value(m2.clone()).expect("golden parses");
    let live_pair: proto::PairResponse = serde_json::from_slice(&paired.body).expect("live parses");
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        keys(&paired.json()),
        keys(&m2),
        "pair response keys drifted"
    );
    assert_eq!(
        live_pair.device_token.len(),
        golden.device_token.len(),
        "token length drifted"
    );
    assert!(live_pair.device_token.starts_with("tok_") && golden.device_token.starts_with("tok_"));

    let library = live
        .get(&live_pair.device_token, "/transfer/v1/library", &[])
        .await;
    assert_eq!(library.status, 200);
    assert_eq!(
        library.json(),
        load("m3_library.json"),
        "library index drifted"
    );

    let m4 = load("m4_manifest.json");
    let path = format!("/transfer/v1/casts/{}/manifest", str_of(&src["castId"]));
    let served = live.get(&live_pair.device_token, &path, &[]).await;
    assert_eq!(served.status, 200);
    assert_eq!(served.json(), m4, "served manifest drifted from the golden");
    let meta = live
        .state
        .ensure_meta(&live.state.casts[0])
        .await
        .expect("meta");
    let built =
        serde_json::to_value(
            live.state
                .manifest_for(&live.state.casts[0], &meta, None, None),
        )
        .unwrap();
    assert_eq!(built, m4, "manifest_for drifted from the golden");
    let parsed: proto::Manifest = serde_json::from_value(m4).expect("golden parses");
    parsed
        .validate()
        .expect("golden manifest passes the shared validator");
    live.server.stop();
}

#[tokio::test]
async fn m5_ranged_fetch_answers_the_golden_headers_and_bytes() {
    let src = load("source.json");
    let m5 = load("m5_file_range.json");
    let (req, resp) = (&m5["request"], &m5["response"]);
    let range = str_of(&req["range"]);
    let etag = str_of(&req["ifRange"]);
    let size = u64_of(&src["length"]);
    let (start, end) = range
        .strip_prefix("bytes=")
        .unwrap()
        .split_once('-')
        .unwrap();
    let (start, end): (u64, u64) = (start.parse().unwrap(), end.parse().unwrap());
    assert!(
        matches!(server::plan_range(Some(range), Some(etag), etag, size),
        server::RangePlan::Partial(s, e) if s == start && e == end)
    );
    assert!(str_of(&req["authorization"]).starts_with("Bearer tok_"));

    let live = serve_fixture_cast(&src).await;
    let secret = live.state.mint_secret();
    let token: proto::PairResponse =
        serde_json::from_slice(&live.pair(&secret).await.body).unwrap();
    let path = format!(
        "/transfer/v1/casts/{}/files/{}",
        str_of(&src["castId"]),
        str_of(&src["wirePath"])
    );
    let got = live
        .get(
            &token.device_token,
            &path,
            &[("range", range), ("if-range", etag)],
        )
        .await;
    assert_eq!(u64::from(got.status), u64_of(&resp["status"]));
    for (name, value) in resp["headers"].as_object().expect("headers") {
        assert_eq!(
            got.header(name),
            Some(str_of(value)),
            "header {name} drifted"
        );
    }
    let want = &source_bytes(&src)[start as usize..=end as usize];
    assert_eq!(got.body.len(), want.len());
    assert_eq!(
        Sha256::digest(&got.body),
        Sha256::digest(want),
        "range bytes differ"
    );
    live.server.stop();
}

#[tokio::test]
async fn neg_b1_wrong_fingerprint_never_reaches_the_protocol_layer() {
    let neg = load("neg_b1_fingerprint_mismatch.json");
    let der = hex::decode(str_of(&neg["certDerHex"])).expect("der hex");
    let actual = hex::encode(Sha256::digest(&der));
    assert_eq!(
        actual,
        str_of(&neg["actualFingerprint"]),
        "fingerprint derivation drifted"
    );
    assert_ne!(actual, str_of(&neg["pinnedFingerprint"]));
    let live = serve_fixture_cast(&load("source.json")).await;
    let fp = live.server.fingerprint();
    assert!(
        fp.len() == 64
            && fp
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let pinned_wrong = str_of(&neg["pinnedFingerprint"]);
    let refused =
        lan_pin_client::try_connect(live.server.port(), pinned_wrong, &[&rustls::version::TLS13])
            .await;
    assert!(
        refused.is_err(),
        "a mis-pinned phone must fail the handshake, never see HTTP"
    );
    live.server.stop();
}

#[tokio::test]
async fn neg_b2_replayed_or_expired_secret_gets_the_documented_403() {
    let neg = load("neg_b2_secret_replay.json");
    let live = serve_fixture_cast(&load("source.json")).await;
    let secret = live.state.mint_secret();
    let first_pair = live.pair(&secret).await;
    assert_eq!(first_pair.status, 200);
    let first_pair_json = first_pair.json();
    let replay = live.pair(&secret).await;
    assert_eq!(u64::from(replay.status), u64_of(&neg["status"]));
    assert_eq!(
        replay.json(),
        neg["body"],
        "403 body drifted from the golden"
    );
    let guess = live.pair(&"0".repeat(32)).await;
    assert_eq!(
        (guess.status, &guess.body),
        (replay.status, &replay.body),
        "replay and guess must be byte-identical"
    );
    assert!(
        !live.state.pair_locked(),
        "a replay must not count toward the lockout"
    );
    // C25: a minted secret lives PAIR_SECRET_TTL (300 s) on the desktop's
    // monotonic clock. Accepted at 299.9 s, refused at exactly 300.0 s with the
    // same byte-identical 403 as a replay or a guess; an expired secret never
    // counts toward the lockout; bearer tokens already issued keep working.
    let t0 = std::time::Instant::now();
    let offset = Arc::new(std::sync::Mutex::new(std::time::Duration::ZERO));
    let advance = |ms: u64| *offset.lock().unwrap() = std::time::Duration::from_millis(ms);
    let o = offset.clone();
    live.state
        .set_clock(Arc::new(move || t0 + *o.lock().unwrap()));
    let ttl_ms = session::PAIR_SECRET_TTL.as_millis() as u64;
    assert_eq!(ttl_ms, 300_000, "contract TTL is five minutes");

    let almost = live.state.mint_secret();
    advance(ttl_ms - 100);
    assert_eq!(live.pair(&almost).await.status, 200, "299.9 s: still valid");

    let expired = live.state.mint_secret(); // minted at t0 + 299.9 s
    advance(2 * ttl_ms - 100); // exactly 300.0 s after its mint
    let late = live.pair(&expired).await;
    assert_eq!(
        (late.status, &late.body),
        (replay.status, &replay.body),
        "expired secret must be byte-identical to replay/guess"
    );
    let late_replay = live.pair(&expired).await;
    assert_eq!(
        (late_replay.status, &late_replay.body),
        (replay.status, &replay.body),
        "replay after expiry is still the uniform 403"
    );
    assert!(
        !live.state.pair_locked(),
        "expired and replayed-after-expiry must not count toward the lockout"
    );
    let token = str_of(&first_pair_json["deviceToken"]).to_string();
    let library = live.get(&token, "/transfer/v1/library", &[]).await;
    assert_eq!(
        library.status, 200,
        "issued bearer token survives secret expiry"
    );
    live.server.stop();
}

#[test]
fn neg_b3_protocol_version_two_is_refused_by_the_shared_validator() {
    let neg = load("neg_b3_version_mismatch.json");
    let m: proto::Manifest = serde_json::from_value(neg["manifest"].clone()).expect("shape parses");
    let err = m.validate().expect_err("protocol 2 must be refused");
    assert!(err.contains("unsupported protocol version"), "{err}");
    assert_ne!(neg["qr"]["v"], u64::from(proto::PROTOCOL_VERSION));
    // The sender never reads a version from the phone; the only version checks
    // run on the phone (QR `v`, manifest `protocol`). This side pins that it
    // emits exactly PROTOCOL_VERSION in both places (m1 and m4 goldens).
    assert_eq!(
        load("m4_manifest.json")["protocol"],
        u64::from(proto::PROTOCOL_VERSION)
    );
}

#[tokio::test]
async fn neg_b4_stale_if_range_restarts_full_while_current_etag_resumes() {
    let src = load("source.json");
    let neg = load("neg_b4_interrupted_resume.json");
    let (etag, size) = (str_of(&src["etag"]), u64_of(&src["length"]));
    let (range, stale) = (str_of(&neg["expectedRange"]), str_of(&neg["staleIfRange"]));
    assert!(matches!(
        server::plan_range(Some(range), Some(stale), etag, size),
        server::RangePlan::Full
    ));
    assert!(matches!(
        server::plan_range(Some(range), Some(etag), etag, size),
        server::RangePlan::Partial(..)
    ));
    // The verified-chunk bitmap is phone-side state keyed on our etag and chunk size.
    assert_eq!(neg["sidecar"]["etag"], src["etag"]);
    assert_eq!(neg["sidecar"]["chunk_size"], src["chunkSize"]);
    let live = serve_fixture_cast(&src).await;
    let secret = live.state.mint_secret();
    let token: proto::PairResponse =
        serde_json::from_slice(&live.pair(&secret).await.body).unwrap();
    let path = format!(
        "/transfer/v1/casts/{}/files/{}",
        str_of(&src["castId"]),
        str_of(&src["wirePath"])
    );
    let restarted = live
        .get(
            &token.device_token,
            &path,
            &[("range", range), ("if-range", stale)],
        )
        .await;
    assert_eq!(
        u64::from(restarted.status),
        u64_of(&neg["staleAnswer"]),
        "stale If-Range must restart full"
    );
    assert_eq!(
        restarted.header("content-length"),
        Some(size.to_string().as_str())
    );
    assert_eq!(restarted.header("content-range"), None);
    assert_eq!(restarted.header("etag"), Some(etag));
    let resumed = live
        .get(
            &token.device_token,
            &path,
            &[("range", range), ("if-range", etag)],
        )
        .await;
    assert_eq!(resumed.status, 206, "current etag must resume partially");
    live.server.stop();
}

#[test]
fn neg_b5_corrupted_chunk_hash_passes_shape_validation_and_fails_content() {
    let src = load("source.json");
    let neg = load("neg_b5_integrity_failure.json");
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("book.opus");
    std::fs::write(&audio, source_bytes(&src)).unwrap();
    let meta = session::compute_file_meta(&audio, u64_of(&src["chunkSize"])).expect("meta");
    let idx = u64_of(&neg["chunkIndex"]) as usize;
    assert_eq!(
        meta.chunk_hashes[idx],
        str_of(&neg["actualHash"]),
        "chunk hashing drifted"
    );
    assert_ne!(meta.chunk_hashes[idx], str_of(&neg["declaredHash"]));
    assert_eq!(meta.sha256, str_of(&src["sha256"]));
    assert_eq!(meta.etag, str_of(&src["etag"]), "etag derivation drifted");
    let mut m4 = load("m4_manifest.json");
    m4["files"][0]["chunkHashes"][idx] = neg["declaredHash"].clone();
    let m: proto::Manifest = serde_json::from_value(m4).unwrap();
    // CURRENT behaviour: shape validation is hash-blind. Integrity is content-
    // level and enforced on the phone (chunk refetch, whole-file check at commit).
    m.validate()
        .expect("a wrong but well-formed hash is not a shape error");
}

#[tokio::test]
async fn neg_b6_reserving_the_same_cast_to_two_phones_is_identical_and_stateless() {
    let src = load("source.json");
    let (neg, m4) = (
        load("neg_b6_duplicate_delivery.json"),
        load("m4_manifest.json"),
    );
    assert_eq!(neg["receipt"]["contentHash"], m4["contentHash"]);
    assert_eq!(neg["receipt"]["contentHash"], m4["files"][0]["sha256"]);
    assert_eq!(neg["receipt"]["files"][0]["path"], m4["files"][0]["path"]);
    let live = serve_fixture_cast(&src).await;
    let path = format!("/transfer/v1/casts/{}/manifest", str_of(&src["castId"]));
    // Two phones, two pairings, one cast: the sender keeps no per-cast delivery
    // state, so both receive the golden manifest and the phone alone deduplicates.
    let mut bodies = Vec::new();
    for _ in 0..2 {
        let secret = live.state.mint_secret();
        let token: proto::PairResponse =
            serde_json::from_slice(&live.pair(&secret).await.body).unwrap();
        let served = live.get(&token.device_token, &path, &[]).await;
        assert_eq!(served.status, 200);
        bodies.push(served.json());
    }
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(bodies[0], m4);
    live.server.stop();
}
