//! Sabotage table (note §2): each row must be refused, and refusal must leave the
//! current install and the download dir untouched.

use std::sync::atomic::Ordering;

use super::signing::SigningKey;
use super::tests::{artifact, config, envelope, manifest, rig, updater, PLATFORM_ARTIFACT_KIND};
use super::{Phase, UpdateError};

fn old_binary_intact(dir: &std::path::Path) {
    assert_eq!(
        std::fs::read(dir.join("app.AppImage")).unwrap(),
        b"OLD BINARY"
    );
    let parts = std::fs::read_dir(dir.join("updates"))
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    assert_eq!(parts, 0, "no .part or artifact may remain");
}

#[tokio::test]
async fn tampered_manifest_payload_is_refused_before_parsing() {
    let (key, routes, base, dir) = rig(b"new").await;
    let mut env = routes.lock().unwrap().get("/latest.json").cloned().unwrap();
    let at = env.windows(10).position(|w| w == b"\"payload\":").unwrap() + 12;
    env[at] = if env[at] == b'A' { b'B' } else { b'A' };
    routes.lock().unwrap().insert("/latest.json".into(), env);
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.error_code),
        (Phase::Failed, Some("manifest_signature"))
    );
    assert!(s.available_version.is_none());
}

#[tokio::test]
async fn manifest_signed_by_another_key_is_refused() {
    let (_key, _routes, base, dir) = rig(b"new").await;
    let (_, other) = SigningKey::generate().unwrap();
    let (u, _) = updater(config(&base, Some(other.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.error_code),
        (Phase::Failed, Some("manifest_signature"))
    );
}

#[tokio::test]
async fn tampered_artifact_bytes_fail_the_digest_and_leave_the_install_alone() {
    let (key, routes, base, dir) = rig(b"genuine bytes").await;
    routes
        .lock()
        .unwrap()
        .insert("/txtcaster.AppImage".into(), b"evil bytes!!!".to_vec());
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    assert_eq!(u.check().await.phase, Phase::Available);
    assert!(matches!(u.install().await, Err(UpdateError::Digest { .. })));
    assert_eq!(u.snapshot().error_code, Some("digest"));
    old_binary_intact(dir.path());
}

#[tokio::test]
async fn artifact_with_a_forged_signature_is_refused_even_with_a_correct_digest() {
    let (key, routes, base, dir) = rig(b"payload").await;
    let (_, other) = SigningKey::generate().unwrap();
    let mut art = artifact(&other, format!("{base}/txtcaster.AppImage"), b"payload");
    art.kind = Some(PLATFORM_ARTIFACT_KIND.into());
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        envelope(&key, &manifest("0.2.0", Some(art))),
    );
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    u.check().await;
    assert_eq!(u.install().await, Err(UpdateError::ArtifactSignature));
    old_binary_intact(dir.path());
}

#[tokio::test]
async fn downgrade_and_same_version_are_up_to_date_never_available() {
    for offered in ["0.0.9", "0.1.0"] {
        let (key, routes, base, dir) = rig(b"x").await;
        let art = artifact(&key, format!("{base}/txtcaster.AppImage"), b"x");
        routes.lock().unwrap().insert(
            "/latest.json".into(),
            envelope(&key, &manifest(offered, Some(art))),
        );
        let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
        let s = u.check().await;
        assert_eq!(
            (s.phase, s.available_version),
            (Phase::UpToDate, None),
            "offered {offered}"
        );
        assert!(matches!(u.install().await, Err(UpdateError::Install(_))));
    }
}

#[tokio::test]
async fn pause_updates_shows_nothing_and_logs_once() {
    let (key, routes, base, dir) = rig(b"x").await;
    let mut m = manifest(
        "0.2.0",
        Some(artifact(&key, format!("{base}/txtcaster.AppImage"), b"x")),
    );
    m.pause_updates = true;
    routes
        .lock()
        .unwrap()
        .insert("/latest.json".into(), envelope(&key, &m));
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.available_version, s.error_code),
        (Phase::Paused, None, None)
    );
    assert!(s.last_checked_unix.is_some());
    assert!(u.paused_logged.load(Ordering::SeqCst));
    assert_eq!(u.check().await.phase, Phase::Paused);
}

