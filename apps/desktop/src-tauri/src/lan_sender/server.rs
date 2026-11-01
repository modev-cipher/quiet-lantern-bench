use std::io::SeekFrom;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use http_body_util::{BodyExt, Empty, Full, LengthLimitError, Limited, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::header::{
    AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, IF_RANGE, RANGE,
};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::pki_types::PrivateKeyDer;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use super::session::{self, CastEntry, MetaError, PairOutcome, SessionState};

const STREAM_BUF_BYTES: usize = 256 * 1024;

/// How long a stopped sender keeps flushing a response the phone is still
/// reading before giving up on it. Owner-tunable: too short truncates the
/// final range on slow Wi-Fi (the phone then re-fetches it on resume); too
/// long keeps a zombie flush task alive after stop (the listening port itself
/// is released immediately either way).
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Hard ceiling on connections the listener holds at once. The phone fetches
/// one file at a time on one connection, and the adversarial storm pin fires 32
/// concurrent ranges, so 64 leaves headroom for both while still bounding what
/// an unauthenticated LAN peer can park (task + socket + hyper read buffer).
pub const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// Backoff applied when `accept()` fails in a way that will repeat immediately.
const ACCEPT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// Consecutive accept failures after which we back off even without a
/// classifiable errno (the portable fallback; Windows reaches the same state
/// via `WSAEMFILE`/`WSAENOBUFS`, which this crate cannot name from within the
/// enabled `windows-sys` features).
const ACCEPT_FAILURE_BURST: u32 = 5;

/// `Some(backoff)` for an accept error that leaves the queued connection in
/// place, so an immediate retry burns a core until a descriptor frees.
/// Transient aborts (a peer that vanished mid-accept) return `None` and are
/// retried immediately, so normal pairing latency is untouched.
fn accept_error_backoff(error: &std::io::Error) -> Option<std::time::Duration> {
    #[cfg(unix)]
    {
        if matches!(
            error.raw_os_error(),
            Some(code)
                if code == libc::EMFILE
                    || code == libc::ENFILE
                    || code == libc::ENOBUFS
                    || code == libc::ENOMEM
        ) {
            return Some(ACCEPT_BACKOFF);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = error;
    }
    None
}

pub struct ServerHandle {
    port: u16,
    fingerprint: String,
    shutdown: watch::Sender<bool>,
}

impl ServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn stop(&self) {
        tracing::info!(port = self.port, "lan sender stopping");
        let _ = self.shutdown.send(true);
    }
}

pub fn lan_ips() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .map(|interfaces| {
            interfaces
                .into_iter()
                .filter(|iface| !iface.is_loopback())
                .map(|iface| iface.ip())
                .collect()
        })
        .unwrap_or_default()
}

fn start_failed(stage: &'static str, e: impl std::fmt::Display) -> String {
    let error = e.to_string();
    tracing::error!(stage, error, "lan sender startup failed");
    error
}

pub async fn start(session: Arc<SessionState>) -> Result<ServerHandle, String> {
    let key = rcgen::KeyPair::generate().map_err(|e| start_failed("keypair", e))?;
    let cert = rcgen::CertificateParams::new(vec!["txtcast.local".to_string()])
        .map_err(|e| start_failed("cert_params", e))?
        .self_signed(&key)
        .map_err(|e| start_failed("cert_sign", e))?;
    let cert_der = cert.der().clone();
    let fingerprint = hex::encode(Sha256::digest(cert_der.as_ref()));
    let key_der = PrivateKeyDer::Pkcs8(key.serialize_der().into());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| start_failed("tls_versions", e))?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|e| start_failed("tls_cert", e))?;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));

    let v4 = bind_v4().await?;
    let port = v4
        .local_addr()
        .map_err(|e| start_failed("local_addr", e))?
        .port();
    let v6 = bind_v6_only(port);
    let dual_stack = v6.is_some();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // Per-listener, deliberately: a process-global cap would let one listener's
    // flood throttle another's, and the test suite exercises two listeners in
    // the same process (connection storms included).
    let conns = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    spawn_accept_loop(
        v4,
        acceptor.clone(),
        session.clone(),
        shutdown_rx.clone(),
        conns.clone(),
    );
    if let Some(listener) = v6 {
        spawn_accept_loop(listener, acceptor, session, shutdown_rx, conns);
    }
    tracing::info!(
        port,
        ephemeral_fallback = port != session::DEFAULT_PORT,
        dual_stack,
        fingerprint_len = fingerprint.len(),
        "lan sender listening"
    );
    Ok(ServerHandle {
        port,
        fingerprint,
        shutdown: shutdown_tx,
    })
}

