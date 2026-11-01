use std::path::{Path, PathBuf};

use crate::render::ffmpeg::{self, FfmpegError};
use crate::render::mp3::{
    measure_filter, measured_filter, parse_loudnorm_json, SAMPLE_RATE, TARGET_LUFS,
};

pub const STAGING_DIR: &str = ".staging";
pub const REFERENCE_FILENAME: &str = "reference.wav";
pub const MIN_SECONDS: f64 = 3.0;
pub const MAX_SECONDS: f64 = 12.0;

const HIGHPASS_HZ: u32 = 70;
const INPUT_SCAN_SECONDS: f64 = 120.0;
const TRIM_THRESHOLD_DB: &str = "-50dB";
const TRIM_KEEP_SILENCE_S: &str = "0.05";

#[derive(Debug)]
pub enum VoiceSampleError {
    ToolsUnavailable,
    Unreadable(String),
    TooShort(f64),
    Conditioning(String),
    Io(String),
}

impl std::fmt::Display for VoiceSampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VoiceSampleError::ToolsUnavailable => write!(
                f,
                "txtCaster needs the audio tools (FFmpeg) to prepare a recording, and they are \
                 not installed. Install FFmpeg and reopen this window."
            ),
            VoiceSampleError::Unreadable(name) => write!(
                f,
                "'{name}' is not audio txtCaster can read. Try a WAV, MP3, FLAC or M4A file."
            ),
            VoiceSampleError::TooShort(seconds) => write!(
                f,
                "That sample is only {seconds:.1} seconds of speech. Cloning needs at least \
                 {MIN_SECONDS:.0} seconds — read a full sentence or two."
            ),
            VoiceSampleError::Conditioning(msg) => {
                write!(f, "The recording could not be prepared: {msg}")
            }
            VoiceSampleError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<FfmpegError> for VoiceSampleError {
    fn from(err: FfmpegError) -> Self {
        match err {
            FfmpegError::NotFound => VoiceSampleError::ToolsUnavailable,
            other => VoiceSampleError::Conditioning(other.to_string()),
        }
    }
}

pub fn voices_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("voices")
}

pub fn staging_dir(data_dir: &Path) -> PathBuf {
    voices_dir(data_dir).join(STAGING_DIR)
}

pub fn staging_path(data_dir: &Path, sample_id: &str) -> PathBuf {
    staging_dir(data_dir).join(format!("{sample_id}.wav"))
}

pub fn profile_dir(data_dir: &Path, profile_id: &str) -> PathBuf {
    voices_dir(data_dir).join(profile_id)
}

pub fn reference_path(data_dir: &Path, profile_id: &str) -> PathBuf {
    profile_dir(data_dir, profile_id).join(REFERENCE_FILENAME)
}

/// ffmpeg auto-probes its input, and several demuxers it can select (hls, dash, ffconcat,
/// sdp, image2) treat the file as a *playlist* that references further URLs. Handing it an
/// arbitrary user-chosen path therefore means an `.m3u8` renamed to `.wav` can splice other
/// local files — or, depending on the ffmpeg build's protocol defaults, remote ones — into
/// the reference recording, which then becomes fetchable through the media server. The
/// container is checked here, before ffmpeg sees the file, and the trim stage additionally
/// pins `-protocol_whitelist file`.
pub fn container_is_supported(head: &[u8]) -> bool {
    let starts = |prefix: &[u8]| head.len() >= prefix.len() && &head[..prefix.len()] == prefix;
    if starts(b"RIFF") && head.len() >= 12 && &head[8..12] == b"WAVE" {
        return true;
    }
    if starts(b"fLaC") || starts(b"OggS") || starts(b"ID3") {
        return true;
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return true;
    }
    if head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0 {
        return true;
    }
    false
}

pub fn read_head(path: &Path) -> Result<Vec<u8>, VoiceSampleError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|err| VoiceSampleError::Io(format!("could not open the file: {err}")))?;
    let mut head = vec![0u8; 16];
    let read = file
        .read(&mut head)
        .map_err(|err| VoiceSampleError::Io(format!("could not read the file: {err}")))?;
    head.truncate(read);
    Ok(head)
}

