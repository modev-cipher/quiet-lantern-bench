//! Recorded wire-protocol fixture infrastructure (VA5 unit 1).
//!
//! Every ByokChannel adapter is held to recorded protocol fixtures replayed in
//! CI (TASKS VA5; VOICE_AGENT §3: "recorded WSS protocol fixtures replayed in
//! CI"). This module is the shared rig those replays run on:
//!
//! - [`WireServer`] — a localhost WebSocket fixture server. The adapter under
//!   test connects to `ws://127.0.0.1:<port>` exactly as it would connect to
//!   its provider; the test drives the provider side through a [`WireHandle`]
//!   and reads back every frame and every upgrade handshake the adapter sent.
//!   CI never opens a non-loopback socket (VOICE_AGENT §10).
//! - [`WireFixture`] — the on-disk recorded-fixture format
//!   (`tests/fixtures/voice/<name>.json`). A fixture carries its provider, the
//!   official doc URL + access date it was recorded against (AGENTS.md /
//!   VOICE_AGENT §3.2 implementation rule), the channel protocol version it was
//!   cut for, and a step script.
//! - [`replay`] — the executor: `send_*` steps inject provider frames,
//!   `expect_*` steps assert on what the adapter sent (frames and handshake),
//!   with a hard timeout so a silent adapter fails loudly instead of hanging CI.
//!
//! The handshake record is first-class because two roster fixtures are *about*
//! the handshake: `auth-hygiene-no-query-param-creds` (credentials travel in
//! headers, never in the connect URL) and `cartesia-version-pin` (the dated
//! version header is present on upgrade).

#![cfg(test)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};
use tokio::time::{timeout, Duration};
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as UpgradeRequest, Response as UpgradeResponse,
};
use tokio_tungstenite::tungstenite::Message;

/// How long a replay step may wait before the fixture fails loudly.
const STEP_BUDGET: Duration = Duration::from_secs(5);

/// One WebSocket frame, direction-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireFrame {
    Text(String),
    Binary(Vec<u8>),
}

/// Everything the adapter revealed at WS upgrade time: the request target
/// (path + query) and the header list. The auth-hygiene and version-pin
/// fixtures assert on this record.
#[derive(Debug, Clone)]
pub struct HandshakeRecord {
    /// Path + query exactly as requested, e.g. `/v1/listen?model=nova-3`.
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl HandshakeRecord {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct Shared {
    handshakes: Mutex<Vec<HandshakeRecord>>,
    /// Inbound frames per connection, in arrival order.
    received: Mutex<Vec<Vec<WireFrame>>>,
    pings: Mutex<usize>,
    /// Sender into the live (latest) connection's outbound queue.
    outbound: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    connected: Notify,
    stall: Notify,
    stalled: Mutex<Vec<StalledReader>>,
}

type StalledReader =
    futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>;

/// Test-side handle to the fixture server: inject provider frames, read what
/// the adapter sent, inspect handshakes.
#[derive(Clone)]
pub struct WireHandle {
    shared: Arc<Shared>,
    inbound: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(usize, WireFrame)>>>,
    port: u16,
}

impl WireHandle {
    /// `ws://127.0.0.1:<port><path>` — what the adapter under test connects to.
    pub fn url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:{}{}", self.port, path)
    }

    /// Waits until the adapter has opened connection number `n` (1-based) and
    /// returns its handshake record.
    pub async fn wait_for_connection(&self, n: usize) -> HandshakeRecord {
        timeout(STEP_BUDGET, async {
            loop {
                if let Some(record) = self.shared.handshakes.lock().unwrap().get(n - 1) {
                    return record.clone();
                }
                self.shared.connected.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fixture server: connection {n} never arrived"))
    }

    /// Sends a provider frame into the live connection.
    pub fn send(&self, frame: WireFrame) {
        let message = match frame {
            WireFrame::Text(text) => Message::text(text),
            WireFrame::Binary(bytes) => Message::binary(bytes),
        };
        let guard = self.shared.outbound.lock().unwrap();
        let sender = guard
            .as_ref()
            .expect("fixture server: no live connection to send into");
        sender
            .send(message)
            .expect("fixture server: live connection writer is gone");
    }

    /// Closes the live connection from the provider side (server-initiated
    /// close, e.g. a provider dropping the socket).
    pub fn close_current(&self) {
        let guard = self.shared.outbound.lock().unwrap();
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(Message::Close(None));
        }
    }

    /// Closes the live connection with an explicit close frame, the way the
    /// real provider refuses a bad key (observed live: code 1007 with reason
    /// "API key not valid. Please pass a valid API key.").
    pub fn close_current_with(&self, code: u16, reason: &str) {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
        let guard = self.shared.outbound.lock().unwrap();
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: reason.to_owned().into(),
            })));
        }
    }

    /// Next frame the adapter sends, in order across connections. Panics after
    /// [`STEP_BUDGET`] — a silent adapter is a failed fixture, not a hung test.
    pub async fn next_from_adapter(&self) -> WireFrame {
        self.next_from_adapter_indexed().await.1
    }

    /// Like [`Self::next_from_adapter`], but also says which connection
    /// (0-based) carried the frame — conformance rigs group payloads into
    /// renewal epochs by connection.
    pub async fn next_from_adapter_indexed(&self) -> (usize, WireFrame) {
        let mut inbound = self.inbound.lock().await;
        timeout(STEP_BUDGET, inbound.recv())
            .await
            .expect("fixture server: adapter sent nothing within the step budget")
            .expect("fixture server: inbound queue closed")
    }

    /// Non-blocking variant: `Some((connection, frame))` if one is queued.
    pub async fn try_next_from_adapter(&self) -> Option<(usize, WireFrame)> {
        let mut inbound = self.inbound.lock().await;
        inbound.try_recv().ok()
    }

    /// Every frame received so far, grouped by connection, in order.
    pub fn received_by_connection(&self) -> Vec<Vec<WireFrame>> {
        self.shared.received.lock().unwrap().clone()
    }

    pub fn handshakes(&self) -> Vec<HandshakeRecord> {
        self.shared.handshakes.lock().unwrap().clone()
    }

    pub fn pings_received(&self) -> usize {
        *self.shared.pings.lock().unwrap()
    }

    pub fn stall_current_reads(&self) {
        self.shared.stall.notify_one();
    }
}

