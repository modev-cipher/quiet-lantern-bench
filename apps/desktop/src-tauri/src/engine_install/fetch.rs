use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use super::manifest::{EngineArchive, EnginePart};
use crate::updater::manifest::url_is_acceptable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Offline(String),
    Http { status: u16 },
    InsecureUrl(String),
    Digest { part: usize },
    TooLong { part: usize },
    Io(String),
    Cancelled,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Offline(why) => write!(f, "the download could not connect: {why}"),
            FetchError::Http { status } => write!(f, "the download server answered {status}"),
            FetchError::InsecureUrl(url) => write!(f, "refused a non-https download url {url}"),
            FetchError::Digest { part } => {
                write!(f, "part {part} did not match its signed checksum")
            }
            FetchError::TooLong { part } => write!(f, "part {part} is longer than its signed size"),
            FetchError::Io(why) => write!(f, "the download could not be saved: {why}"),
            FetchError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<std::io::Error> for FetchError {
    fn from(error: std::io::Error) -> Self {
        FetchError::Io(error.to_string())
    }
}

pub fn part_file(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("part.{index:03}"))
}

fn hash_existing(path: &Path, limit: u64) -> std::io::Result<(Sha256, u64)> {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?.take(limit);
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher, total))
}

fn matches_digest(hasher: Sha256, expected_hex: &str) -> bool {
    hex::encode(hasher.finalize()) == expected_hex.to_lowercase()
}

async fn fetch_part(
    client: &reqwest::Client,
    part: &EnginePart,
    index: usize,
    path: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut (dyn FnMut(u64) + Send),
) -> Result<(), FetchError> {
    let url =
        reqwest::Url::parse(&part.url).map_err(|_| FetchError::InsecureUrl(part.url.clone()))?;
    if !url_is_acceptable(&url) {
        return Err(FetchError::InsecureUrl(part.url.clone()));
    }
    let existing = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if existing > part.size {
        std::fs::remove_file(path)?;
    }
    let (mut hasher, mut have) = if existing > 0 && existing <= part.size {
        hash_existing(path, part.size)?
    } else {
        (Sha256::new(), 0)
    };
    on_bytes(have);
    if have == part.size {
        if matches_digest(hasher, &part.sha256) {
            return Ok(());
        }
        std::fs::remove_file(path)?;
        hasher = Sha256::new();
        have = 0;
    }

    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let response = request
        .send()
        .await
        .map_err(|e| FetchError::Offline(e.to_string()))?;
    let status = response.status();
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT && have > 0;
    if !(status.is_success()) {
        return Err(FetchError::Http {
            status: status.as_u16(),
        });
    }
    if !resumed && have > 0 {
        on_bytes(0);
        hasher = Sha256::new();
        have = 0;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(path)?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            file.flush()?;
            return Err(FetchError::Cancelled);
        }
        let chunk = chunk.map_err(|e| FetchError::Offline(e.to_string()))?;
        have += chunk.len() as u64;
        if have > part.size {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(FetchError::TooLong { part: index });
        }
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        on_bytes(have);
    }
    file.sync_all()?;
    if have != part.size || !matches_digest(hasher, &part.sha256) {
        let _ = std::fs::remove_file(path);
        return Err(FetchError::Digest { part: index });
    }
    Ok(())
}

/// Download every part of `archive` into `dir` (resuming what is already there), checking
/// each against its signed size and sha256. Returns the part files in order.
pub async fn fetch_parts(
    client: &reqwest::Client,
    archive: &EngineArchive,
    dir: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64) + Send,
) -> Result<Vec<PathBuf>, FetchError> {
    std::fs::create_dir_all(dir)?;
    let mut finished: u64 = 0;
    let mut paths = Vec::with_capacity(archive.parts.len());
    for (index, part) in archive.parts.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(FetchError::Cancelled);
        }
        let path = part_file(dir, index);
        let base = finished;
        let total = archive.size;
        fetch_part(client, part, index, &path, cancel, &mut |have| {
            on_progress(base + have, total)
        })
        .await?;
        finished += part.size;
        paths.push(path);
    }
    Ok(paths)
}