pub fn is_sample_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Deleting a profile removes its directory recursively, and the id that names that directory
/// arrives from the caller. Today an exact row match stands between the two, so nothing escapes;
/// this keeps that true if an id ever reaches the table by another route. Refusing `.` also puts
/// the staging directory out of reach, since it is the one sibling that is not a profile.
pub fn is_profile_id(value: &str) -> bool {
    is_sample_id(value)
}

fn trim_filter() -> String {
    let one = format!(
        "silenceremove=start_periods=1:start_silence={TRIM_KEEP_SILENCE_S}:\
         start_threshold={TRIM_THRESHOLD_DB}:detection=peak"
    );
    format!("{one},areverse,{one},areverse")
}

pub fn conditioning_prefix() -> String {
    format!("highpass=f={HIGHPASS_HZ},{}", trim_filter())
}

fn own(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

pub fn trim_args(source: &str, trimmed: &str) -> Vec<String> {
    own(&[
        "-hide_banner",
        "-nostdin",
        "-y",
        "-protocol_whitelist",
        "file",
        "-t",
        &INPUT_SCAN_SECONDS.to_string(),
        "-i",
        source,
        "-af",
        &conditioning_prefix(),
        "-ar",
        &SAMPLE_RATE.to_string(),
        "-ac",
        "1",
        "-c:a",
        "pcm_s16le",
        trimmed,
    ])
}

pub fn measure_args(trimmed: &str) -> Vec<String> {
    own(&[
        "-hide_banner",
        "-nostdin",
        "-i",
        trimmed,
        "-t",
        &MAX_SECONDS.to_string(),
        "-af",
        &measure_filter(),
        "-f",
        "null",
        "-",
    ])
}

pub fn encode_args(trimmed: &str, target: &str, filter: &str) -> Vec<String> {
    own(&[
        "-hide_banner",
        "-nostdin",
        "-y",
        "-i",
        trimmed,
        "-t",
        &MAX_SECONDS.to_string(),
        "-af",
        filter,
        "-ar",
        &SAMPLE_RATE.to_string(),
        "-ac",
        "1",
        "-c:a",
        "pcm_s16le",
        target,
    ])
}

fn borrow(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

pub fn wav_seconds(path: &Path) -> Result<f64, VoiceSampleError> {
    let bytes = std::fs::read(path)
        .map_err(|err| VoiceSampleError::Unreadable(format!("{}: {err}", path.display())))?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(VoiceSampleError::Unreadable(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ));
    }
    let mut cursor = 12usize;
    let mut byte_rate: Option<u32> = None;
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        let body = cursor + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            byte_rate = Some(u32::from_le_bytes([
                bytes[body + 8],
                bytes[body + 9],
                bytes[body + 10],
                bytes[body + 11],
            ]));
        }
        if id == b"data" {
            let rate = byte_rate.filter(|rate| *rate > 0).ok_or_else(|| {
                VoiceSampleError::Unreadable("the file declares no sample rate".into())
            })?;
            let available = bytes.len().saturating_sub(body);
            return Ok(size.min(available) as f64 / f64::from(rate));
        }
        cursor = body + size + (size % 2);
    }
    Err(VoiceSampleError::Unreadable(
        "the file has no audio data".into(),
    ))
}

pub async fn condition_into(source: &Path, target: &Path) -> Result<f64, VoiceSampleError> {
    let parent = target
        .parent()
        .ok_or_else(|| VoiceSampleError::Io("the staging path has no directory".into()))?;
    std::fs::create_dir_all(parent).map_err(|err| {
        VoiceSampleError::Io(format!("could not create {}: {err}", parent.display()))
    })?;
    let trimmed = parent.join(format!(
        "{}.trim.wav",
        target
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sample".into())
    ));
    if !container_is_supported(&read_head(source)?) {
        return Err(VoiceSampleError::Unreadable(
            source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| source.display().to_string()),
        ));
    }
    let outcome = condition_steps(source, &trimmed, target).await;
    let _ = std::fs::remove_file(&trimmed);
    if outcome.is_err() {
        let _ = std::fs::remove_file(target);
    }
    outcome
}

