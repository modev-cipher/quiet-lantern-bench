use txtcaster_lib::render::ffmpeg;

#[tokio::test]
async fn a_missing_encoder_is_reported_before_any_cast_work_starts() {
    let missing = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TXTCASTER_FFMPEG", missing.path().join("no-such-ffmpeg"));
    let refused = ffmpeg::ensure_available().await;
    assert!(
        matches!(refused, Err(ffmpeg::FfmpegError::NotFound)),
        "{refused:?}"
    );

    std::env::remove_var("TXTCASTER_FFMPEG");
    std::env::set_var("TXTCASTER_FFPROBE", missing.path().join("no-such-ffprobe"));
    let refused = ffmpeg::ensure_available().await;
    assert!(
        matches!(&refused, Err(ffmpeg::FfmpegError::Spawn(msg)) if msg.contains("ffprobe")),
        "a cast that cannot measure its chapters must be refused too: {refused:?}"
    );

    std::env::remove_var("TXTCASTER_FFPROBE");
    ffmpeg::ensure_available()
        .await
        .expect("the encoder on PATH is found once it is there");
    ffmpeg::ensure_available()
        .await
        .expect("and a found encoder stays found");
}
