use txtcaster_lib::media_server::{start, MediaServerError};

async fn server_with_root() -> (
    tempfile::TempDir,
    txtcaster_lib::media_server::MediaServerHandle,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let handle = start("test-token-123".into()).await.expect("start");
    (dir, handle)
}

fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, bytes).expect("write");
    path
}

#[tokio::test]
async fn serves_registered_file_with_ranges() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().to_path_buf();
    let path = write_file(&root, "exports/audio.mp3", b"0123456789");
    let url = server.register(&path, &[root]).expect("register");

    let client = reqwest::Client::new();
    let full = client.get(&url).send().await.expect("full");
    assert_eq!(full.status(), 200);
    assert_eq!(
        full.headers().get("accept-ranges").map(|v| v.as_bytes()),
        Some(b"bytes".as_slice())
    );
    assert_eq!(
        full.headers().get("content-type").map(|v| v.as_bytes()),
        Some(b"audio/mpeg".as_slice())
    );
    assert_eq!(full.bytes().await.expect("body").as_ref(), b"0123456789");

    let partial = client
        .get(&url)
        .header("Range", "bytes=2-5")
        .send()
        .await
        .expect("partial");
    assert_eq!(partial.status(), 206);
    assert_eq!(
        partial.headers().get("content-range").map(|v| v.as_bytes()),
        Some(b"bytes 2-5/10".as_slice())
    );
    assert_eq!(partial.bytes().await.expect("body").as_ref(), b"2345");

    let suffix = client
        .get(&url)
        .header("Range", "bytes=-3")
        .send()
        .await
        .expect("suffix");
    assert_eq!(suffix.status(), 206);
    assert_eq!(suffix.bytes().await.expect("body").as_ref(), b"789");

    let open_ended = client
        .get(&url)
        .header("Range", "bytes=7-")
        .send()
        .await
        .expect("open");
    assert_eq!(open_ended.status(), 206);
    assert_eq!(open_ended.bytes().await.expect("body").as_ref(), b"789");

    let unsatisfiable = client
        .get(&url)
        .header("Range", "bytes=50-60")
        .send()
        .await
        .expect("bad range");
    assert_eq!(unsatisfiable.status(), 416);
}

#[tokio::test]
async fn head_requests_report_length_without_body() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().to_path_buf();
    let path = write_file(&root, "cache/previews/p.wav", b"RIFFdata");
    let url = server.register(&path, &[root]).expect("register");

    let response = reqwest::Client::new()
        .head(&url)
        .send()
        .await
        .expect("head");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-length")
            .map(|v| v.as_bytes()),
        Some(b"8".as_slice())
    );
    assert_eq!(
        response.headers().get("content-type").map(|v| v.as_bytes()),
        Some(b"audio/wav".as_slice())
    );
    assert!(response.bytes().await.expect("body").is_empty());
}

#[tokio::test]
async fn rejects_missing_or_wrong_token() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().to_path_buf();
    let path = write_file(&root, "exports/a.mp3", b"x");
    let url = server.register(&path, &[root]).expect("register");

    let client = reqwest::Client::new();
    let no_token = url.split('?').next().expect("base url").to_string();
    assert_eq!(
        client.get(&no_token).send().await.expect("res").status(),
        401
    );
    let wrong = format!("{no_token}?t=wrong-token");
    assert_eq!(client.get(&wrong).send().await.expect("res").status(), 401);
}

#[tokio::test]
async fn unknown_id_is_not_found() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().to_path_buf();
    let path = write_file(&root, "exports/a.mp3", b"x");
    let url = server.register(&path, &[root]).expect("register");
    let bogus = url.replace(
        url.split("/media/")
            .nth(1)
            .expect("id")
            .split('?')
            .next()
            .expect("id"),
        "does-not-exist",
    );
    assert_eq!(
        reqwest::Client::new()
            .get(&bogus)
            .send()
            .await
            .expect("res")
            .status(),
        404
    );
}

#[tokio::test]
async fn registration_rejects_paths_outside_roots() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().join("exports");
    std::fs::create_dir_all(&root).expect("mkdir");
    let outside = write_file(dir.path(), "secret/creds.txt", b"nope");

    let err = server
        .register(&outside, std::slice::from_ref(&root))
        .expect_err("must reject");
    assert!(matches!(err, MediaServerError::OutsideRoots(_)));

    let traversal = root.join("../secret/creds.txt");
    let err = server
        .register(&traversal, &[root])
        .expect_err("must reject traversal");
    assert!(matches!(err, MediaServerError::OutsideRoots(_)));
}

#[tokio::test]
async fn registration_rejects_directories_and_missing_files() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().to_path_buf();
    let subdir = root.join("exports");
    std::fs::create_dir_all(&subdir).expect("mkdir");

    let err = server
        .register(&subdir, std::slice::from_ref(&root))
        .expect_err("must reject dir");
    assert!(matches!(err, MediaServerError::NotAFile(_)));

    let missing = root.join("exports/ghost.mp3");
    let err = server
        .register(&missing, &[root])
        .expect_err("must reject missing");
    assert!(matches!(err, MediaServerError::Canonicalize(_)));
}

#[tokio::test]
async fn same_path_registers_to_same_url() {
    let (dir, server) = server_with_root().await;
    let root = dir.path().to_path_buf();
    let path = write_file(&root, "exports/a.mp3", b"x");
    let first = server
        .register(&path, std::slice::from_ref(&root))
        .expect("first");
    let second = server.register(&path, &[root]).expect("second");
    assert_eq!(first, second);
}