/// A localhost WebSocket fixture server. Accepts sequential connections (a
/// renewal recycle is a second connection on the same server) and records
/// everything the adapter reveals.
pub struct WireServer;

impl WireServer {
    /// Binds on an OS-assigned loopback port and returns the test-side handle.
    pub async fn start() -> WireHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture server: bind loopback");
        let port = listener.local_addr().expect("fixture server: addr").port();
        let shared = Arc::new(Shared::default());
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

        let accept_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handshake_shared = Arc::clone(&accept_shared);
                // The signature is tungstenite's `Callback` trait: the Err
                // type is theirs, so its size is not ours to box.
                #[allow(clippy::result_large_err)]
                let callback = move |request: &UpgradeRequest,
                                     response: UpgradeResponse|
                      -> Result<UpgradeResponse, ErrorResponse> {
                    let record = HandshakeRecord {
                        target: request
                            .uri()
                            .path_and_query()
                            .map(|pq| pq.as_str().to_string())
                            .unwrap_or_default(),
                        headers: request
                            .headers()
                            .iter()
                            .map(|(field, value)| {
                                (
                                    field.as_str().to_string(),
                                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                                )
                            })
                            .collect(),
                    };
                    handshake_shared.handshakes.lock().unwrap().push(record);
                    Ok(response)
                };
                let Ok(socket) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
                    continue;
                };
                accept_shared.received.lock().unwrap().push(Vec::new());
                let connection_idx = accept_shared.received.lock().unwrap().len() - 1;
                let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Message>();
                *accept_shared.outbound.lock().unwrap() = Some(outbound_tx);
                accept_shared.connected.notify_waiters();

                let (mut writer, mut reader) = socket.split();
                let write_task = tokio::spawn(async move {
                    while let Some(message) = outbound_rx.recv().await {
                        let closing = matches!(message, Message::Close(_));
                        if writer.send(message).await.is_err() || closing {
                            return;
                        }
                    }
                });

                let read_shared = Arc::clone(&accept_shared);
                let read_inbound = inbound_tx.clone();
                loop {
                    let next = tokio::select! {
                        next = reader.next() => Some(next),
                        _ = read_shared.stall.notified() => None,
                    };
                    let Some(next) = next else {
                        read_shared.stalled.lock().unwrap().push(reader);
                        break;
                    };
                    let Some(Ok(message)) = next else {
                        break;
                    };
                    let frame = match message {
                        Message::Text(text) => WireFrame::Text(text.to_string()),
                        Message::Binary(bytes) => WireFrame::Binary(bytes.to_vec()),
                        Message::Ping(_) => {
                            *read_shared.pings.lock().unwrap() += 1;
                            continue;
                        }
                        Message::Close(_) => break,
                        _ => continue,
                    };
                    read_shared.received.lock().unwrap()[connection_idx].push(frame.clone());
                    let _ = read_inbound.send((connection_idx, frame));
                }
                write_task.abort();
            }
        });

        WireHandle {
            shared,
            inbound: Arc::new(tokio::sync::Mutex::new(inbound_rx)),
            port,
        }
    }
}

