use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const SEGMENT_GAP_MS: i64 = 400;
pub const CHAPTER_GAP_MS: i64 = 1200;
pub const SAMPLE_RATE: u32 = 24_000;
pub const TARGET_LUFS: f64 = -19.0;
pub const TARGET_TP: f64 = -1.5;
pub const TARGET_LRA: f64 = 11.0;

#[derive(Debug, Clone, PartialEq)]
pub struct RenderSegment {
    pub segment_id: String,
    pub chapter_id: String,
    pub audio_hash: String,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SegmentSpan {
    pub segment_id: String,
    pub chapter_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineItem {
    Wav(PathBuf),
    SegmentGap,
    ChapterGap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChapterMarker {
    pub title: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub items: Vec<TimelineItem>,
    pub chapters: Vec<ChapterMarker>,
    pub segments: Vec<SegmentSpan>,
    pub total_ms: i64,
}

pub fn cache_wav_path(cache_dir: &Path, hash: &str) -> PathBuf {
    crate::cache_path::cache_path(cache_dir, hash)
}

pub fn is_valid_wav_artifact(cache_dir: &Path, hash: &str) -> bool {
    let path = cache_wav_path(cache_dir, hash);
    is_valid_wav_file(&path)
}

pub fn is_valid_wav_file(path: &Path) -> bool {
    for _ in 0..3 {
        match std::fs::metadata(path) {
            Ok(meta) => return meta.is_file() && meta.len() > 44,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    false
}

pub fn playable_segments(segments: &[crate::storage::job::ScriptSegment]) -> Vec<RenderSegment> {
    segments
        .iter()
        .filter_map(|segment| {
            let hash = segment.audio_hash.as_ref()?;
            let duration_ms = segment.duration_ms?;
            Some(RenderSegment {
                segment_id: segment.id.clone(),
                chapter_id: segment.chapter_id.clone(),
                audio_hash: hash.clone(),
                duration_ms,
            })
        })
        .collect()
}

pub fn build_timeline(
    segments: &[RenderSegment],
    cache_dir: &Path,
    chapter_title: impl Fn(&str) -> String,
) -> Timeline {
    let mut items = Vec::new();
    let mut chapters = Vec::new();
    let mut spans = Vec::new();
    let mut cursor: i64 = 0;
    let mut current: Option<(String, i64)> = None;

    for segment in segments {
        match &mut current {
            Some((chapter_id, _)) if *chapter_id == segment.chapter_id => {
                items.push(TimelineItem::SegmentGap);
                cursor += SEGMENT_GAP_MS;
            }
            Some((chapter_id, start_ms)) => {
                chapters.push(ChapterMarker {
                    title: chapter_title(chapter_id),
                    start_ms: *start_ms,
                    end_ms: cursor,
                });
                items.push(TimelineItem::ChapterGap);
                cursor += CHAPTER_GAP_MS;
                current = Some((segment.chapter_id.clone(), cursor));
            }
            None => {
                current = Some((segment.chapter_id.clone(), cursor));
            }
        }
        items.push(TimelineItem::Wav(cache_wav_path(
            cache_dir,
            &segment.audio_hash,
        )));
        spans.push(SegmentSpan {
            segment_id: segment.segment_id.clone(),
            chapter_id: segment.chapter_id.clone(),
            start_ms: cursor,
            end_ms: cursor + segment.duration_ms,
        });
        cursor += segment.duration_ms;
    }
    if let Some((chapter_id, start_ms)) = current {
        chapters.push(ChapterMarker {
            title: chapter_title(&chapter_id),
            start_ms,
            end_ms: cursor,
        });
    }
    Timeline {
        items,
        chapters,
        segments: spans,
        total_ms: cursor,
    }
}

pub fn concat_escape(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

pub fn concat_list(timeline: &Timeline, segment_gap_wav: &Path, chapter_gap_wav: &Path) -> String {
    let mut out = String::from("ffconcat version 1.0\n");
    for item in &timeline.items {
        let path = match item {
            TimelineItem::Wav(path) => path.as_path(),
            TimelineItem::SegmentGap => segment_gap_wav,
            TimelineItem::ChapterGap => chapter_gap_wav,
        };
        out.push_str(&format!("file '{}'\n", concat_escape(path)));
    }
    out
}

pub fn silence_wav(duration_ms: i64) -> Vec<u8> {
    let frames = (SAMPLE_RATE as i64 * duration_ms / 1000) as u32;
    let data_bytes = frames * 2;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    out.resize(44 + data_bytes as usize, 0);
    out
}

fn metadata_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() {
            out.push(' ');
            continue;
        }
        match ch {
            '=' | ';' | '#' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

pub fn ffmetadata(title: &str, author: Option<&str>, chapters: &[ChapterMarker]) -> String {
    let mut out = String::from(";FFMETADATA1\n");
    out.push_str(&format!("title={}\n", metadata_escape(title)));
    if let Some(author) = author {
        out.push_str(&format!("artist={}\n", metadata_escape(author)));
    }
    for chapter in chapters {
        out.push_str("[CHAPTER]\nTIMEBASE=1/1000\n");
        out.push_str(&format!("START={}\n", chapter.start_ms));
        out.push_str(&format!("END={}\n", chapter.end_ms));
        out.push_str(&format!("title={}\n", metadata_escape(&chapter.title)));
    }
    out
}

pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            other => other,
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_end_matches(['.', ' '])
        .trim_start()
        .to_string();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else if is_windows_device_name(&trimmed) {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

/// Windows resolves these to devices whatever the extension: `NUL.mp3` is not a file.
fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let digit = upper
                .strip_prefix("COM")
                .or_else(|| upper.strip_prefix("LPT"));
            matches!(digit, Some(d) if d.len() == 1 && matches!(d.as_bytes()[0], b'1'..=b'9'))
        }
    }
}

/// POSIX and NTFS both cap one path component at 255 bytes. The publisher
/// appends `.partial` (8 bytes) to this name before the final rename, so the
/// assembled name has to fit in the remaining 247 or the encode fails with
/// ENAMETOOLONG on the way to disk.
pub const EXPORT_NAME_BUDGET_BYTES: usize = 247;

/// Floor for the book title, derived from the budget rather than tuned: when a
/// scope label is too long the label truncates itself, so a chapter title lifted
/// from a PDF outline can never push the book's own title out of the name.
const MIN_BOOK_TITLE_BYTES: usize = 32;

const TAIL_BUDGET_BYTES: usize = EXPORT_NAME_BUDGET_BYTES - MIN_BOOK_TITLE_BYTES;

/// The one marker both the byte trim and the scope formatter use to say
/// "there is more here than you are seeing". Kept public so a label that
/// elides chapters and a label that runs out of bytes look the same to a user.
pub const TRUNCATION_MARKER: char = '\u{2026}';

/// Bytes the name spends that are neither the book title nor the scope label:
/// the ` - ` that joins them and the `.mp3` the publisher writes. Everything
/// else in the tail budget belongs to the label, and the label formatter caps
/// its chapter list at the same number so the two never disagree about who
/// truncates.
const SKELETON_BYTES: usize = " - .mp3".len();

/// Share of the name the scope label may claim. Exported because the scope
/// formatter has to cap its own list before it gets here: a label truncated
/// twice, once by the list cap and once by the byte budget, would show two
/// ellipses and read as two separate losses.
pub const SCOPE_LABEL_BUDGET_BYTES: usize = TAIL_BUDGET_BYTES.saturating_sub(SKELETON_BYTES);

fn truncate_on_char_boundary(value: &str, budget: usize) -> &str {
    let mut end = budget.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn marked_truncation(value: &str, budget: usize) -> String {
    if value.len() <= budget {
        return value.to_string();
    }
    let head =
        truncate_on_char_boundary(value, budget.saturating_sub(TRUNCATION_MARKER.len_utf8()));
    format!("{head}{TRUNCATION_MARKER}")
}

/// The user-facing half of an export path.
///
/// This is the *name*, not the identity. Two casts can legitimately produce the
/// same bytes here — the same book rendered with two voices, or the same title
/// on two books — because what tells them apart is the cast directory the name
/// is joined into (`orchestrator::render::cast_key`), which carries the whole
/// cast identity. Adding the voice or mode back into the name would duplicate
/// that identity for no user-facing gain: the Audios shelf reads the title and
/// the mode from the database row and never shows the voice
/// (`audioShelfModel.ts::buildAudioShelf`).
pub fn output_filename(book_title: &str, scope_label: &str) -> String {
    let tail = format!(
        " - {}.mp3",
        marked_truncation(scope_label, SCOPE_LABEL_BUDGET_BYTES)
    );
    let title_budget = EXPORT_NAME_BUDGET_BYTES.saturating_sub(tail.len());
    let title = truncate_on_char_boundary(book_title, title_budget);
    sanitize_filename(&format!("{title}{tail}"))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoudnormMeasurement {
    pub input_i: String,
    pub input_tp: String,
    pub input_lra: String,
    pub input_thresh: String,
    pub target_offset: String,
}

pub fn parse_loudnorm_json(stderr: &str) -> Option<LoudnormMeasurement> {
    let mut result = None;
    let mut search = stderr;
    while let Some(start) = search.find('{') {
        let candidate = &search[start..];
        if let Some(end) = candidate.find('}') {
            let block = &candidate[..=end];
            if let Ok(parsed) = serde_json::from_str::<LoudnormMeasurement>(block) {
                result = Some(parsed);
            }
            search = &candidate[end + 1..];
        } else {
            break;
        }
    }
    result
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinearGate {
    pub holds: bool,
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub projected_tp: f64,
}

pub const SENTINEL_TP: f64 = 99.0;
pub const SENTINEL_THRESH: f64 = -70.0;

pub fn linear_gate(measurement: &LoudnormMeasurement) -> Option<LinearGate> {
    let input_i = measurement.input_i.parse::<f64>().ok()?;
    let input_tp = measurement.input_tp.parse::<f64>().ok()?;
    let input_lra = measurement.input_lra.parse::<f64>().ok()?;
    let input_thresh = measurement.input_thresh.parse::<f64>().ok()?;
    if ![input_i, input_tp, input_lra, input_thresh]
        .iter()
        .all(|value| value.is_finite())
    {
        return None;
    }
    let projected_tp = input_tp + (TARGET_LUFS - input_i);
    let measurements_usable = input_tp != SENTINEL_TP
        && input_thresh != SENTINEL_THRESH
        && input_lra != 0.0
        && input_i != 0.0;
    Some(LinearGate {
        holds: measurements_usable && projected_tp <= TARGET_TP && input_lra <= TARGET_LRA,
        input_i,
        input_tp,
        input_lra,
        projected_tp,
    })
}

pub fn measured_filter(measurement: &LoudnormMeasurement) -> String {
    format!(
        "loudnorm=I={TARGET_LUFS}:TP={TARGET_TP}:LRA={TARGET_LRA}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
        measurement.input_i,
        measurement.input_tp,
        measurement.input_lra,
        measurement.input_thresh,
        measurement.target_offset
    )
}

pub fn measure_filter() -> String {
    format!("loudnorm=I={TARGET_LUFS}:TP={TARGET_TP}:LRA={TARGET_LRA}:print_format=json")
}

pub const CHAPTER_ENCODER_ARGS: &[&str] = &["-c:a", "libmp3lame", "-b:a", "128k", "-f", "mp3"];

pub fn chapter_encode_args<'a>(
    concat_list_path: &'a str,
    loudnorm_filter: &'a str,
    output_path: &'a str,
) -> Vec<&'a str> {
    let mut args = vec![
        "-hide_banner",
        "-nostdin",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        concat_list_path,
        "-af",
        loudnorm_filter,
    ];
    args.extend_from_slice(CHAPTER_ENCODER_ARGS);
    args.push(output_path);
    args
}

pub fn encoder_version_identity(version_output: &str) -> String {
    let mut ffmpeg = None;
    let mut libavcodec = None;
    for line in version_output.lines() {
        let line = line.trim();
        if ffmpeg.is_none() {
            if let Some(rest) = line.strip_prefix("ffmpeg version ") {
                ffmpeg = rest.split_whitespace().next().map(str::to_string);
            }
        }
        if libavcodec.is_none() {
            if let Some(rest) = line.strip_prefix("libavcodec") {
                let numbers: String = rest
                    .split('/')
                    .next()
                    .unwrap_or(rest)
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect();
                if !numbers.is_empty() {
                    libavcodec = Some(numbers);
                }
            }
        }
    }
    match (ffmpeg, libavcodec) {
        (Some(ffmpeg), Some(libavcodec)) => format!("ffmpeg{ffmpeg}+libavcodec{libavcodec}"),
        _ => {
            let mut hasher = Sha256::new();
            hasher.update(version_output.as_bytes());
            format!("raw:{}", hex::encode(hasher.finalize()))
        }
    }
}

pub fn encoder_signature(encoder_args: &[&str], version_identity: &str) -> String {
    format!(
        "{}|sr={SAMPLE_RATE}|{version_identity}",
        encoder_args.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(chapter: &str, hash: &str, ms: i64) -> RenderSegment {
        RenderSegment {
            segment_id: format!("seg-{hash}"),
            chapter_id: chapter.into(),
            audio_hash: hash.into(),
            duration_ms: ms,
        }
    }

    #[test]
    fn cache_path_uses_two_char_shard() {
        let path = cache_wav_path(Path::new("/cache"), "abcdef");
        assert_eq!(path, PathBuf::from("/cache/ab/abcdef"));
    }

    #[test]
    fn timeline_inserts_gaps_and_chapter_markers() {
        let segments = vec![
            seg("c1", "aa11", 1000),
            seg("c1", "bb22", 2000),
            seg("c2", "cc33", 500),
        ];
        let timeline = build_timeline(&segments, Path::new("/cache"), |id| format!("T {id}"));
        assert_eq!(
            timeline.items,
            vec![
                TimelineItem::Wav(PathBuf::from("/cache/aa/aa11")),
                TimelineItem::SegmentGap,
                TimelineItem::Wav(PathBuf::from("/cache/bb/bb22")),
                TimelineItem::ChapterGap,
                TimelineItem::Wav(PathBuf::from("/cache/cc/cc33")),
            ]
        );
        assert_eq!(
            timeline.chapters,
            vec![
                ChapterMarker {
                    title: "T c1".into(),
                    start_ms: 0,
                    end_ms: 3400,
                },
                ChapterMarker {
                    title: "T c2".into(),
                    start_ms: 4600,
                    end_ms: 5100,
                },
            ]
        );
        assert_eq!(timeline.total_ms, 5100);
    }

    #[test]
    fn timeline_empty_input_is_empty() {
        let timeline = build_timeline(&[], Path::new("/cache"), |_| String::new());
        assert!(timeline.items.is_empty());
        assert!(timeline.chapters.is_empty());
        assert_eq!(timeline.total_ms, 0);
    }

    #[test]
    fn concat_list_references_gap_files_and_escapes_quotes() {
        let segments = vec![seg("c1", "aa11", 1000), seg("c1", "bb22", 1000)];
        let timeline = build_timeline(&segments, Path::new("/ca'che"), |_| String::new());
        let text = concat_list(
            &timeline,
            Path::new("/tmp/gap400.wav"),
            Path::new("/tmp/gap1200.wav"),
        );
        let quoted = |shard: &str, name: &str| {
            Path::new("/ca'che")
                .join(shard)
                .join(name)
                .display()
                .to_string()
                .replace('\'', "'\\''")
        };
        assert_eq!(
            text,
            format!(
                "ffconcat version 1.0\nfile '{}'\nfile '/tmp/gap400.wav'\nfile '{}'\n",
                quoted("aa", "aa11"),
                quoted("bb", "bb22")
            )
        );
        assert!(
            text.contains("'/ca'\\''che"),
            "the quote inside the cache root is escaped"
        );
    }

    #[test]
    fn silence_wav_has_expected_size_and_header() {
        let wav = silence_wav(400);
        assert_eq!(wav.len(), 44 + 9600 * 2);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert!(wav[44..].iter().all(|b| *b == 0));
    }

    #[test]
    fn ffmetadata_writes_chapters_and_escapes() {
        let chapters = vec![ChapterMarker {
            title: "Intro; part #1 = start".into(),
            start_ms: 0,
            end_ms: 1500,
        }];
        let text = ffmetadata("My=Book", Some("Ada"), &chapters);
        assert_eq!(
            text,
            ";FFMETADATA1\ntitle=My\\=Book\nartist=Ada\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1500\ntitle=Intro\\; part \\#1 \\= start\n"
        );
    }

    #[test]
    fn filename_is_sanitized() {
        assert_eq!(output_filename("A/B: C?", "Ch 1-3"), "A_B_ C_ - Ch 1-3.mp3");
        assert_eq!(sanitize_filename("   "), "untitled");
        assert_eq!(sanitize_filename("name."), "name");
        assert_eq!(sanitize_filename("name . ."), "name");
    }

    #[test]
    fn windows_device_names_never_become_a_file_name() {
        for name in ["CON", "nul.mp3", "Com1", "LPT9.tar.gz", "aux .mp3"] {
            assert!(
                sanitize_filename(name).starts_with('_'),
                "{name} must not stay a device name"
            );
        }
        for name in [
            "Console",
            "COM0.mp3",
            "COM10",
            "Nullable.mp3",
            "CON - Chapter 1.mp3",
        ] {
            assert_eq!(sanitize_filename(name), name);
        }
    }

    /// The visible name is no longer what tells two casts apart, and it must not
    /// pretend to be: what separates them is the cast directory the name is
    /// joined into (`orchestrator::render::cast_key`), so two books that share a
    /// title now share a filename byte for byte and still cannot overwrite each
    /// other. Asserting that here — where the name is assembled — is what keeps
    /// it true; `render.rs`'s cast-identity tests prove the paths differ.
    #[test]
    fn the_visible_name_no_longer_carries_the_voice_the_mode_or_the_book_id() {
        assert_eq!(
            output_filename("Title", "Ch 1"),
            "Title - Ch 1.mp3",
            "the shelf reads the mode from the row and never shows the voice, so carrying \
             either here told the user nothing and only made the name longer"
        );
        let name = output_filename("Title", "Ch 1");
        assert!(!name.contains('['), "{name}");
        assert!(!name.ends_with(".mp3 ]"), "{name}");
    }

    #[test]
    fn ffmetadata_escapes_control_characters() {
        let chapters = vec![ChapterMarker {
            title: "Part 1\nSection 2\r\0".into(),
            start_ms: 0,
            end_ms: 1000,
        }];
        let text = ffmetadata("Book\nTitle\r", Some("Author\tName"), &chapters);
        assert!(text.contains("title=Book Title \n"));
        assert!(text.contains("artist=Author Name\n"));
        assert!(text.contains("title=Part 1 Section 2  \n"));
    }

    #[test]
    fn loudnorm_json_parses_last_block_from_stderr() {
        let stderr = "size=N/A time=00:00:05.10\n[Parsed_loudnorm_0 @ 0x55] \n{\n\t\"input_i\" : \"-27.61\",\n\t\"input_tp\" : \"-5.12\",\n\t\"input_lra\" : \"3.90\",\n\t\"input_thresh\" : \"-38.13\",\n\t\"output_i\" : \"-19.02\",\n\t\"output_tp\" : \"-1.50\",\n\t\"output_lra\" : \"3.40\",\n\t\"output_thresh\" : \"-29.51\",\n\t\"normalization_type\" : \"dynamic\",\n\t\"target_offset\" : \"0.02\"\n}\n";
        let parsed = parse_loudnorm_json(stderr).expect("measurement");
        assert_eq!(parsed.input_i, "-27.61");
        assert_eq!(parsed.target_offset, "0.02");
        let filter = measured_filter(&parsed);
        assert!(filter.contains("measured_I=-27.61"));
        assert!(filter.contains("linear=true"));
        assert!(filter.starts_with("loudnorm=I=-19:TP=-1.5:LRA=11:"));
    }

    const REAL_VERSION_OUTPUT: &str = "ffmpeg version 7.1.5 Copyright (c) 2000-2026 the FFmpeg developers\nbuilt with gcc 15 (GCC)\nconfiguration: --prefix=/usr --libdir=/usr/lib64 --enable-libmp3lame\nlibavutil      59. 39.100 / 59. 39.100\nlibavcodec     61. 19.101 / 61. 19.101\nlibavformat    61.  7.103 / 61.  7.103\n";

    #[test]
    fn the_signature_covers_exactly_the_encoder_arguments_the_encode_passes() {
        let args = chapter_encode_args("/w/list.txt", "loudnorm=I=-19", "/c/out.partial0");
        let tail = args[args.len() - CHAPTER_ENCODER_ARGS.len() - 1..args.len() - 1].to_vec();
        assert_eq!(
            tail, CHAPTER_ENCODER_ARGS,
            "the encode must pass the very flags the signature hashes, or the hash \
             stops identifying the bytes"
        );
        assert_eq!(args.last().copied(), Some("/c/out.partial0"));
        assert!(
            !encoder_signature(CHAPTER_ENCODER_ARGS, "ffmpeg7.1.5").contains("loudnorm"),
            "the measurement addresses the bytes and must never enter chapter identity"
        );
    }

    #[test]
    fn the_signature_changes_when_the_encode_arguments_change() {
        let identity = encoder_version_identity(REAL_VERSION_OUTPUT);
        assert_ne!(
            encoder_signature(CHAPTER_ENCODER_ARGS, &identity),
            encoder_signature(&["-c:a", "libmp3lame", "-q:a", "2", "-f", "mp3"], &identity)
        );
    }

    #[test]
    fn the_signature_changes_when_the_encoder_version_changes() {
        assert_ne!(
            encoder_signature(CHAPTER_ENCODER_ARGS, "ffmpeg7.1.5+libavcodec61.19.101"),
            encoder_signature(CHAPTER_ENCODER_ARGS, "ffmpeg8.0.0+libavcodec62.1.100")
        );
    }

    #[test]
    fn the_version_identity_reads_the_encoder_versions_and_ignores_the_build_configuration() {
        assert_eq!(
            encoder_version_identity(REAL_VERSION_OUTPUT),
            "ffmpeg7.1.5+libavcodec61.19.101"
        );
        let rebuilt = REAL_VERSION_OUTPUT.replace(
            "configuration: --prefix=/usr --libdir=/usr/lib64 --enable-libmp3lame",
            "configuration: --prefix=/opt/homebrew --enable-libmp3lame --enable-shared",
        );
        assert_eq!(
            encoder_version_identity(&rebuilt),
            encoder_version_identity(REAL_VERSION_OUTPUT),
            "a distro rebuild of the same versions must not invalidate every chapter artifact"
        );
    }

    #[test]
    fn an_unrecognised_banner_still_yields_a_signature_derived_from_the_binary() {
        let identity = encoder_version_identity("some vendor fork, no recognisable banner\n");
        assert!(identity.starts_with("raw:"));
        assert_ne!(
            identity,
            encoder_version_identity("a different vendor fork\n"),
            "an unparsable banner must still separate two different binaries"
        );
        assert_ne!(
            identity,
            encoder_version_identity(REAL_VERSION_OUTPUT),
            "the fallback must never collide with a parsed identity"
        );
    }

    #[test]
    fn loudnorm_parse_rejects_garbage() {
        assert!(parse_loudnorm_json("no json here").is_none());
        assert!(parse_loudnorm_json("{\"other\": 1}").is_none());
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn measurement(i: &str, tp: &str, lra: &str) -> LoudnormMeasurement {
        LoudnormMeasurement {
            input_i: i.into(),
            input_tp: tp.into(),
            input_lra: lra.into(),
            input_thresh: "-35.20".into(),
            target_offset: "0.02".into(),
        }
    }

    #[test]
    fn the_measured_production_book_reverts_to_dynamic_on_true_peak() {
        let gate = linear_gate(&measurement("-24.92", "-5.56", "1.70")).expect("parsed");
        assert!(gate.input_lra <= TARGET_LRA);
        assert!((gate.projected_tp - 0.36).abs() < 0.01);
        assert!(
            !gate.holds,
            "quiet narration needs +5.92 dB, which puts true peak above the target"
        );
    }

    #[test]
    fn a_zero_measured_lra_is_not_linear_because_ffmpeg_treats_it_as_unusable() {
        let gate = linear_gate(&measurement("-19.50", "-9.00", "0.00")).expect("parsed");
        assert!(gate.projected_tp <= TARGET_TP);
        assert!(gate.input_lra <= TARGET_LRA);
        assert!(
            !gate.holds,
            "af_loudnorm rejects measured_lra == 0, so claiming linear would stitch \
             per-chapter dynamic normalisation with -c copy"
        );
    }

    #[test]
    fn the_other_three_sentinels_also_force_dynamic() {
        let unusable_tp = LoudnormMeasurement {
            input_i: "-19.50".into(),
            input_tp: "99".into(),
            input_lra: "3.00".into(),
            input_thresh: "-35.20".into(),
            target_offset: "0.02".into(),
        };
        assert!(!linear_gate(&unusable_tp).expect("parsed").holds);

        let unusable_thresh = LoudnormMeasurement {
            input_i: "-19.50".into(),
            input_tp: "-9.00".into(),
            input_lra: "3.00".into(),
            input_thresh: "-70".into(),
            target_offset: "0.02".into(),
        };
        assert!(!linear_gate(&unusable_thresh).expect("parsed").holds);

        let unusable_i = LoudnormMeasurement {
            input_i: "0".into(),
            input_tp: "-9.00".into(),
            input_lra: "3.00".into(),
            input_thresh: "-35.20".into(),
            target_offset: "0.02".into(),
        };
        assert!(!linear_gate(&unusable_i).expect("parsed").holds);
    }

    #[test]
    fn the_target_offset_is_not_part_of_the_condition() {
        let mut wide_offset = measurement("-19.50", "-4.00", "3.10");
        wide_offset.target_offset = "9.00".into();
        assert_eq!(
            linear_gate(&wide_offset).expect("parsed").projected_tp,
            linear_gate(&measurement("-19.50", "-4.00", "3.10"))
                .expect("parsed")
                .projected_tp,
            "af_loudnorm computes offset_tp from measured_tp and the target only"
        );
    }

    #[test]
    fn the_true_peak_boundary_is_inclusive() {
        let exactly_at_target = measurement("-19.00", "-1.50", "3.00");
        let gate = linear_gate(&exactly_at_target).expect("parsed");
        assert_eq!(gate.projected_tp, TARGET_TP);
        assert!(
            gate.holds,
            "offset_tp <= target_tp is inclusive in the source"
        );

        let just_over = measurement("-19.00", "-1.49", "3.00");
        assert!(!linear_gate(&just_over).expect("parsed").holds);
    }

    #[test]
    fn the_loudness_range_boundary_is_inclusive() {
        let at_target = measurement("-19.00", "-9.00", "11.00");
        assert!(linear_gate(&at_target).expect("parsed").holds);
        let just_over = measurement("-19.00", "-9.00", "11.01");
        assert!(!linear_gate(&just_over).expect("parsed").holds);
    }

    #[test]
    fn a_wide_range_input_reverts_even_when_the_peak_fits() {
        let gate = linear_gate(&measurement("-19.00", "-9.00", "12.40")).expect("parsed");
        assert!(gate.projected_tp <= TARGET_TP);
        assert!(gate.input_lra > TARGET_LRA);
        assert!(!gate.holds);
    }

    #[test]
    fn a_loud_narrow_input_keeps_the_linear_path() {
        let gate = linear_gate(&measurement("-19.50", "-4.00", "3.10")).expect("parsed");
        assert!(gate.holds);
        assert!((gate.projected_tp - (-3.50)).abs() < 0.01);
    }

    #[test]
    fn an_unparsable_measurement_never_claims_the_linear_path() {
        assert!(linear_gate(&measurement("-inf", "-5.0", "2.0")).is_none());
        assert!(linear_gate(&measurement("nonsense", "-5.0", "2.0")).is_none());
    }

    #[test]
    fn a_very_long_title_is_trimmed_to_the_export_name_byte_budget() {
        let title = "a".repeat(300);
        let tail = " - Pages 1-8.mp3";
        let name = output_filename(&title, "Pages 1-8");
        assert!(
            name.len() <= 247,
            "the export name must fit the {}-byte budget with room for the .partial suffix, \
             got {} bytes",
            247,
            name.len()
        );
        let kept = name
            .strip_suffix(tail)
            .expect("the scope label must survive the trim");
        assert!(title.starts_with(kept));
        assert_eq!(
            kept.len(),
            247 - tail.len(),
            "the title must keep every byte of the budget it can"
        );
    }

    #[test]
    fn a_multibyte_title_is_trimmed_on_a_character_boundary() {
        let title = "كتاب".repeat(120);
        let tail = " - Pages 1-8.mp3";
        let name = output_filename(&title, "Pages 1-8");
        assert!(name.len() <= 247);
        let kept = name
            .strip_suffix(tail)
            .expect("the scope label must survive the trim");
        assert!(
            title.starts_with(kept),
            "the trimmed title must be a whole-character prefix, never a byte cut"
        );
        assert!(
            (247 - tail.len()) - kept.len() < 4,
            "the trim may only round down to the nearest character boundary, gave up {} bytes",
            (247 - tail.len()) - kept.len()
        );
    }

    fn assert_maximal_char_boundary_prefix(kept: &str, source: &str, allowance: usize) {
        assert!(
            source.starts_with(kept),
            "the kept part must be a whole-character prefix of the label: {kept:?}"
        );
        let next = source[kept.len()..]
            .chars()
            .next()
            .expect("the label continues past the cut");
        assert!(
            kept.len() + next.len_utf8() > allowance,
            "gave up {} bytes a character boundary did not force it to give up",
            allowance + next.len_utf8() - kept.len()
        );
    }

    #[test]
    fn an_unbounded_chapter_title_truncates_itself_and_keeps_the_book_title() {
        let chapter = "Chapter about ".repeat(34) + "an outrageously long outline entry";
        let name = output_filename("My Book", &chapter);
        assert!(
            name.len() <= EXPORT_NAME_BUDGET_BYTES,
            "the assembled name must fit the budget, got {} bytes",
            name.len()
        );
        assert!(name.ends_with(".mp3"), "the extension must survive: {name}");
        assert!(
            name.starts_with("My Book - "),
            "an enormous scope label must not silently discard the book title: {name}"
        );
        let shown = name
            .strip_prefix("My Book - ")
            .and_then(|rest| rest.strip_suffix(".mp3"))
            .expect("the surrounding tail must survive the truncation");
        assert!(
            shown.ends_with(TRUNCATION_MARKER),
            "the user must be able to see that the label was cut: {shown}"
        );
        assert!(
            shown.len() <= SCOPE_LABEL_BUDGET_BYTES,
            "the label must fit the share of the tail budget reserved for it, got {} bytes",
            shown.len()
        );
        assert_maximal_char_boundary_prefix(
            &shown[..shown.len() - TRUNCATION_MARKER.len_utf8()],
            &chapter,
            SCOPE_LABEL_BUDGET_BYTES - TRUNCATION_MARKER.len_utf8(),
        );
    }

    #[test]
    fn every_variable_part_stays_inside_the_budget() {
        let long_scope = "c".repeat(400);
        let multibyte_scope = "الفصل ".repeat(90);
        let cases: [(&str, &str); 5] = [
            ("Book", long_scope.as_str()),
            ("Book", multibyte_scope.as_str()),
            ("Book", "Ch 1-3"),
            ("", ""),
            ("", "Ch 1-3"),
        ];
        for (book_title, scope_label) in cases {
            let name = output_filename(book_title, scope_label);
            assert!(
                name.len() <= EXPORT_NAME_BUDGET_BYTES,
                "{name:?} is {} bytes",
                name.len()
            );
            assert!(
                name.ends_with(".mp3"),
                "sanitizing must not cost the extension: {name:?}"
            );
            assert!(
                !name.ends_with([' ', '.']),
                "sanitizing must not leave a name the publisher cannot use: {name:?}"
            );
        }
    }

    #[test]
    fn a_name_at_the_budget_can_actually_be_written_to_disk() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cast_dir = dir.path().join("0123456789abcdef");
        std::fs::create_dir(&cast_dir).expect("cast dir");
        let name = output_filename(
            &"a".repeat(300),
            &"A very long chapter title from a PDF outline ".repeat(6),
        );
        let partial_name = format!("{name}{}", crate::storage::export::PARTIAL_SUFFIX);
        let written = std::fs::write(cast_dir.join(&name), b"id3");
        assert!(
            written.is_ok(),
            "a {}-byte name must be creatable on this filesystem: {:?}",
            name.len(),
            written.err()
        );
        let partial_written = std::fs::write(cast_dir.join(&partial_name), b"id3");
        assert!(
            partial_written.is_ok(),
            "the encode writes the partial name first, inside the cast directory: {:?}",
            partial_written.err()
        );
        assert_eq!(
            name.len(),
            EXPORT_NAME_BUDGET_BYTES,
            "the title and the truncated tail together fill the budget exactly"
        );
        assert_eq!(
            partial_name.len(),
            EXPORT_NAME_BUDGET_BYTES + crate::storage::export::PARTIAL_SUFFIX.len(),
            "247 for the name and 8 for .partial is the 255 a path component allows"
        );
        assert!(
            std::fs::write(cast_dir.join("p".repeat(256)), b"").is_err(),
            "255 is a filesystem constant, not a Rust one: this platform must refuse 256 bytes"
        );
        assert!(cast_dir.join(&name).is_file());
        assert!(cast_dir.join(&partial_name).is_file());
    }

    #[test]
    fn a_multibyte_chapter_title_lands_on_a_character_boundary() {
        let chapter = "الفصل ".repeat(90);
        let name = output_filename("كتاب الكتاب", &chapter);
        assert!(name.len() <= EXPORT_NAME_BUDGET_BYTES);
        assert!(name.starts_with("كتاب الكتاب - "), "{name}");
        let shown = name
            .strip_prefix("كتاب الكتاب - ")
            .and_then(|rest| rest.strip_suffix(".mp3"))
            .expect("the surrounding tail must survive: {name}");
        assert!(shown.ends_with(TRUNCATION_MARKER), "{shown}");
        assert_maximal_char_boundary_prefix(
            &shown[..shown.len() - TRUNCATION_MARKER.len_utf8()],
            &chapter,
            SCOPE_LABEL_BUDGET_BYTES - TRUNCATION_MARKER.len_utf8(),
        );
    }

    /// The scope formatter caps its chapter list against this same constant. If
    /// the two ever disagree, a label shows one ellipsis for the elided chapters
    /// and another for the byte cut, which reads as two separate losses.
    #[test]
    fn the_exported_label_budget_is_the_one_the_name_actually_reserves() {
        let label = "c".repeat(SCOPE_LABEL_BUDGET_BYTES + 50);
        let name = output_filename("Book", &label);
        let shown = name
            .strip_prefix("Book - ")
            .and_then(|rest| rest.strip_suffix(".mp3"))
            .expect("tail");
        assert!(
            shown.len() <= SCOPE_LABEL_BUDGET_BYTES,
            "a label at the boundary must fit, got {} bytes for a {}-byte budget",
            shown.len(),
            SCOPE_LABEL_BUDGET_BYTES
        );
    }
}