async fn bind_v4() -> Result<TcpListener, String> {
    let preferred = SocketAddr::from(([0, 0, 0, 0], session::DEFAULT_PORT));
    match TcpListener::bind(preferred).await {
        Ok(listener) => return Ok(listener),
        Err(e) => tracing::info!(
            preferred_port = session::DEFAULT_PORT,
            error = %e,
            "lan sender preferred port unavailable, falling back to an ephemeral port"
        ),
    }
    TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
        .await
        .map_err(|e| start_failed("bind_v4", e))
}

fn v6_step<T, E: std::fmt::Display>(stage: &'static str, result: Result<T, E>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::warn!(
                stage,
                error = %e,
                "lan sender ipv6 listener unavailable, continuing v4-only"
            );
            None
        }
    }
}

fn bind_v6_only(port: u16) -> Option<TcpListener> {
    let socket = v6_step(
        "socket",
        socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        ),
    )?;
    v6_step("only_v6", socket.set_only_v6(true))?;
    v6_step("nonblocking", socket.set_nonblocking(true))?;
    let addr = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
    v6_step("bind", socket.bind(&addr.into()))?;
    v6_step("listen", socket.listen(64))?;
    v6_step("into_tokio", TcpListener::from_std(socket.into()))
}

fn spawn_accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    session: Arc<SessionState>,
    mut shutdown: watch::Receiver<bool>,
    conns: Arc<tokio::sync::Semaphore>,
) {
    tokio::spawn(async move {
        let mut accept_failures: u32 = 0;
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(pair) => {
                            accept_failures = 0;
                            pair
                        }
                        Err(e) => {
                            accept_failures += 1;
                            tracing::warn!(
                                error = %e,
                                consecutive = accept_failures,
                                "lan sender accept failed"
                            );
                            if accept_error_backoff(&e).is_some()
                                || accept_failures >= ACCEPT_FAILURE_BURST
                            {
                                tokio::time::sleep(ACCEPT_BACKOFF).await;
                            }
                            continue;
                        }
                    };
                    let family = if peer.ip().is_ipv4() { "v4" } else { "v6" };
                    if !session::peer_allowed(&peer.ip()) {
                        tracing::warn!(family, "lan sender refused a non-local peer");
                        continue;
                    }
                    let conn = session.accepted_connections.fetch_add(1, Ordering::Relaxed) + 1;
                    let span = tracing::info_span!("lan_conn", conn, family);
                    // Shed, never queue: awaiting a permit here would park the
                    // accept loop, and `stop()` is another branch of this same
                    // select!. The dropped stream sends an immediate reset, so
                    // the peer fails over to its next host candidate.
                    let Ok(permit) = conns.clone().try_acquire_owned() else {
                        session.shed_connections.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            conn,
                            family,
                            max = MAX_CONCURRENT_CONNECTIONS,
                            "lan sender shed a connection: listener at capacity"
                        );
                        continue;
                    };
                    let acceptor = acceptor.clone();
                    let session = session.clone();
                    let mut shutdown = shutdown.clone();
                    tokio::spawn(tracing::Instrument::instrument(async move {
                        // Held for the connection's lifetime: a clean close, a
                        // deadline, a transport error and a panic all return the
                        // permit when this task ends.
                        let _permit = permit;
                        let handshake_deadline = session.handshake_deadline;
                        let header_read_timeout = session.header_read_timeout;
                        let handshake_started = std::time::Instant::now();
                        let handshake = tokio::select! {
                            _ = shutdown.changed() => return,
                            outcome = tokio::time::timeout(
                                handshake_deadline,
                                acceptor.accept(stream),
                            ) => match outcome {
                                Ok(result) => result,
                                Err(_) => {
                                    tracing::warn!(
                                        ms = handshake_started.elapsed().as_millis() as u64,
                                        "lan sender dropped a peer that stalled in the TLS handshake"
                                    );
                                    return;
                                }
                            },
                        };
                        let tls_stream = match handshake {
                            Ok(tls_stream) => tls_stream,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    ms = handshake_started.elapsed().as_millis() as u64,
                                    "lan sender tls handshake failed, peer never reached the protocol layer"
                                );
                                return;
                            }
                        };
                        tracing::debug!(
                            ms = handshake_started.elapsed().as_millis() as u64,
                            "lan sender tls session established"
                        );
                        let io = TokioIo::new(tls_stream);
                        let service = service_fn(move |request| {
                            let session = session.clone();
                            async move { handle_request(&session, request).await }
                        });
                        let connection = hyper::server::conn::http1::Builder::new()
                            // Both lines belong together: hyper documents that
                            // configuring `header_read_timeout` without a `Timer`
                            // **panics** on connection setup.
                            .timer(TokioTimer::new())
                            .header_read_timeout(header_read_timeout)
                            .serve_connection(io, service);
                        let mut connection = std::pin::pin!(connection);
                        tokio::select! {
                            _ = shutdown.changed() => {
                                // A stop must not tear down a response the phone is
                                // still reading: dropping the future here closes the
                                // TLS stream without close_notify and the phone loses
                                // the range it was mid-download on. Disable keep-alive
                                // and drain the in-flight exchange instead.
                                connection.as_mut().graceful_shutdown();
                                match tokio::time::timeout(DRAIN_GRACE, connection.as_mut()).await {
                                    Ok(Ok(())) => tracing::debug!(
                                        "lan sender drained an in-flight connection at shutdown"
                                    ),
                                    Ok(Err(e)) => tracing::debug!(
                                        error = %e,
                                        "lan sender connection ended with a transport error during the shutdown drain"
                                    ),
                                    Err(_) => tracing::warn!(
                                        grace_secs = DRAIN_GRACE.as_secs(),
                                        "lan sender abandoned an in-flight connection after the drain grace period"
                                    ),
                                }
                            }
                            outcome = connection.as_mut() => {
                                if let Err(e) = outcome {
                                    tracing::debug!(error = %e, "lan sender connection ended with a transport error");
                                }
                            }
                        }
                    }, span));
                }
            }
        }
    });
}