#[tokio::test]
async fn offline_is_silent_and_leaves_no_error_or_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let (_, key) = SigningKey::generate().unwrap();
    let (u, _) = updater(config(
        "http://127.0.0.1:9",
        Some(key.public_key()),
        dir.path(),
    ));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.error_code, s.last_checked_unix),
        (Phase::Idle, None, None)
    );
    assert!(s.last_attempt_offline);
}

#[tokio::test]
async fn an_origin_with_nothing_published_is_not_a_failed_update() {
    let (key, routes, base, dir) = rig(b"never fetched").await;
    routes.lock().unwrap().clear();
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!((s.phase, s.error_code), (Phase::Idle, None));
    assert!(s.last_attempt_offline);
}

#[tokio::test]
async fn oversized_artifact_is_cut_off_at_the_signed_size() {
    let (key, routes, base, dir) = rig(b"tiny").await;
    routes
        .lock()
        .unwrap()
        .insert("/txtcaster.AppImage".into(), vec![1u8; 5000]);
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    u.check().await;
    assert_eq!(u.install().await, Err(UpdateError::SizeExceeded { cap: 4 }));
    old_binary_intact(dir.path());
}

#[tokio::test]
async fn cancel_is_silent_and_returns_to_available() {
    let body = vec![3u8; 50_000];
    let (key, routes, base, dir) = rig(&body).await;
    let art = artifact(&key, format!("{base}/slow.AppImage"), &body);
    routes
        .lock()
        .unwrap()
        .insert("/slow.AppImage".into(), body.clone());
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        envelope(&key, &manifest("0.2.0", Some(art))),
    );
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    let u = std::sync::Arc::new(u);
    u.check().await;
    let worker = {
        let u = u.clone();
        tokio::spawn(async move { u.install().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        u.snapshot().phase,
        Phase::Downloading,
        "cancel must land mid-download"
    );
    u.cancel();
    assert_eq!(worker.await.unwrap(), Err(UpdateError::Cancelled));
    assert_eq!(
        (u.snapshot().phase, u.snapshot().error_code),
        (Phase::Available, None)
    );
    old_binary_intact(dir.path());
}

#[tokio::test]
async fn plaintext_artifact_url_off_loopback_is_refused() {
    let (key, routes, base, dir) = rig(b"x").await;
    let mut art = artifact(&key, "http://example.com/txtcaster.AppImage".into(), b"x");
    art.kind = Some(PLATFORM_ARTIFACT_KIND.into());
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        envelope(&key, &manifest("0.2.0", Some(art))),
    );
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    u.check().await;
    assert!(matches!(
        u.install().await,
        Err(UpdateError::InsecureUrl(_))
    ));
    old_binary_intact(dir.path());
}

// ---- Review finding F1: the verified bytes are the installed bytes ----

async fn staged_for(dir: &std::path::Path) -> (super::download::Staged, std::path::PathBuf) {
    use super::download::stage_download;
    let (key, _routes, base, _rig_dir) = rig(b"verified payload").await;
    let art = artifact(
        &key,
        format!("{base}/txtcaster.AppImage"),
        b"verified payload",
    );
    let client = reqwest::Client::new();
    let staging = dir.join("app.AppImage.new");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let staged = stage_download(&client, &art, &staging, &key.public_key(), &cancel, |_| {})
        .await
        .expect("genuine download stages");
    (staged, staging)
}

#[tokio::test]
async fn staged_bytes_changed_after_verification_are_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app.AppImage");
    std::fs::write(&target, b"OLD BINARY").unwrap();
    let (staged, staging) = staged_for(dir.path()).await;
    // Attacker with a second handle flips the contents between verify and commit.
    std::fs::write(&staging, b"verified payloaX").unwrap();
    assert_eq!(
        staged.commit_rename(&target),
        Err(UpdateError::StagedTampered)
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"OLD BINARY");
}