/// The on-disk recorded-fixture format. Provenance fields are mandatory: a
/// fixture that cannot say which official doc page it was recorded against,
/// and when, is not a recorded fixture (VOICE_AGENT §3.2 implementation rule).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireFixture {
    pub name: String,
    pub provider: String,
    pub doc_url: String,
    pub doc_accessed: String,
    pub channel_protocol_version: u32,
    pub steps: Vec<WireStep>,
}

/// One replay step. `send_*` plays the provider; `expect_*` asserts on the
/// adapter. Matching is needle-based (`contains` / `absent`) so fixtures pin
/// the protocol-relevant parts of a frame without freezing incidental ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WireStep {
    /// The adapter's next frame must be text containing every needle in
    /// `contains` and none in `absent`.
    ExpectText {
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
    },
    /// The adapter's next frame must be binary of at least `min_bytes`; when
    /// `payload_base64` is set, the frame's bytes must equal it exactly — the
    /// binary counterpart of a text needle, for adapters whose hot path is raw
    /// binary frames.
    ExpectBinary {
        min_bytes: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload_base64: Option<String>,
    },
    /// Assert on the upgrade handshake of connection `connection` (1-based):
    /// the request target contains/lacks the given needles, and every named
    /// header is present.
    ExpectHandshake {
        connection: usize,
        #[serde(default)]
        target_contains: Vec<String>,
        #[serde(default)]
        target_absent: Vec<String>,
        #[serde(default)]
        headers_present: Vec<String>,
    },
    /// Inject a provider text frame (recorded provider output).
    SendText { text: String },
    /// Inject a provider binary frame, base64-encoded in the fixture file.
    SendBinary { base64: String },
    /// Provider closes the socket.
    ServerClose,
}

/// Directory the recorded fixtures live in.
pub fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("voice")
}

/// Loads `tests/fixtures/voice/<name>.json`.
pub fn load_fixture(name: &str) -> WireFixture {
    let path = fixture_dir().join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("fixture {name}: read {path:?}: {err}"));
    let fixture: WireFixture = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("fixture {name}: parse {path:?}: {err}"));
    assert_eq!(
        fixture.name, name,
        "fixture file {path:?} carries the wrong name"
    );
    assert!(
        !fixture.doc_url.is_empty() && !fixture.doc_accessed.is_empty(),
        "fixture {name}: recorded fixtures must carry their official-doc provenance"
    );
    fixture
}

/// Replays a fixture's provider side against the adapter connected to
/// `handle`. Panics with the fixture name and step index on any mismatch.
pub async fn replay(fixture: &WireFixture, handle: &WireHandle) {
    assert_eq!(
        fixture.channel_protocol_version,
        super::CHANNEL_PROTOCOL_VERSION,
        "fixture {}: recorded for a different channel protocol version — re-record it",
        fixture.name
    );
    for (idx, step) in fixture.steps.iter().enumerate() {
        run_step(step, handle)
            .await
            .unwrap_or_else(|err| panic!("fixture {} step {idx} ({step:?}): {err}", fixture.name));
    }
}