#[cfg(all(test, unix))]
mod accept_backoff_tests {
    use super::accept_error_backoff;

    #[test]
    fn descriptor_exhaustion_backs_off_and_transient_aborts_do_not() {
        use std::io::Error;
        // Sticky: the queued connection is not consumed, so an immediate retry
        // would spin a core until a descriptor frees.
        assert!(accept_error_backoff(&Error::from_raw_os_error(libc::EMFILE)).is_some());
        assert!(accept_error_backoff(&Error::from_raw_os_error(libc::ENFILE)).is_some());
        assert!(accept_error_backoff(&Error::from_raw_os_error(libc::ENOBUFS)).is_some());
        assert!(accept_error_backoff(&Error::from_raw_os_error(libc::ENOMEM)).is_some());
        // Transient: a peer that vanished mid-accept must not add latency.
        assert!(accept_error_backoff(&Error::from_raw_os_error(libc::ECONNABORTED)).is_none());
        assert!(accept_error_backoff(&Error::from(std::io::ErrorKind::WouldBlock)).is_none());
    }
}

type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>;

fn status_response(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_LENGTH, 0)
        .header(CONNECTION, "close")
        .body(Empty::new().map_err(std::io::Error::other).boxed_unsync())
        .expect("static response")
}

fn json_response(status: StatusCode, value: &impl serde::Serialize) -> Response<Body> {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_LENGTH, bytes.len())
        .header(CONNECTION, "close")
        .body(
            Full::new(Bytes::from(bytes))
                .map_err(std::io::Error::other)
                .boxed_unsync(),
        )
        .expect("static response")
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryCast {
    cast_id: String,
    title: String,
    duration_seconds: Option<f64>,
    total_size: u64,
}