#[tokio::test]
async fn a_file_moved_over_the_staging_path_after_verification_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app.AppImage");
    std::fs::write(&target, b"OLD BINARY").unwrap();
    let (staged, staging) = staged_for(dir.path()).await;
    // Same bytes, different inode: a substituted file must not ride the rename.
    let decoy = dir.path().join("decoy");
    std::fs::write(&decoy, b"verified payload").unwrap();
    std::fs::rename(&decoy, &staging).unwrap();
    assert_eq!(
        staged.commit_rename(&target),
        Err(UpdateError::StagedSubstituted)
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"OLD BINARY");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_preplaced_at_the_staging_path_is_refused_and_its_target_untouched() {
    use super::download::stage_download;
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"precious").unwrap();
    let staging = dir.path().join("app.AppImage.new");
    std::os::unix::fs::symlink(&victim, &staging).unwrap();
    let (key, _routes, base, _rig_dir) = rig(b"payload").await;
    let art = artifact(&key, format!("{base}/txtcaster.AppImage"), b"payload");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let err = stage_download(
        &reqwest::Client::new(),
        &art,
        &staging,
        &key.public_key(),
        &cancel,
        |_| {},
    )
    .await
    .err()
    .expect("must refuse");
    assert!(matches!(err, UpdateError::StagingSymlink(_)), "{err:?}");
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn installer_args_carry_our_argv_into_the_relaunch() {
    use super::install::installer_args;
    let none: Vec<std::ffi::OsString> = vec![];
    assert_eq!(
        installer_args(&none),
        ["/P", "/R", "/UPDATE"]
            .map(std::ffi::OsString::from)
            .to_vec()
    );
    let some = ["--open", "book.pdf"].map(std::ffi::OsString::from);
    let got = installer_args(&some);
    assert_eq!(got[3], std::ffi::OsString::from("/ARGS"));
    assert_eq!(got[4], std::ffi::OsString::from("--open book.pdf"));
    let spaced = [
        "--open",
        r"C:\My Books\a.pdf",
        r#"say "hi""#,
        r"C:\dir with\",
    ]
    .map(std::ffi::OsString::from);
    assert_eq!(
        installer_args(&spaced)[4],
        std::ffi::OsString::from(r#"--open "C:\My Books\a.pdf" "say \"hi\"" "C:\dir with\\""#)
    );
}

#[test]
fn build_metadata_never_makes_an_equal_precedence_release_installable() {
    use super::manifest::{compare, Offer};
    assert_eq!(compare("1.2.3+b1", "1.2.3+b2"), Ok(Offer::Same));
    assert_eq!(compare("1.2.3+b2", "1.2.3"), Ok(Offer::Same));
    assert_eq!(compare("1.2.3", "1.2.4+anything"), Ok(Offer::Newer));
    assert_eq!(compare("1.2.3", "1.2.3-rc.1+b9"), Ok(Offer::Older));
    assert_eq!(compare("1.2.3-rc.1", "1.2.3"), Ok(Offer::Newer));
}

// ---- Review finding F3: redirects only to the origin host (or GitHub asset hosts) over https ----

#[tokio::test]
async fn a_same_host_redirect_to_the_manifest_is_followed() {
    let (key, routes, base, dir) = rig(b"x").await;
    let real = routes.lock().unwrap().remove("/latest.json").unwrap();
    routes.lock().unwrap().insert("/real.json".into(), real);
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        format!("REDIRECT:{base}/real.json").into_bytes(),
    );
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    assert_eq!(u.check().await.phase, Phase::Available);
}

#[tokio::test]
async fn a_cross_host_redirect_is_refused_as_insecure_not_offline() {
    let (key, routes, base, dir) = rig(b"x").await;
    // Same loopback server, different host name: not the origin host → refused.
    let other = base.replace("127.0.0.1", "localhost");
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        format!("REDIRECT:{other}/latest.json").into_bytes(),
    );
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.error_code),
        (Phase::Failed, Some("insecure_url"))
    );
    assert!(!s.last_attempt_offline);
}

#[tokio::test]
async fn a_redirect_off_loopback_over_plain_http_is_refused() {
    let (key, routes, base, dir) = rig(b"x").await;
    routes.lock().unwrap().insert(
        "/latest.json".into(),
        b"REDIRECT:http://github.com/latest.json".to_vec(),
    );
    let (u, _) = updater(config(&base, Some(key.public_key()), dir.path()));
    let s = u.check().await;
    assert_eq!(
        (s.phase, s.error_code),
        (Phase::Failed, Some("insecure_url"))
    );
}