pub fn whole_digest(paths: &[PathBuf]) -> std::io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    for path in paths {
        let mut file = std::fs::File::open(path)?;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
pub(crate) mod fixture {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Default)]
    pub struct Served {
        pub files: HashMap<String, Vec<u8>>,
        pub ignore_range: bool,
        pub requests: Vec<(String, Option<String>)>,
        pub fail_after: Option<(String, usize)>,
    }

    /// Minimal file server for loopback fixtures: GET with optional Range.
    pub async fn serve(state: Arc<Mutex<Served>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut read = 0;
                    loop {
                        let Ok(n) = socket.read(&mut buf[read..]).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        read += n;
                        if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..read]).to_string();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let range = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .map(|l| l[6..].trim().to_string());
                    let (body, status, cut) = {
                        let mut s = state.lock().expect("lock");
                        s.requests.push((path.clone(), range.clone()));
                        let cut = s
                            .fail_after
                            .take()
                            .filter(|(p, _)| *p == path)
                            .map(|(_, n)| n);
                        match s.files.get(&path).cloned() {
                            None => (Vec::new(), "404 Not Found", None),
                            Some(full) => {
                                let start = range
                                    .as_deref()
                                    .filter(|_| !s.ignore_range)
                                    .and_then(|r| r.strip_prefix("bytes="))
                                    .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
                                match start {
                                    Some(start) => (
                                        full[start.min(full.len())..].to_vec(),
                                        "206 Partial Content",
                                        cut,
                                    ),
                                    None => (full, "200 OK", cut),
                                }
                            }
                        }
                    };
                    let _ = crate::test_support::mock_http::write_head(
                        &mut socket,
                        status,
                        &[],
                        Some(body.len()),
                    )
                    .await;
                    match cut {
                        Some(n) => {
                            let _ = socket.write_all(&body[..n.min(body.len())]).await;
                        }
                        None => {
                            let _ = socket.write_all(&body).await;
                        }
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        base
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixture::{serve, Served};
    use super::*;

    fn bytes(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    fn archive(base: &str, parts: &[Vec<u8>]) -> EngineArchive {
        let mut whole = Sha256::new();
        let parts: Vec<EnginePart> = parts
            .iter()
            .enumerate()
            .map(|(i, body)| {
                whole.update(body);
                EnginePart {
                    url: format!("{base}/e.{i:03}"),
                    size: body.len() as u64,
                    sha256: hex::encode(Sha256::digest(body)),
                }
            })
            .collect();
        EngineArchive {
            size: parts.iter().map(|p| p.size).sum(),
            sha256: hex::encode(whole.finalize()),
            signature: String::new(),
            unpacked_size: 0,
            parts,
        }
    }

    async fn served(parts: &[Vec<u8>]) -> (Arc<Mutex<Served>>, String) {
        let state = Arc::new(Mutex::new(Served::default()));
        for (i, body) in parts.iter().enumerate() {
            state
                .lock()
                .expect("lock")
                .files
                .insert(format!("/e.{i:03}"), body.clone());
        }
        let base = serve(Arc::clone(&state)).await;
        (state, base)
    }

    #[tokio::test]
    async fn all_parts_download_in_order_and_the_whole_digest_matches() {
        let parts = vec![bytes(3000, 1), bytes(2000, 2), bytes(10, 3)];
        let (_state, base) = served(&parts).await;
        let archive = archive(&base, &parts);
        let dir = tempfile::tempdir().expect("dir");
        let mut last = (0, 0);
        let files = fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |done, total| last = (done, total),
        )
        .await
        .expect("fetch");
        assert_eq!(files.len(), 3);
        assert_eq!(last, (5010, 5010));
        assert_eq!(
            hex::encode(whole_digest(&files).expect("digest")),
            archive.sha256
        );
    }

    #[tokio::test]
    async fn an_interrupted_part_resumes_with_a_range_request() {
        let parts = vec![bytes(4000, 7)];
        let (state, base) = served(&parts).await;
        state.lock().expect("lock").fail_after = Some(("/e.000".into(), 1500));
        let archive = archive(&base, &parts);
        let dir = tempfile::tempdir().expect("dir");
        let client = reqwest::Client::new();
        let first = fetch_parts(
            &client,
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await;
        assert!(first.is_err(), "a cut transfer must not pass: {first:?}");
        let kept = std::fs::metadata(part_file(dir.path(), 0))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(kept > 0, "the cut transfer keeps its bytes for resume");
        fetch_parts(
            &client,
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await
        .expect("resume");
        let requests = state.lock().expect("lock").requests.clone();
        assert_eq!(
            requests.last(),
            Some(&("/e.000".to_string(), Some(format!("bytes={kept}-"))))
        );
        assert_eq!(
            std::fs::read(part_file(dir.path(), 0)).expect("part"),
            parts[0]
        );
    }

    #[tokio::test]
    async fn a_server_that_ignores_range_restarts_the_part_cleanly() {
        let parts = vec![bytes(4000, 9)];
        let (state, base) = served(&parts).await;
        state.lock().expect("lock").ignore_range = true;
        let archive = archive(&base, &parts);
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(part_file(dir.path(), 0), &parts[0][..1000]).expect("partial");
        fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await
        .expect("fetch");
        assert_eq!(
            std::fs::read(part_file(dir.path(), 0)).expect("part"),
            parts[0]
        );
    }

    #[tokio::test]
    async fn a_complete_verified_part_is_not_downloaded_again() {
        let parts = vec![bytes(500, 1), bytes(500, 2)];
        let (state, base) = served(&parts).await;
        let archive = archive(&base, &parts);
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(part_file(dir.path(), 0), &parts[0]).expect("done part");
        fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await
        .expect("fetch");
        let requested: Vec<String> = state
            .lock()
            .expect("lock")
            .requests
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(requested, vec!["/e.001".to_string()]);
    }

    #[tokio::test]
    async fn a_part_with_the_wrong_bytes_is_refused_and_removed() {
        let parts = vec![bytes(800, 4)];
        let (state, base) = served(&parts).await;
        let archive = archive(&base, &parts);
        state
            .lock()
            .expect("lock")
            .files
            .insert("/e.000".into(), bytes(800, 5));
        let dir = tempfile::tempdir().expect("dir");
        let result = fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await;
        assert_eq!(result, Err(FetchError::Digest { part: 0 }));
        assert!(!part_file(dir.path(), 0).exists());
    }

    #[tokio::test]
    async fn a_part_longer_than_signed_is_refused() {
        let parts = vec![bytes(800, 4)];
        let (state, base) = served(&parts).await;
        let archive = archive(&base, &parts);
        state
            .lock()
            .expect("lock")
            .files
            .insert("/e.000".into(), bytes(5000, 4));
        let dir = tempfile::tempdir().expect("dir");
        let result = fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await;
        assert_eq!(result, Err(FetchError::TooLong { part: 0 }));
    }

    #[tokio::test]
    async fn a_non_https_remote_url_is_refused_before_any_request() {
        let parts = vec![bytes(10, 1)];
        let mut archive = archive("http://example.org", &parts);
        archive.parts[0].url = "http://example.org/e.000".into();
        let dir = tempfile::tempdir().expect("dir");
        let result = fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .await;
        assert!(
            matches!(result, Err(FetchError::InsecureUrl(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_cancelled_fetch_stops_before_the_next_part() {
        let parts = vec![bytes(10, 1)];
        let (_state, base) = served(&parts).await;
        let archive = archive(&base, &parts);
        let dir = tempfile::tempdir().expect("dir");
        let result = fetch_parts(
            &reqwest::Client::new(),
            &archive,
            dir.path(),
            &AtomicBool::new(true),
            |_, _| {},
        )
        .await;
        assert_eq!(result, Err(FetchError::Cancelled));
    }
}