async fn run_step(step: &WireStep, handle: &WireHandle) -> Result<(), String> {
    match step {
        WireStep::ExpectText { contains, absent } => {
            let frame = handle.next_from_adapter().await;
            let WireFrame::Text(text) = frame else {
                return Err(format!("expected a text frame, adapter sent {frame:?}"));
            };
            for needle in contains {
                if !text.contains(needle.as_str()) {
                    return Err(format!("adapter text frame lacks `{needle}`: {text}"));
                }
            }
            for needle in absent {
                if text.contains(needle.as_str()) {
                    return Err(format!(
                        "adapter text frame must not contain `{needle}`: {text}"
                    ));
                }
            }
            Ok(())
        }
        WireStep::ExpectBinary {
            min_bytes,
            payload_base64,
        } => {
            let frame = handle.next_from_adapter().await;
            let WireFrame::Binary(bytes) = frame else {
                return Err(format!("expected a binary frame, adapter sent {frame:?}"));
            };
            if bytes.len() < *min_bytes {
                return Err(format!(
                    "binary frame carries {} bytes, expected at least {min_bytes}",
                    bytes.len()
                ));
            }
            if let Some(pinned) = payload_base64 {
                use base64::Engine as _;
                let expected = base64::engine::general_purpose::STANDARD
                    .decode(pinned)
                    .map_err(|err| format!("fixture pinned payload is not base64: {err}"))?;
                if bytes != expected {
                    return Err(format!(
                        "binary frame differs from the pinned payload: got {} bytes {bytes:02x?}, \
                         expected {} bytes {expected:02x?}",
                        bytes.len(),
                        expected.len()
                    ));
                }
            }
            Ok(())
        }
        WireStep::ExpectHandshake {
            connection,
            target_contains,
            target_absent,
            headers_present,
        } => {
            let record = handle.wait_for_connection(*connection).await;
            for needle in target_contains {
                if !record.target.contains(needle.as_str()) {
                    return Err(format!(
                        "handshake target lacks `{needle}`: {}",
                        record.target
                    ));
                }
            }
            for needle in target_absent {
                if record.target.contains(needle.as_str()) {
                    return Err(format!(
                        "handshake target must not contain `{needle}`: {}",
                        record.target
                    ));
                }
            }
            for name in headers_present {
                if record.header(name).is_none() {
                    return Err(format!("handshake lacks header `{name}`"));
                }
            }
            Ok(())
        }
        WireStep::SendText { text } => {
            handle.send(WireFrame::Text(text.clone()));
            Ok(())
        }
        WireStep::SendBinary { base64 } => {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(base64)
                .map_err(|err| format!("fixture binary payload is not base64: {err}"))?;
            handle.send(WireFrame::Binary(bytes));
            Ok(())
        }
        WireStep::ServerClose => {
            handle.close_current();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connect(
        handle: &WireHandle,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let (socket, _) = tokio_tungstenite::connect_async(handle.url("/session?model=fixture"))
            .await
            .expect("client connect");
        socket
    }

    #[tokio::test]
    async fn the_server_records_handshakes_and_frames_in_order() {
        let handle = WireServer::start().await;
        let mut client = connect(&handle).await;

        let record = handle.wait_for_connection(1).await;
        assert_eq!(record.target, "/session?model=fixture");
        assert!(
            record.header("host").is_some(),
            "the upgrade request's headers are captured"
        );

        client.send(Message::text("{\"hello\":1}")).await.unwrap();
        client.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();

        assert_eq!(
            handle.next_from_adapter().await,
            WireFrame::Text("{\"hello\":1}".to_string())
        );
        assert_eq!(
            handle.next_from_adapter().await,
            WireFrame::Binary(vec![1, 2, 3])
        );
        assert_eq!(
            handle.received_by_connection(),
            vec![vec![
                WireFrame::Text("{\"hello\":1}".to_string()),
                WireFrame::Binary(vec![1, 2, 3]),
            ]]
        );
    }

    #[tokio::test]
    async fn provider_frames_reach_the_adapter_and_a_server_close_ends_the_stream() {
        let handle = WireServer::start().await;
        let mut client = connect(&handle).await;
        handle.wait_for_connection(1).await;

        handle.send(WireFrame::Text("{\"event\":\"ready\"}".to_string()));
        handle.send(WireFrame::Binary(vec![9u8; 4]));
        let first = client.next().await.unwrap().unwrap();
        assert_eq!(first, Message::text("{\"event\":\"ready\"}"));
        let second = client.next().await.unwrap().unwrap();
        assert_eq!(second, Message::binary(vec![9u8; 4]));

        handle.close_current();
        loop {
            match client.next().await {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => continue,
                Some(Err(_)) => break,
            }
        }
    }

    #[tokio::test]
    async fn a_second_connection_gets_its_own_handshake_and_frame_group() {
        let handle = WireServer::start().await;
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "nothing is queued before the adapter sends"
        );
        let mut first = connect(&handle).await;
        handle.wait_for_connection(1).await;
        first.send(Message::text("first")).await.unwrap();
        assert_eq!(
            handle.next_from_adapter_indexed().await,
            (0, WireFrame::Text("first".to_string())),
            "frames carry the 0-based connection that sent them"
        );
        first.close(None).await.unwrap();

        let mut second = connect(&handle).await;
        handle.wait_for_connection(2).await;
        second.send(Message::text("second")).await.unwrap();
        assert_eq!(
            handle.next_from_adapter_indexed().await,
            (1, WireFrame::Text("second".to_string()))
        );
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the queue is drained"
        );

        let by_connection = handle.received_by_connection();
        assert_eq!(by_connection.len(), 2, "one frame group per connection");
        assert_eq!(by_connection[0], vec![WireFrame::Text("first".to_string())]);
        assert_eq!(
            by_connection[1],
            vec![WireFrame::Text("second".to_string())]
        );
        assert_eq!(handle.handshakes().len(), 2);

        handle.send(WireFrame::Text("routed".to_string()));
        let routed = second.next().await.unwrap().unwrap();
        assert_eq!(
            routed,
            Message::text("routed"),
            "provider sends route to the latest connection"
        );
    }

    fn fixture_with_steps(steps: Vec<WireStep>) -> WireFixture {
        WireFixture {
            name: "inline".to_string(),
            provider: "fixture-self-test".to_string(),
            doc_url: "https://example.invalid/self-test".to_string(),
            doc_accessed: "2026-08-20".to_string(),
            channel_protocol_version: crate::voice::channel::CHANNEL_PROTOCOL_VERSION,
            steps,
        }
    }

    #[tokio::test]
    async fn replay_walks_expect_and_send_steps_against_a_live_peer() {
        let handle = WireServer::start().await;
        let mut client = connect(&handle).await;
        let fixture = fixture_with_steps(vec![
            WireStep::ExpectHandshake {
                connection: 1,
                target_contains: vec!["model=fixture".to_string()],
                target_absent: vec!["key=".to_string()],
                headers_present: vec!["host".to_string()],
            },
            WireStep::ExpectText {
                contains: vec!["\"setup\"".to_string()],
                absent: vec!["api_key".to_string()],
            },
            WireStep::SendText {
                text: "{\"ack\":true}".to_string(),
            },
        ]);

        let driver = tokio::spawn(async move {
            client.send(Message::text("{\"setup\":{}}")).await.unwrap();
            let reply = client.next().await.unwrap().unwrap();
            assert_eq!(reply, Message::text("{\"ack\":true}"));
        });

        replay(&fixture, &handle).await;
        driver.await.unwrap();
    }

    #[tokio::test]
    async fn replay_reds_on_a_frame_that_lacks_the_pinned_needle() {
        let handle = WireServer::start().await;
        let mut client = connect(&handle).await;
        handle.wait_for_connection(1).await;
        let fixture = fixture_with_steps(vec![WireStep::ExpectText {
            contains: vec!["\"setup\"".to_string()],
            absent: vec![],
        }]);

        client
            .send(Message::text("{\"wrong\":true}"))
            .await
            .unwrap();
        let outcome = tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(replay(&fixture, &handle));
            futures_util::FutureExt::catch_unwind(result).await
        })
        .await
        .unwrap();
        assert!(
            outcome.is_err(),
            "a frame missing the pinned needle must red the replay"
        );
    }

    #[tokio::test]
    async fn replay_refuses_a_fixture_cut_for_another_protocol_version() {
        let handle = WireServer::start().await;
        let _client = connect(&handle).await;
        let mut fixture = fixture_with_steps(vec![]);
        fixture.channel_protocol_version = crate::voice::channel::CHANNEL_PROTOCOL_VERSION + 1;

        let outcome = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(replay(
            &fixture, &handle,
        )))
        .await;
        assert!(
            outcome.is_err(),
            "a stale-version fixture must be re-recorded, not silently replayed"
        );
    }

    #[test]
    fn the_fixture_format_round_trips_and_rejects_unknown_fields() {
        let fixture = fixture_with_steps(vec![
            WireStep::ExpectBinary {
                min_bytes: 320,
                payload_base64: None,
            },
            WireStep::SendBinary {
                base64: "AAEC".to_string(),
            },
            WireStep::ServerClose,
        ]);
        let raw = serde_json::to_string_pretty(&fixture).unwrap();
        let back: WireFixture = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.steps.len(), 3);

        let unknown = raw.replace("\"provider\"", "\"provider_x\"");
        assert!(
            serde_json::from_str::<WireFixture>(&unknown).is_err(),
            "unknown fields red the parse: a typo in a fixture must not silently no-op"
        );
    }
}
