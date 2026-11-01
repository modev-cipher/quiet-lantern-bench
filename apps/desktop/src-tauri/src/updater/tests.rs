//! Loopback fixture origin + the happy path. Sabotage rows live in tests_sabotage.rs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::manifest::{Artifact, Envelope, Manifest};
use super::signing::{PublicKey, SigningKey};
use super::{Config, Phase, Snapshot, Updater};

pub type Routes = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Minimal loopback origin over the shared mock writer: path → body; unknown path → 404. Never the real origin.
pub async fn serve(routes: Routes) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = routes.lock().unwrap().get(&path).cloned();
                match body {
                    // Body "REDIRECT:<url>" answers 302 Location: <url>.
                    Some(b) if b.starts_with(b"REDIRECT:") => {
                        let to = String::from_utf8_lossy(&b[9..]).to_string();
                        crate::test_support::mock_http::write_head(
                            &mut sock,
                            "302 Found",
                            &[("Location", to.as_str())],
                            Some(0),
                        )
                        .await
                    }
                    // Paths containing "slow" stream half the body, pause, then the rest:
                    // a deterministic window for the cancel test.
                    Some(b) if path.contains("slow") => {
                        use tokio::io::AsyncWriteExt;
                        let mid = b.len() / 2;
                        crate::test_support::mock_http::write_head(
                            &mut sock,
                            "200 OK",
                            &[],
                            Some(b.len()),
                        )
                        .await
                        .ok();
                        sock.write_all(&b[..mid]).await.ok();
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        sock.write_all(&b[mid..]).await.ok();
                        Ok(())
                    }
                    Some(b) => {
                        crate::test_support::mock_http::write_response(
                            &mut sock,
                            "200 OK",
                            "application/octet-stream",
                            &b,
                        )
                        .await
                    }
                    None => {
                        crate::test_support::mock_http::write_response(
                            &mut sock,
                            "404 Not Found",
                            "text/plain",
                            b"nope",
                        )
                        .await
                    }
                }
                .ok();
            });
        }
    });
    base
}

pub fn envelope(key: &SigningKey, manifest: &Manifest) -> Vec<u8> {
    let payload = serde_json::to_vec(manifest).unwrap();
    let env = Envelope {
        payload: B64.encode(&payload),
        signature: key.sign_manifest(&payload),
    };
    serde_json::to_vec(&env).unwrap()
}

pub fn artifact(key: &SigningKey, url: String, bytes: &[u8]) -> Artifact {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Artifact {
        url,
        sha256: hex::encode(digest),
        size: bytes.len() as u64,
        signature: key.sign_artifact_digest(&digest),
        kind: Some(PLATFORM_ARTIFACT_KIND.into()),
    }
}

/// The artifact kind this platform's install plan acts on (`install::plan`):
/// the per-user installer on Windows, the self-replacing AppImage elsewhere.
/// The rig publishes it so download-path invariants run on every platform.
pub const PLATFORM_ARTIFACT_KIND: &str = if cfg!(target_os = "windows") {
    "nsis"
} else {
    "appimage"
};

pub fn manifest(version: &str, artifact: Option<Artifact>) -> Manifest {
    let mut platforms = std::collections::BTreeMap::new();
    if let Some(a) = artifact {
        platforms.insert(super::manifest::platform_key(), a);
    }
    Manifest {
        version: version.into(),
        notes: Some("Faster casts".into()),
        pub_date: None,
        pause_updates: false,
        release_url: Some("https://example.invalid/releases".into()),
        platforms,
    }
}

pub fn config(base: &str, pubkey: Option<PublicKey>, dir: &std::path::Path) -> Config {
    Config {
        current_version: "0.1.0".into(),
        manifest_url: Some(reqwest::Url::parse(&format!("{base}/latest.json")).unwrap()),
        pubkey,
        downloads_dir: dir.join("updates"),
        check_interval: Duration::from_secs(3600),
        first_delay: Duration::from_secs(0),
        appimage: Some(dir.join("app.AppImage").to_string_lossy().into_owned()),
    }
}

pub fn updater(cfg: Config) -> (Updater, Arc<Mutex<Vec<Snapshot>>>) {
    let seen: Arc<Mutex<Vec<Snapshot>>> = Arc::default();
    let sink = seen.clone();
    (
        Updater::new(cfg, Box::new(move |s| sink.lock().unwrap().push(s.clone()))).unwrap(),
        seen,
    )
}

/// Full rig: origin serving a signed 0.2.0 with one artifact for this platform.
pub async fn rig(payload: &[u8]) -> (SigningKey, Routes, String, tempfile::TempDir) {
    let (_, key) = SigningKey::generate().unwrap();
    let routes: Routes = Arc::default();
    let base = serve(routes.clone()).await;
    let art = artifact(&key, format!("{base}/txtcaster.AppImage"), payload);
    routes
        .lock()
        .unwrap()
        .insert("/txtcaster.AppImage".into(), payload.to_vec());
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        envelope(&key, &manifest("0.2.0", Some(art))),
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.AppImage"), b"OLD BINARY").unwrap();
    (key, routes, base, dir)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn happy_path_check_then_install_replaces_the_executable_and_asks_for_restart() {
    let new_binary = vec![7u8; 100_000];
    let (key, _routes, base, dir) = rig(&new_binary).await;
    let (u, seen) = updater(config(&base, Some(key.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.available_version.as_deref(), s.install_kind),
        (Phase::Available, Some("0.2.0"), Some("restart"))
    );
    assert_eq!(s.notes.as_deref(), Some("Faster casts"));
    assert!(s.last_checked_unix.is_some());
    assert_eq!(u.install().await, Ok(super::Handoff::Restart));
    assert_eq!(
        std::fs::read(dir.path().join("app.AppImage")).unwrap(),
        new_binary
    );
    let phases: Vec<Phase> = seen.lock().unwrap().iter().map(|s| s.phase).collect();
    for p in [
        Phase::Checking,
        Phase::Downloading,
        Phase::Verifying,
        Phase::Installing,
    ] {
        assert!(phases.contains(&p), "missing {p:?} in {phases:?}");
    }
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("updates"))
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "download dir must be clean after handoff: {leftovers:?}"
    );
    assert!(
        !dir.path().join("app.AppImage.new").exists(),
        "staging file must be gone after the rename"
    );
}

#[tokio::test]
async fn disabled_build_never_touches_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let (u, seen) = updater(config("http://127.0.0.1:9", None, dir.path()));
    let s = u.check().await;
    assert_eq!((s.phase, s.enabled), (Phase::Disabled, false));
    assert!(
        seen.lock().unwrap().is_empty(),
        "no transition may be emitted"
    );
}

#[test]
fn a_release_build_asks_the_public_releases_repo_for_updates() {
    let url = reqwest::Url::parse(super::state::DEFAULT_MANIFEST_URL).expect("default url parses");
    assert_eq!(url.host_str(), Some("github.com"));
    assert_eq!(
        url.path(),
        "/torkiv-labs/txtcaster-releases/releases/latest/download/latest.json"
    );
}