async fn condition_steps(
    source: &Path,
    trimmed: &Path,
    target: &Path,
) -> Result<f64, VoiceSampleError> {
    let source_arg = source.to_string_lossy().to_string();
    let trimmed_arg = trimmed.to_string_lossy().to_string();
    let target_arg = target.to_string_lossy().to_string();

    // areverse and loudnorm in one graph trip an ffmpeg 7.1 filter-scheduler assertion
    // (`best_input >= 0`, fftools/ffmpeg_filter.c) whose reachability depends on the input
    // length, and which can leave a truncated file behind while ffmpeg still exits 0. The
    // trim and the loudness passes therefore run as separate invocations; the disjointness
    // is pinned by no_stage_mixes_a_reversing_filter_with_loudnorm.
    let cut = ffmpeg::run(&borrow(&trim_args(&source_arg, &trimmed_arg))).await?;
    if !cut.status.success() {
        return Err(VoiceSampleError::Unreadable(
            source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| source.display().to_string()),
        ));
    }

    let pass1 = ffmpeg::run(&borrow(&measure_args(&trimmed_arg))).await?;
    if !pass1.status.success() {
        return Err(VoiceSampleError::Conditioning(ffmpeg::stderr_tail(&pass1)));
    }
    let measurement = parse_loudnorm_json(&String::from_utf8_lossy(&pass1.stderr))
        .ok_or_else(|| VoiceSampleError::Conditioning("loudness measurement missing".into()))?;

    let filter = measured_filter(&measurement);
    let pass2 = ffmpeg::run(&borrow(&encode_args(&trimmed_arg, &target_arg, &filter))).await?;
    if !pass2.status.success() {
        return Err(VoiceSampleError::Conditioning(ffmpeg::stderr_tail(&pass2)));
    }

    let seconds = wav_seconds(target)?;
    if seconds < MIN_SECONDS {
        return Err(VoiceSampleError::TooShort(seconds));
    }
    Ok(seconds)
}