#[derive(serde::Serialize)]
struct Library {
    casts: Vec<LibraryCast>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PairOk {
    device_token: String,
}

#[derive(serde::Serialize)]
struct WireError {
    error: &'static str,
}

fn transfer_id(request: &Request<hyper::body::Incoming>) -> String {
    request
        .headers()
        .get("x-transfer-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .unwrap_or("-")
        .to_string()
}

async fn handle_request(
    session: &Arc<SessionState>,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, std::convert::Infallible> {
    let xfer = transfer_id(&request);
    let path = request.uri().path().to_string();
    let Some(rest) = path.strip_prefix("/transfer/v1") else {
        tracing::debug!(
            xfer,
            path_len = path.len(),
            "lan sender refused a non-protocol path with 404"
        );
        return Ok(status_response(StatusCode::NOT_FOUND));
    };
    if request.method() == Method::POST && rest == "/pair" {
        return Ok(handle_pair(session, &request, &xfer));
    }
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let bearer = auth_header.and_then(|value| value.strip_prefix("Bearer "));
    let authorized = bearer.is_some_and(|token| session.token_valid(token));
    if !authorized {
        let cause = match (auth_header, bearer) {
            (None, _) => "missing_authorization_header",
            (Some(_), None) => "not_a_bearer_header",
            (Some(_), Some(token)) => {
                if token.len() == 36 && token.starts_with("tok_") {
                    "unknown_or_revoked_token"
                } else {
                    "token_shape_invalid"
                }
            }
        };
        tracing::debug!(
            xfer,
            cause,
            token_len = bearer.map(str::len).unwrap_or(0),
            "lan sender request unauthorized, peer sees the uniform 401"
        );
        return Ok(status_response(StatusCode::UNAUTHORIZED));
    }
    if request.method() == Method::POST && rest == "/receipt" {
        return Ok(handle_receipt(session, request, &xfer).await);
    }
    if request.method() != Method::GET {
        tracing::debug!(
            xfer,
            method = %request.method(),
            "lan sender refused a non-GET method with 404"
        );
        return Ok(status_response(StatusCode::NOT_FOUND));
    }
    if rest == "/library" {
        tracing::debug!(
            xfer,
            casts = session.casts.len(),
            "lan sender served the library index"
        );
        return Ok(handle_library(session));
    }
    if let Some(tail) = rest.strip_prefix("/casts/") {
        let mut segments = tail.splitn(2, '/');
        let cast_id = segments.next().unwrap_or("");
        let action = segments.next().unwrap_or("");
        let Some(entry) = session.cast(cast_id) else {
            tracing::debug!(
                xfer,
                cast_id_len = cast_id.len(),
                "lan sender refused an unknown cast id with 404"
            );
            return Ok(status_response(StatusCode::NOT_FOUND));
        };
        if action == "manifest" {
            return Ok(handle_manifest(session, &entry, &xfer).await);
        }
        if let Some(file_name) = action.strip_prefix("files/") {
            return Ok(handle_file(session, &entry, file_name, &request, &xfer).await);
        }
    }
    tracing::debug!(
        xfer,
        "lan sender refused an unknown protocol route with 404"
    );
    Ok(status_response(StatusCode::NOT_FOUND))
}

fn handle_pair(
    session: &Arc<SessionState>,
    request: &Request<hyper::body::Incoming>,
    xfer: &str,
) -> Response<Body> {
    let secret_header = request.headers().get("x-pairing-secret");
    let supplied = secret_header
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let device_name = request
        .headers()
        .get("x-device-name")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("Phone");
    tracing::info!(
        xfer,
        secret_header_present = secret_header.is_some(),
        secret_len = supplied.len(),
        device_name_len = device_name.len(),
        "lan sender pair attempt received"
    );
    let response = pair_response(session, supplied, device_name);
    tracing::info!(
        xfer,
        status = response.status().as_u16(),
        "lan sender pair attempt answered"
    );
    response
}

fn pair_response(session: &Arc<SessionState>, supplied: &str, device_name: &str) -> Response<Body> {
    match session.try_pair(supplied, device_name) {
        PairOutcome::Token(token) => json_response(
            StatusCode::OK,
            &PairOk {
                device_token: token,
            },
        ),
        PairOutcome::Rejected => json_response(
            StatusCode::FORBIDDEN,
            &WireError {
                error: "secret rejected",
            },
        ),
    }
}

const MAX_RECEIPT_BYTES: u64 = 64 * 1024;

async fn handle_receipt(
    session: &Arc<SessionState>,
    request: Request<hyper::body::Incoming>,
    xfer: &str,
) -> Response<Body> {
    let content_len = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if content_len.is_some_and(|len| len > MAX_RECEIPT_BYTES) {
        tracing::warn!(
            xfer,
            "lan sender receipt payload too large (content-length)"
        );
        return status_response(StatusCode::PAYLOAD_TOO_LARGE);
    }
    // Bound the read at the cap instead of buffering first and checking after:
    // a peer that omits `content-length` frames the body as chunked and controls
    // how long we read, so an unbounded `collect()` parks one task plus one
    // growing buffer per request with no deadline on the wire size. The
    // declared-length check above stays as the cheap early refusal.
    let body_bytes = match Limited::new(request.into_body(), MAX_RECEIPT_BYTES as usize)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => {
            tracing::warn!(
                xfer,
                limit = MAX_RECEIPT_BYTES,
                "lan sender receipt payload exceeded the cap while streaming"
            );
            return status_response(StatusCode::PAYLOAD_TOO_LARGE);
        }
        Err(e) => {
            tracing::warn!(xfer, error = %e, "lan sender receipt body read failed");
            return status_response(StatusCode::BAD_REQUEST);
        }
    };
    if body_bytes.len() as u64 > MAX_RECEIPT_BYTES {
        tracing::warn!(xfer, "lan sender receipt payload too large (actual bytes)");
        return status_response(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let receipt: crate::lan_sender::proto::Receipt = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(xfer, error = %e, "lan sender receipt json malformed");
            return status_response(StatusCode::BAD_REQUEST);
        }
    };
    if let Err(reason) = receipt.validate() {
        tracing::warn!(xfer, reason, "lan sender receipt validation failed");
        return status_response(StatusCode::BAD_REQUEST);
    }
    match session.record_receipt(&receipt).await {
        Ok(_) => {
            tracing::info!(xfer, cast_id = %receipt.cast_id, "lan sender receipt recorded");
            json_response(StatusCode::OK, &serde_json::json!({ "status": "ok" }))
        }
        Err(e) if e.contains("unknown cast id") => {
            tracing::warn!(xfer, cast_id = %receipt.cast_id, "lan sender receipt for unknown cast");
            status_response(StatusCode::NOT_FOUND)
        }
        Err(reason) => {
            tracing::warn!(xfer, reason, "lan sender receipt rejected");
            status_response(StatusCode::BAD_REQUEST)
        }
    }
}

fn handle_library(session: &Arc<SessionState>) -> Response<Body> {
    let casts = session
        .casts
        .iter()
        .map(|entry| LibraryCast {
            cast_id: entry.source.cast_id.clone(),
            title: entry.source.title.clone(),
            duration_seconds: entry.source.duration_seconds,
            total_size: entry.size_hint.load(Ordering::Relaxed),
        })
        .collect();
    json_response(StatusCode::OK, &Library { casts })
}

async fn handle_manifest(
    session: &Arc<SessionState>,
    entry: &Arc<CastEntry>,
    xfer: &str,
) -> Response<Body> {
    let meta = match session.ensure_meta(entry).await {
        Ok(meta) => meta,
        Err(MetaError::Missing) => {
            tracing::warn!(
                xfer,
                cast_id = %entry.source.cast_id,
                "lan sender manifest source file missing, answered 404"
            );
            return status_response(StatusCode::NOT_FOUND);
        }
        Err(MetaError::Io(reason)) => {
            tracing::warn!(xfer, reason, "lan sender manifest hashing failed");
            return status_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let manifest = {
        let cover = match session.ensure_cover_meta(entry).await {
            Ok(cover) => cover,
            Err(MetaError::Io(reason)) => {
                tracing::warn!(xfer, reason, "lan sender cover hashing failed");
                return status_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
            // ensure_cover_meta reports absence as Ok(None); Missing is
            // unreachable here but must stay exhaustive.
            Err(MetaError::Missing) => None,
        };
        let alignment = match session.ensure_alignment_meta(entry).await {
            Ok(alignment) => alignment,
            Err(MetaError::Io(reason)) => {
                tracing::warn!(xfer, reason, "lan sender alignment hashing failed");
                return status_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
            Err(MetaError::Missing) => None,
        };
        session.manifest_for(entry, &meta, cover.as_deref(), alignment.as_deref())
    };
    if let Err(reason) = manifest.validate() {
        tracing::warn!(xfer, reason, "lan sender produced an invalid manifest");
        return status_response(StatusCode::INTERNAL_SERVER_ERROR);
    }
    tracing::debug!(
        xfer,
        cast_id = %entry.source.cast_id,
        size = meta.size,
        chunks = meta.chunk_hashes.len(),
        "lan sender served a manifest"
    );
    json_response(StatusCode::OK, &manifest)
}

pub enum RangePlan {
    Full,
    Partial(u64, u64),
    Invalid,
}

pub fn plan_range(range: Option<&str>, if_range: Option<&str>, etag: &str, size: u64) -> RangePlan {
    plan_range_multi(range, if_range, &[etag], size)
}

pub fn plan_range_multi(
    range: Option<&str>,
    if_range: Option<&str>,
    etags: &[&str],
    size: u64,
) -> RangePlan {
    let Some(raw) = range else {
        return RangePlan::Full;
    };
    if if_range.is_some_and(|pinned| !etags.contains(&pinned)) {
        return RangePlan::Full;
    }
    let Some(spec) = raw.strip_prefix("bytes=") else {
        return RangePlan::Invalid;
    };
    let Some((start_raw, end_raw)) = spec.split_once('-') else {
        return RangePlan::Invalid;
    };
    let Ok(start) = start_raw.parse::<u64>() else {
        return RangePlan::Invalid;
    };
    if start >= size {
        return RangePlan::Invalid;
    }
    let end = if end_raw.is_empty() {
        size - 1
    } else {
        match end_raw.parse::<u64>() {
            Ok(end) => end.min(size - 1),
            Err(_) => return RangePlan::Invalid,
        }
    };
    if end < start {
        return RangePlan::Invalid;
    }
    RangePlan::Partial(start, end)
}

/// Which bundle file a `files/<name>` request resolves to. The server never
/// opens caller-supplied paths: only the hashed audio entry, hashed
/// cover entry, and hashed alignment entry (when the manifest advertised them) are servable.
enum BundleFile<'a> {
    Audio,
    Cover(&'a session::CoverMeta),
    Alignment(&'a session::AlignmentFileMeta),
}

fn resolve_bundle_file<'a>(
    entry: &'a CastEntry,
    file_name: &str,
    cover: Option<&'a session::CoverMeta>,
    alignment: Option<&'a session::AlignmentFileMeta>,
) -> Option<BundleFile<'a>> {
    if file_name == entry.wire_path {
        return Some(BundleFile::Audio);
    }
    if let Some(cover) = cover {
        if file_name == cover.wire {
            return Some(BundleFile::Cover(cover));
        }
    }
    if let Some(alignment) = alignment {
        if file_name == alignment.wire {
            return Some(BundleFile::Alignment(alignment));
        }
    }
    None
}

async fn handle_file(
    session: &Arc<SessionState>,
    entry: &Arc<CastEntry>,
    file_name: &str,
    request: &Request<hyper::body::Incoming>,
    xfer: &str,
) -> Response<Body> {
    let cover = match session.ensure_cover_meta(entry).await {
        Ok(cover) => cover,
        Err(MetaError::Io(reason)) => {
            tracing::warn!(xfer, reason, "lan sender cover metadata failed");
            return status_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
        // Absence is Ok(None); Missing is unreachable but exhaustive.
        Err(MetaError::Missing) => None,
    };
    let alignment = match session.ensure_alignment_meta(entry).await {
        Ok(alignment) => alignment,
        Err(MetaError::Io(reason)) => {
            tracing::warn!(xfer, reason, "lan sender alignment metadata failed");
            return status_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(MetaError::Missing) => None,
    };
    let resolved =
        match resolve_bundle_file(entry, file_name, cover.as_deref(), alignment.as_deref()) {
            Some(resolved) => resolved,
            None => {
                tracing::debug!(
                    xfer,
                    cast_id = %entry.source.cast_id,
                    file_name_len = file_name.len(),
                    "lan sender refused a non-wire file name with 404"
                );
                return status_response(StatusCode::NOT_FOUND);
            }
        };
    let (disk_path, meta) = match &resolved {
        BundleFile::Audio => (entry.source.path.clone(), session.ensure_meta(entry).await),
        BundleFile::Cover(cover) => (cover.path.clone(), Ok(cover.meta.clone())),
        BundleFile::Alignment(alignment) => (alignment.path.clone(), Ok(alignment.meta.clone())),
    };
    let meta = match meta {
        Ok(meta) => meta,
        Err(MetaError::Missing) => {
            tracing::warn!(
                xfer,
                cast_id = %entry.source.cast_id,
                "lan sender file source missing on disk, answered 404"
            );
            return status_response(StatusCode::NOT_FOUND);
        }
        Err(MetaError::Io(reason)) => {
            tracing::warn!(xfer, reason, "lan sender file metadata failed");
            return status_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let manifest_meta = session.ensure_meta(entry).await.ok();
    let manifest_etag = manifest_meta.as_ref().map(|m| m.etag.as_str());
    serve_file(
        session,
        entry,
        file_name,
        &disk_path,
        &meta,
        manifest_etag,
        request,
        xfer,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn serve_file(
    session: &Arc<SessionState>,
    entry: &Arc<CastEntry>,
    file_name: &str,
    disk_path: &Path,
    meta: &session::FileMeta,
    manifest_etag: Option<&str>,
    request: &Request<hyper::body::Incoming>,
    xfer: &str,
) -> Response<Body> {
    let range = request.headers().get(RANGE).and_then(|v| v.to_str().ok());
    let if_range = request
        .headers()
        .get(IF_RANGE)
        .and_then(|v| v.to_str().ok());
    let mut valid_etags = vec![meta.etag.as_str()];
    if let Some(m_etag) = manifest_etag {
        if !valid_etags.contains(&m_etag) {
            valid_etags.push(m_etag);
        }
    }
    let stale_if_range = if_range.is_some_and(|pinned| !valid_etags.contains(&pinned));
    let (status, start, end) = match plan_range_multi(range, if_range, &valid_etags, meta.size) {
        RangePlan::Invalid => {
            tracing::warn!(
                xfer,
                cast_id = %entry.source.cast_id,
                range_header_len = range.map(str::len).unwrap_or(0),
                size = meta.size,
                "lan sender range unsatisfiable, answered 416"
            );
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{}", meta.size))
                .header(CONTENT_LENGTH, 0)
                .header(CONNECTION, "close")
                .body(Empty::new().map_err(std::io::Error::other).boxed_unsync())
                .expect("static response");
        }
        RangePlan::Partial(start, end) => (StatusCode::PARTIAL_CONTENT, start, end),
        RangePlan::Full => {
            if meta.size == 0 {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header(ETAG, meta.etag.clone())
                    .header(CONTENT_LENGTH, 0)
                    .header(CONNECTION, "close")
                    .body(Empty::new().map_err(std::io::Error::other).boxed_unsync())
                    .expect("static response");
            }
            (StatusCode::OK, 0, meta.size - 1)
        }
    };
    let length = end - start + 1;
    if stale_if_range {
        tracing::debug!(
            xfer,
            cast_id = %entry.source.cast_id,
            "lan sender ignored a stale if-range pin, serving the full body"
        );
    }
    tracing::debug!(
        xfer,
        cast_id = %entry.source.cast_id,
        file_name = %file_name,
        status = status.as_u16(),
        start,
        end,
        length,
        "lan sender streaming a file range"
    );
    let body = stream_file(
        session.clone(),
        entry.clone(),
        disk_path.to_path_buf(),
        file_name.to_string(),
        meta.size,
        start,
        end,
        xfer.to_string(),
    );
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(ETAG, meta.etag.clone())
        .header(CONTENT_LENGTH, length)
        .header(CONNECTION, "close");
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(CONTENT_RANGE, format!("bytes {start}-{end}/{}", meta.size));
    }
    builder.body(body).expect("file response")
}

#[allow(clippy::too_many_arguments)]
fn stream_file(
    session: Arc<SessionState>,
    entry: Arc<CastEntry>,
    disk_path: std::path::PathBuf,
    file_name: String,
    size: u64,
    start: u64,
    end: u64,
    xfer: String,
) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(2);
    let xfer_owned = xfer.clone();
    let writer_session = session.clone();
    let writer_entry = entry.clone();
    tokio::spawn(async move {
        let session = writer_session;
        let entry = writer_entry;
        let mut file = match tokio::fs::File::open(&disk_path).await {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    xfer,
                    cast_id = %entry.source.cast_id,
                    error = %error,
                    "lan sender stream aborted, source open failed"
                );
                let _ = tx.send(Err(error)).await;
                return;
            }
        };
        if let Err(error) = file.seek(SeekFrom::Start(start)).await {
            tracing::warn!(
                xfer,
                cast_id = %entry.source.cast_id,
                start,
                error = %error,
                "lan sender stream aborted, seek failed"
            );
            let _ = tx.send(Err(error)).await;
            return;
        }
        let mut remaining = end - start + 1;
        let mut buf = vec![0u8; STREAM_BUF_BYTES];
        while remaining > 0 {
            let take = remaining.min(buf.len() as u64) as usize;
            match file.read(&mut buf[..take]).await {
                Ok(0) => {
                    tracing::warn!(
                        xfer,
                        cast_id = %entry.source.cast_id,
                        remaining,
                        "lan sender stream aborted, file shrank while sending"
                    );
                    let _ = tx
                        .send(Err(std::io::Error::other("file shrank while sending")))
                        .await;
                    return;
                }
                Ok(read) => {
                    let frame = Frame::data(Bytes::copy_from_slice(&buf[..read]));
                    if tx.send(Ok(frame)).await.is_err() {
                        tracing::debug!(
                            xfer,
                            cast_id = %entry.source.cast_id,
                            remaining,
                            "lan sender stream ended early, peer dropped the connection"
                        );
                        return;
                    }
                    session.record_sent(&entry, read as u64);
                    remaining -= read as u64;
                }
                Err(error) => {
                    tracing::warn!(
                        xfer,
                        cast_id = %entry.source.cast_id,
                        remaining,
                        error = %error,
                        "lan sender stream aborted, read failed"
                    );
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }
        tracing::debug!(
            xfer,
            cast_id = %entry.source.cast_id,
            start,
            end,
            "lan sender finished queueing a range"
        );
    });
    // Completion is recorded only when the last frame is handed to hyper, not
    // when the reader task finishes queueing it: queueing runs megabytes ahead
    // of the peer (channel + TLS + kernel buffers), and a "done" that fires on
    // queueing flips the session phase while the phone is still mid-download —
    // inviting a stop that kills the transfer it just reported as sent.
    struct Drain {
        rx: tokio::sync::mpsc::Receiver<Result<Frame<Bytes>, std::io::Error>>,
        session: Arc<SessionState>,
        entry: Arc<CastEntry>,
        file_name: String,
        size: u64,
        end: u64,
        remaining: u64,
        xfer: String,
    }
    let state = Drain {
        rx,
        session,
        entry,
        file_name,
        size,
        end,
        remaining: end - start + 1,
        xfer: xfer_owned,
    };
    let stream = futures_util::stream::unfold(state, |mut s| async move {
        let item = s.rx.recv().await?;
        if let Ok(frame) = &item {
            if let Some(data) = frame.data_ref() {
                s.remaining = s.remaining.saturating_sub(data.len() as u64);
                if s.remaining == 0 {
                    s.session
                        .record_range_complete(&s.entry, &s.file_name, s.end, s.size);
                    tracing::debug!(
                        xfer = %s.xfer,
                        cast_id = %s.entry.source.cast_id,
                        file_name = %s.file_name,
                        end = s.end,
                        "lan sender delivered the final frame of a range"
                    );
                }
            }
        }
        Some((item, s))
    });
    StreamBody::new(stream).boxed_unsync()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_plans_follow_the_contract() {
        assert!(matches!(plan_range(None, None, "e", 100), RangePlan::Full));
        assert!(matches!(
            plan_range(Some("bytes=0-49"), None, "e", 100),
            RangePlan::Partial(0, 49)
        ));
        assert!(matches!(
            plan_range(Some("bytes=50-"), None, "e", 100),
            RangePlan::Partial(50, 99)
        ));
        assert!(matches!(
            plan_range(Some("bytes=50-1000"), None, "e", 100),
            RangePlan::Partial(50, 99)
        ));
        assert!(matches!(
            plan_range(Some("bytes=0-49"), Some("e"), "e", 100),
            RangePlan::Partial(0, 49)
        ));
    }

    #[test]
    fn stale_if_range_forces_full_body() {
        assert!(matches!(
            plan_range(Some("bytes=50-99"), Some("old"), "current", 100),
            RangePlan::Full
        ));
    }

    fn pair_test_session() -> (tempfile::TempDir, Arc<SessionState>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("book.mp3");
        std::fs::write(&path, b"payload").expect("write payload");
        let session = SessionState::new(
            vec![session::CastSource {
                cast_id: "cst_pair".into(),
                title: "Pair".into(),
                path,
                cover_path: None,
                alignment_path: None,
                duration_seconds: None,
            }],
            crate::lan_sender::proto::MIN_CHUNK_SIZE as u64,
            Arc::new(|_| {}),
        )
        .expect("session");
        (dir, Arc::new(session))
    }

    async fn response_fields(
        resp: Response<Body>,
    ) -> (StatusCode, Vec<(String, Vec<u8>)>, Vec<u8>) {
        let status = resp.status();
        let headers: Vec<(String, Vec<u8>)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
            .collect();
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    #[tokio::test]
    async fn consumed_replay_and_wrong_guess_yield_byte_identical_rejections() {
        let (_dir, session) = pair_test_session();
        let secret = session.mint_secret();
        let paired = pair_response(&session, &secret, "Phone");
        assert_eq!(paired.status(), StatusCode::OK);

        let replay = response_fields(pair_response(&session, &secret, "Phone")).await;
        let guess = response_fields(pair_response(
            &session,
            "ffffffffffffffffffffffffffffffff",
            "Phone",
        ))
        .await;
        assert_eq!(replay.0, StatusCode::FORBIDDEN);
        assert_eq!(replay.0, guess.0);
        assert_eq!(replay.1, guess.1);
        assert_eq!(replay.2, guess.2);
    }
    #[test]
    fn bundle_resolution_serves_only_hashed_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let audio = dir.path().join("book.mp3");
        std::fs::write(&audio, b"payload").expect("write audio");
        let session = SessionState::new(
            vec![session::CastSource {
                cast_id: "cst_cover".into(),
                title: "Cover".into(),
                path: audio,
                cover_path: None,
                alignment_path: None,
                duration_seconds: None,
            }],
            crate::lan_sender::proto::MIN_CHUNK_SIZE as u64,
            Arc::new(|_| {}),
        )
        .expect("session");
        let entry = session.cast("cst_cover").expect("entry");
        // Audio always resolves, even with no cover anywhere.
        assert!(matches!(
            resolve_bundle_file(&entry, "audio.mp3", None, None),
            Some(BundleFile::Audio)
        ));
        assert!(resolve_bundle_file(&entry, "cover.png", None, None).is_none());
        assert!(resolve_bundle_file(&entry, "alignment.json", None, None).is_none());
        assert!(resolve_bundle_file(&entry, "secrets.zip", None, None).is_none());

        // An advertised cover resolves by its exact wire name only.
        let cover = session::CoverMeta {
            path: dir.path().join("cover.png"),
            wire: "cover.png",
            meta: Arc::new(session::FileMeta {
                size: 3,
                modified_ms: 0,
                sha256: "a".repeat(64),
                chunk_hashes: vec![],
                etag: "etag-abc".into(),
            }),
        };
        assert!(matches!(
            resolve_bundle_file(&entry, "cover.png", Some(&cover), None),
            Some(BundleFile::Cover(_))
        ));
        assert!(resolve_bundle_file(&entry, "cover.jpg", Some(&cover), None).is_none());
        assert!(resolve_bundle_file(&entry, "audio.mp3", Some(&cover), None).is_some());

        // An advertised alignment resolves by its exact wire name only.
        let alignment = session::AlignmentFileMeta {
            path: dir.path().join("alignment.json"),
            wire: "alignment.json",
            meta: Arc::new(session::FileMeta {
                size: 10,
                modified_ms: 0,
                sha256: "b".repeat(64),
                chunk_hashes: vec![],
                etag: "etag-def".into(),
            }),
            proto_meta: crate::lan_sender::proto::AlignmentMeta {
                origin: Some("measured".into()),
                total_ms: Some(60_000),
            },
        };
        assert!(matches!(
            resolve_bundle_file(&entry, "alignment.json", None, Some(&alignment)),
            Some(BundleFile::Alignment(_))
        ));
        assert!(resolve_bundle_file(&entry, "other.json", None, Some(&alignment)).is_none());
    }

    #[test]
    fn bad_ranges_are_unsatisfiable() {
        for header in [
            "bytes=100-200",
            "bytes=5-1",
            "bytes=-50",
            "bytes=abc-def",
            "bytes=0-1,5-6",
            "items=0-1",
            "bytes=0",
        ] {
            assert!(
                matches!(plan_range(Some(header), None, "e", 100), RangePlan::Invalid),
                "{header} should be invalid"
            );
        }
        assert!(matches!(
            plan_range(Some("bytes=0-"), None, "e", 0),
            RangePlan::Invalid
        ));
    }
}
