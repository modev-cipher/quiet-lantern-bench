#![allow(dead_code)]

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme, SupportedProtocolVersion};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

#[derive(Debug)]
struct PinVerifier {
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = hex::encode(Sha256::digest(end_entity.as_ref()));
        if actual == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("fingerprint mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RawResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("json body")
    }
}

pub async fn request(
    port: u16,
    fingerprint: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> RawResponse {
    request_with_body(port, fingerprint, method, path, headers, &[]).await
}

pub async fn request_with_body(
    port: u16,
    fingerprint: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> RawResponse {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("tls13 client")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier {
            fingerprint: fingerprint.to_string(),
            provider,
        }))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from("txtcast.local").expect("server name");
    let mut tls = connector.connect(name, stream).await.expect("handshake");
    let mut head = format!("{method} {path} HTTP/1.1\r\nhost: txtcast.local\r\n");
    for (key, value) in headers {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str(&format!(
        "connection: close\r\ncontent-length: {}\r\n\r\n",
        body.len()
    ));
    tls.write_all(head.as_bytes()).await.expect("write request");
    if !body.is_empty() {
        tls.write_all(body).await.expect("write body");
    }
    let mut raw = Vec::new();
    let _ = tls.read_to_end(&mut raw).await;
    parse(raw)
}

fn client_config(
    fingerprint: &str,
    versions: &[&'static SupportedProtocolVersion],
) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)
        .expect("client protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier {
            fingerprint: fingerprint.to_string(),
            provider,
        }))
        .with_no_client_auth()
}

pub async fn try_connect(
    port: u16,
    fingerprint: &str,
    versions: &[&'static SupportedProtocolVersion],
) -> std::io::Result<TlsStream<TcpStream>> {
    let stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let connector =
        tokio_rustls::TlsConnector::from(Arc::new(client_config(fingerprint, versions)));
    let name = ServerName::try_from("txtcast.local").expect("server name");
    connector.connect(name, stream).await
}

pub async fn raw_request(port: u16, fingerprint: &str, raw: &[u8]) -> Vec<u8> {
    let mut tls = try_connect(port, fingerprint, &[&rustls::version::TLS13])
        .await
        .expect("handshake");
    tls.write_all(raw).await.expect("write raw");
    tls.flush().await.expect("flush raw");
    let mut buf = Vec::new();
    let _ = tls.read_to_end(&mut buf).await;
    buf
}

/// Streams a request body framed with `transfer-encoding: chunked`.
///
/// `request_with_body` always declares `content-length`, so a peer that omits
/// the declared size — the only way past the receipt endpoint's early
/// length check — had no coverage. `terminate` sends the final zero-length
/// chunk; false leaves the body open on purpose, which is how a sender that
/// buffers before applying the cap is made to park a task.
#[allow(clippy::too_many_arguments)]
async fn chunked_impl(
    port: u16,
    fingerprint: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    chunk_bytes: usize,
    terminate: bool,
) -> Vec<u8> {
    let mut tls = try_connect(port, fingerprint, &[&rustls::version::TLS13])
        .await
        .expect("handshake");
    let mut head = format!("{method} {path} HTTP/1.1\r\nhost: txtcast.local\r\n");
    for (key, value) in headers {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str("connection: close\r\ntransfer-encoding: chunked\r\n\r\n");
    tls.write_all(head.as_bytes()).await.expect("write head");
    for chunk in body.chunks(chunk_bytes.max(1)) {
        tls.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await
            .expect("write chunk size");
        tls.write_all(chunk).await.expect("write chunk");
        tls.write_all(b"\r\n")
            .await
            .expect("write chunk terminator");
    }
    if terminate {
        tls.write_all(b"0\r\n\r\n").await.expect("write last chunk");
    }
    tls.flush().await.expect("flush chunked body");
    let mut raw = Vec::new();
    let _ = tls.read_to_end(&mut raw).await;
    raw
}

/// A complete chunked request. Returns the raw response bytes so callers can
/// apply their own deadline (`status_of` reads the status line off them).
pub async fn chunked_request(
    port: u16,
    fingerprint: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    chunk_bytes: usize,
) -> Vec<u8> {
    chunked_impl(
        port,
        fingerprint,
        method,
        path,
        headers,
        body,
        chunk_bytes,
        true,
    )
    .await
}

/// The same request with the body deliberately left open: the final
/// zero-length chunk is never sent, so the server cannot know the body ended.
/// Callers wrap this in a timeout — a bounded reader answers immediately.
pub async fn open_ended_chunked_request(
    port: u16,
    fingerprint: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    chunk_bytes: usize,
) -> Vec<u8> {
    chunked_impl(
        port,
        fingerprint,
        method,
        path,
        headers,
        body,
        chunk_bytes,
        false,
    )
    .await
}

pub fn status_of(raw: &[u8]) -> Option<u16> {
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    head.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

fn parse(raw: Vec<u8>) -> RawResponse {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw[split + 4..].to_vec();
    let mut lines = head.lines();
    let status_line = lines.next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status");
    let headers = lines
        .map(|line| {
            let (key, value) = line.split_once(':').expect("header line");
            (key.trim().to_ascii_lowercase(), value.trim().to_string())
        })
        .collect();
    RawResponse {
        status,
        headers,
        body,
    }
}