pub fn sweep_staging(data_dir: &Path) -> usize {
    let dir = staging_dir(data_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn loudness_target() -> f64 {
    TARGET_LUFS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(seconds: f64, rate: u32) -> Vec<u8> {
        let frames = (seconds * f64::from(rate)) as u32;
        let data_len = frames * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend(vec![0u8; data_len as usize]);
        out
    }

    fn tempdir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("txtcaster-voice-{}", crate::storage::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn staging_lives_beside_the_profiles_it_becomes() {
        let data = Path::new("/data");
        assert_eq!(staging_dir(data), Path::new("/data/voices/.staging"));
        assert_eq!(
            staging_path(data, "abc").parent().unwrap().parent(),
            reference_path(data, "profile-1").parent().unwrap().parent()
        );
    }

    #[test]
    fn reference_path_matches_the_documented_layout() {
        assert_eq!(
            reference_path(Path::new("/data"), "voice-42"),
            Path::new("/data/voices/voice-42/reference.wav")
        );
    }

    #[test]
    fn sample_ids_reject_anything_that_could_escape_the_staging_dir() {
        assert!(is_sample_id("0198f0c2-1a2b-7c3d-8e4f-5a6b7c8d9e0f"));
        for bad in ["", "..", "../etc", "a/b", "a\\b", "a b", &"x".repeat(65)] {
            assert!(!is_sample_id(bad), "accepted: {bad}");
        }
    }

    #[test]
    fn wav_seconds_reads_the_declared_duration() {
        let dir = tempdir();
        let path = dir.join("a.wav");
        std::fs::write(&path, wav(5.0, 24_000)).unwrap();
        let seconds = wav_seconds(&path).unwrap();
        assert!((seconds - 5.0).abs() < 0.01, "{seconds}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wav_seconds_rejects_a_file_that_is_not_a_riff() {
        let dir = tempdir();
        let path = dir.join("a.wav");
        std::fs::write(&path, b"this is not audio").unwrap();
        assert!(matches!(
            wav_seconds(&path),
            Err(VoiceSampleError::Unreadable(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wav_seconds_uses_the_bytes_present_when_the_header_overstates_them() {
        let dir = tempdir();
        let path = dir.join("a.wav");
        let mut bytes = wav(5.0, 24_000);
        bytes.truncate(44 + 24_000);
        std::fs::write(&path, bytes).unwrap();
        let seconds = wav_seconds(&path).unwrap();
        assert!((seconds - 0.5).abs() < 0.01, "{seconds}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn filter_of(args: &[String]) -> String {
        let index = args.iter().position(|arg| arg == "-af").expect("-af");
        args[index + 1].clone()
    }

    #[test]
    fn no_stage_mixes_a_reversing_filter_with_loudnorm() {
        let stages = [
            trim_args("in.wav", "trim.wav"),
            measure_args("trim.wav"),
            encode_args("trim.wav", "out.wav", "loudnorm=I=-19:measured_I=-30"),
        ];
        for stage in &stages {
            let filter = filter_of(stage);
            assert!(
                !(filter.contains("areverse") && filter.contains("loudnorm")),
                "ffmpeg 7.1 asserts (best_input >= 0) when areverse and loudnorm share a \
                 graph, and can leave a truncated file behind while still exiting 0: {filter}"
            );
        }
    }

    #[test]
    fn the_loudness_stages_read_the_already_trimmed_file_and_cap_the_window() {
        let measure = measure_args("trim.wav");
        let encode = encode_args("trim.wav", "out.wav", "loudnorm=I=-19");
        for stage in [&measure, &encode] {
            let index = stage.iter().position(|arg| arg == "-t").expect("-t");
            assert_eq!(stage[index + 1], MAX_SECONDS.to_string());
            let input = stage.iter().position(|arg| arg == "-i").expect("-i");
            assert_eq!(stage[input + 1], "trim.wav");
        }
    }

    #[test]
    fn the_trim_stage_bounds_how_much_of_a_long_import_it_decodes() {
        let args = trim_args("in.wav", "trim.wav");
        let index = args.iter().position(|arg| arg == "-t").expect("-t");
        assert_eq!(args[index + 1], INPUT_SCAN_SECONDS.to_string());
        assert!(args.iter().position(|a| a == "-t") < args.iter().position(|a| a == "-i"));
    }

    #[test]
    fn every_stage_writes_the_mono_24k_pcm_the_model_expects() {
        for args in [
            trim_args("in.wav", "trim.wav"),
            encode_args("trim.wav", "out.wav", "loudnorm=I=-19"),
        ] {
            assert!(args.windows(2).any(|w| w[0] == "-ar" && w[1] == "24000"));
            assert!(args.windows(2).any(|w| w[0] == "-ac" && w[1] == "1"));
            assert!(args
                .windows(2)
                .any(|w| w[0] == "-c:a" && w[1] == "pcm_s16le"));
        }
    }

    #[test]
    fn a_directory_in_staging_survives_a_sweep_and_is_not_counted() {
        let dir = tempdir();
        let staging = staging_dir(&dir);
        std::fs::create_dir_all(staging.join("nested")).unwrap();
        std::fs::write(staging.join("nested").join("keep.wav"), b"x").unwrap();
        std::fs::write(staging.join("a.wav"), b"y").unwrap();
        assert_eq!(sweep_staging(&dir), 1);
        assert!(staging.join("nested").join("keep.wav").is_file());
        assert!(!staging.join("a.wav").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_profile_id_can_never_name_a_directory_outside_the_voices_folder() {
        for hostile in [
            "..",
            "../..",
            "a/../..",
            STAGING_DIR,
            "",
            "a/b",
            "a\\b",
            "a.b",
            "~",
        ] {
            assert!(
                !is_profile_id(hostile),
                "'{hostile}' would be handed to a recursive delete"
            );
        }
        assert!(is_profile_id("01J9Z2K3-ab_CD"));
    }

    #[test]
    fn the_trim_stage_refuses_every_protocol_except_local_files() {
        let args = trim_args("in.wav", "trim.wav");
        let index = args
            .iter()
            .position(|arg| arg == "-protocol_whitelist")
            .expect("the input protocol must be pinned");
        assert_eq!(args[index + 1], "file");
        assert!(index < args.iter().position(|a| a == "-i").expect("-i"));
    }

    #[test]
    fn only_the_containers_the_picker_offers_reach_ffmpeg() {
        assert!(container_is_supported(b"RIFF\x00\x00\x00\x00WAVEfmt "));
        assert!(container_is_supported(b"fLaC\x00\x00\x00\x22"));
        assert!(container_is_supported(b"OggS\x00\x02\x00\x00"));
        assert!(container_is_supported(b"ID3\x04\x00\x00\x00\x00\x00"));
        assert!(container_is_supported(b"\x00\x00\x00\x20ftypM4A "));
        assert!(container_is_supported(b"\xff\xfb\x90\x00"));
    }

    #[test]
    fn a_playlist_pretending_to_be_audio_is_refused_before_ffmpeg_probes_it() {
        for hostile in [
            &b"#EXTM3U\n#EXT-X-VERSION:3\n"[..],
            &b"ffconcat version 1.0\nfile '/etc/passwd'"[..],
            &b"v=0\no=- 0 0 IN IP4 127.0.0.1"[..],
            &b"<?xml version=\"1.0\"?><MPD>"[..],
            &b"#EXTINF"[..],
            &b""[..],
            &b"RIFF\x00\x00\x00\x00AVI "[..],
        ] {
            assert!(
                !container_is_supported(hostile),
                "ffmpeg would have probed this as a playlist: {:?}",
                String::from_utf8_lossy(hostile)
            );
        }
    }

    #[test]
    fn the_conditioning_chain_highpasses_and_trims_but_never_denoises() {
        let prefix = conditioning_prefix();
        assert!(prefix.starts_with("highpass=f=70,"));
        assert!(prefix.contains("silenceremove"));
        assert!(prefix.contains("areverse"));
        assert!(
            !prefix.contains("afftdn"),
            "afftdn measurably lowered clone similarity at 20 dB and 12 dB SNR across two \
             seeds; see the C4 PR body. Do not add it back without new measurements."
        );
    }

    #[test]
    fn sweeping_staging_removes_every_orphan_and_tolerates_a_missing_dir() {
        let dir = tempdir();
        assert_eq!(sweep_staging(&dir), 0);
        let staging = staging_dir(&dir);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("a.wav"), b"x").unwrap();
        std::fs::write(staging.join("b.wav"), b"y").unwrap();
        assert_eq!(sweep_staging(&dir), 2);
        assert_eq!(sweep_staging(&dir), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sweeping_staging_never_touches_a_saved_profile() {
        let dir = tempdir();
        let saved = reference_path(&dir, "voice-1");
        std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
        std::fs::write(&saved, b"kept").unwrap();
        std::fs::create_dir_all(staging_dir(&dir)).unwrap();
        std::fs::write(staging_dir(&dir).join("a.wav"), b"x").unwrap();
        assert_eq!(sweep_staging(&dir), 1);
        assert!(saved.is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_saved_reference_shares_a_parent_with_staging_so_the_move_cannot_cross_filesystems() {
        let data = Path::new("/data");
        let from = staging_path(data, "abc");
        let to = reference_path(data, "voice-1");
        assert_eq!(
            from.parent().unwrap().parent(),
            to.parent().unwrap().parent()
        );
    }

    #[test]
    fn loudness_target_matches_the_render_pipeline() {
        assert_eq!(loudness_target(), TARGET_LUFS);
    }
}
