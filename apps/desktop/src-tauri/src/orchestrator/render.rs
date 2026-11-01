use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use futures_util::future::BoxFuture;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::stage::{ControlReceiver, Stage, StageContext, StageOutcome};
use super::state::{JobState, OrchestratorError, Scope};
use crate::render::ffmpeg::{self, FfmpegError};
use crate::render::mp3::{
    build_timeline, cache_wav_path, chapter_encode_args, concat_escape, concat_list,
    encoder_signature, encoder_version_identity, ffmetadata, linear_gate, measure_filter,
    measured_filter, output_filename, parse_loudnorm_json, silence_wav, RenderSegment, Timeline,
    CHAPTER_ENCODER_ARGS, CHAPTER_GAP_MS, SCOPE_LABEL_BUDGET_BYTES, SEGMENT_GAP_MS, TARGET_LRA,
    TARGET_TP, TRUNCATION_MARKER,
};
use crate::storage::{book, chapter_render, export, job};

pub struct RenderingStage {
    cache_dir: PathBuf,
    exports_dir: PathBuf,
}

impl RenderingStage {
    pub fn new(dirs: &crate::storage::paths::StorageDirs) -> Self {
        RenderingStage {
            cache_dir: dirs.cache_dir.clone(),
            exports_dir: dirs.exports_dir().to_path_buf(),
        }
    }

    pub fn exports_dir(&self) -> &Path {
        &self.exports_dir
    }
}

const COVER_SCALE_FILTER: &str =
    "scale=w='min(600,iw)':h='min(600,ih)':force_original_aspect_ratio=decrease";
const COVER_JPEG_QUALITY: &str = "4";

fn ffmpeg_error(err: FfmpegError) -> OrchestratorError {
    match err {
        FfmpegError::NotFound => OrchestratorError::FfmpegNotFound,
        other => OrchestratorError::Render(other.to_string()),
    }
}

enum FfmpegRun {
    Finished(Output),
    Interrupted(StageOutcome),
}

async fn run_ffmpeg(
    args: &[&str],
    control: &mut ControlReceiver,
) -> Result<FfmpegRun, OrchestratorError> {
    if let Some(outcome) = control.interruption() {
        return Ok(FfmpegRun::Interrupted(outcome));
    }
    let mut child = ffmpeg::spawn(args).await.map_err(ffmpeg_error)?;
    tokio::select! {
        finished = child.wait_with_output() => {
            let output = finished
                .map_err(|err| OrchestratorError::Render(format!("ffmpeg failed: {err}")))?;
            Ok(FfmpegRun::Finished(output))
        }
        outcome = control.interrupted() => Ok(FfmpegRun::Interrupted(outcome)),
    }
}

async fn ffmpeg_version_output() -> Result<String, OrchestratorError> {
    let output = ffmpeg::run(&["-hide_banner", "-version"])
        .await
        .map_err(ffmpeg_error)?;
    if !output.status.success() {
        return Err(OrchestratorError::Render(
            "ffmpeg -version returned a failure".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn stderr_tail(output: &Output) -> String {
    ffmpeg::stderr_tail(output)
}

/// Identity of one cast, and the name of the directory that holds it.
///
/// `export.path` is `UNIQUE` (`migrations/0006_export.sql`) and a collision is
/// not a near-miss: the publisher renames the new audio over the old path
/// *before* the row is written, so two casts sharing a path means one reader is
/// served the other one's bytes while its own row says fresh. The path therefore
/// has to carry everything that decides what the audio is, and the human-facing
/// name cannot: the book, the exact scope, the voice and the mode.
///
/// The scope is normalized first. Chapter ids are sorted and de-duplicated, so
/// `{a,b}` and `{b,a}` — the same selection the UI can emit in two orders — are
/// one cast and update in place rather than becoming two files that overwrite
/// each other. A page range is normalized as *ordered*, not swapped: the two
/// orders are two different spans, and folding them together would put two
/// different casts in one directory.
///
/// What is hashed is framed so that the digest input decodes back to the four
/// arguments. Every fixed-width field is written raw, after a fixed-width field
/// marker that is not derived from any input, and every variable-length field is
/// written as its own byte length — a fixed-width big-endian `u64` — and then its
/// bytes. The framing is what makes the choice safe: a chapter id is the one
/// field whose bytes are entirely free-form, so no delimiter can separate them,
/// because an id may contain any byte. Under a `\x1f` delimiter the selections
/// `[a, "b\x1fc"]` and `["a\x1fb", c]` were one byte stream, and a
/// `chapters:{n}` count does not help because both are the same length. The
/// count is kept, written as a fixed-width `u64`, because it is free; the length
/// prefixes are the part that carries the identity.
pub fn cast_key(book_id: &str, scope: &Scope, voice_profile_id: &str, mode: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"export_cast:v1");
    hasher.update(b"\x1e");
    hash_framed(&mut hasher, book_id.as_bytes());
    match scope {
        Scope::Pages {
            page_start,
            page_end,
        } => {
            hasher.update(b"\x1epages:");
            hasher.update(page_start.to_be_bytes());
            hasher.update(page_end.to_be_bytes());
        }
        Scope::Chapters { chapter_ids } => {
            let mut ids: Vec<&str> = chapter_ids.iter().map(String::as_str).collect();
            ids.sort_unstable();
            ids.dedup();
            hasher.update(b"\x1echapters:");
            hasher.update((ids.len() as u64).to_be_bytes());
            for id in ids {
                hash_framed(&mut hasher, id.as_bytes());
            }
        }
    }
    hasher.update(b"\x1evoice:");
    hash_framed(&mut hasher, voice_profile_id.as_bytes());
    hasher.update(b"\x1emode:");
    hash_framed(&mut hasher, mode.as_bytes());
    hex::encode(hasher.finalize())
}

/// One variable-length field of the `cast_key` digest input: its byte length as a
/// fixed-width big-endian `u64`, then the bytes. Framing by length rather than by
/// a delimiter is what keeps an id that contains the delimiter from being read as
/// a shorter id followed by another one.
fn hash_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// Label for a scope that names no chapter at all. `Scope::validate` rejects an
/// empty list, so this is only reachable from a hand-edited `scope_json` on a
/// resumed job, and it has to say so rather than print an empty list.
const NO_CHAPTER_SCOPE_LABEL: &str = "Ch none";

/// Label for a scope whose chapters have no rows in this book. The ids are
/// still in the cast key, so the cast is still distinct from every other one —
/// only the position in the book is unknowable, and `unknown` is the truth.
const UNRESOLVED_SCOPE_LABEL: &str = "Ch unknown";

fn scope_label(scope: &Scope, chapters: &HashMap<String, book::Chapter>) -> String {
    match scope {
        Scope::Pages {
            page_start,
            page_end,
        } => format!("Pages {page_start}-{page_end}"),
        Scope::Chapters { chapter_ids } => chapter_scope_label(chapter_ids, chapters),
    }
}

/// Human-readable chapter scope, built from where each chapter sits in the
/// book.
///
/// The number is `chapter.idx + 1`: `idx` is the engine's reading order and
/// starts at 0, so a book would otherwise read "Ch 0". The chapter *id* is an
/// opaque token and the chapter *title* is unbounded text lifted from a
/// document outline — neither belongs in a filename, and the title is what made
/// the old single-chapter name untrustworthy as identity.
///
/// A contiguous run collapses (`Ch 1-60`, not sixty numbers), because the
/// common case is "the whole book" and sixty numbers is not a name a person
/// reads. A selection too long for the budget is capped with a visible
/// remainder rather than cut: `Ch 3, 7, 12, …+8`. The cap is measured against
/// the same byte budget `output_filename` reserves for the label, so the two
/// never both truncate and the user never sees two ellipses.
fn chapter_scope_label(
    chapter_ids: &[String],
    chapters: &HashMap<String, book::Chapter>,
) -> String {
    if chapter_ids.is_empty() {
        return NO_CHAPTER_SCOPE_LABEL.to_string();
    }
    let mut ids: Vec<&str> = chapter_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();

    let mut numbers: Vec<i64> = ids
        .iter()
        .filter_map(|id| chapters.get(*id).map(|chapter| chapter.idx + 1))
        .filter(|number| *number > 0)
        .collect();
    if numbers.is_empty() {
        return UNRESOLVED_SCOPE_LABEL.to_string();
    }
    numbers.sort_unstable();
    numbers.dedup();

    let prefix = "Ch ";
    let reserved = remainder_suffix_len(ids.len());
    let mut shown: Vec<String> = Vec::new();
    let mut shown_ids = 0usize;
    let mut used = prefix.len();
    for (start, end) in chapter_runs(&numbers) {
        let item = chapter_run_label(start, end);
        let cost = item.len() + if shown.is_empty() { 0 } else { 2 };
        // The first item is always admitted: a run label is at most two
        // numbers wide, so it cannot crowd out the whole budget.
        if !shown.is_empty() && used + cost + reserved > SCOPE_LABEL_BUDGET_BYTES {
            break;
        }
        used += cost;
        shown_ids += (end - start + 1) as usize;
        shown.push(item);
    }
    let remainder = ids.len().saturating_sub(shown_ids);
    let mut label = String::with_capacity(used + reserved);
    label.push_str(prefix);
    label.push_str(&shown.join(", "));
    if remainder > 0 {
        label.push_str(&remainder_suffix(remainder));
    }
    label
}

fn chapter_runs(numbers: &[i64]) -> Vec<(i64, i64)> {
    let mut runs: Vec<(i64, i64)> = Vec::new();
    for &number in numbers {
        match runs.last_mut() {
            Some(last) if number == last.1 + 1 => last.1 = number,
            _ => runs.push((number, number)),
        }
    }
    runs
}

fn chapter_run_label(start: i64, end: i64) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// Bytes the elision marker costs for `remainder` unshown chapters: the `, `
/// that introduces it, the marker itself and the signed count. Counted at the
/// largest number it could ever be, so a list that stops early has already
/// paid for the suffix it is about to print.
fn remainder_suffix_len(remainder: usize) -> usize {
    remainder_suffix(remainder).len()
}

fn remainder_suffix(remainder: usize) -> String {
    format!(
        ", {TRUNCATION_MARKER}+{remainder}",
        TRUNCATION_MARKER = TRUNCATION_MARKER
    )
}

fn write_work_files(
    work_dir: &Path,
    timeline: &Timeline,
    title: &str,
    author: Option<&str>,
) -> Result<(PathBuf, PathBuf), std::io::Error> {
    let segment_gap = work_dir.join("gap_segment.wav");
    let chapter_gap = work_dir.join("gap_chapter.wav");
    std::fs::write(&segment_gap, silence_wav(SEGMENT_GAP_MS))?;
    std::fs::write(&chapter_gap, silence_wav(CHAPTER_GAP_MS))?;
    let list_path = work_dir.join("concat.txt");
    std::fs::write(
        &list_path,
        concat_list(timeline, &segment_gap, &chapter_gap),
    )?;
    let meta_path = work_dir.join("ffmetadata.txt");
    std::fs::write(&meta_path, ffmetadata(title, author, &timeline.chapters))?;
    Ok((list_path, meta_path))
}

async fn ffprobe_duration_ms(path: &Path) -> Result<i64, OrchestratorError> {
    let mut command = crate::render::ffmpeg::probe_command();
    command.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=nw=1:nk=1",
    ]);
    command.arg(path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = command
        .output()
        .await
        .map_err(|err| OrchestratorError::Render(format!("ffprobe failed: {err}")))?;
    if !output.status.success() {
        return Err(OrchestratorError::Render(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let seconds: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| OrchestratorError::Render("ffprobe returned no duration".into()))?;
    Ok((seconds * 1000.0).round() as i64)
}

fn mp3_packets_ms(packets: i64, sample_rate: i64) -> i64 {
    let samples_per_frame = if sample_rate < 32_000 { 576 } else { 1152 };
    (packets * samples_per_frame * 1000 + sample_rate / 2) / sample_rate
}

/// What a `-c copy` concat plays for this chapter: every packet, encoder delay included.
/// FFmpeg 8 subtracts that delay from `format=duration`, so the declared figure no longer
/// matches and chapter origins would drift by it, chapter after chapter.
pub async fn encoded_chapter_ms(path: &Path) -> Result<i64, OrchestratorError> {
    let mut command = crate::render::ffmpeg::probe_command();
    command.args([
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-count_packets",
        "-show_entries",
        "stream=nb_read_packets,sample_rate",
        "-of",
        "default=nw=1",
    ]);
    command.arg(path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = command
        .output()
        .await
        .map_err(|err| OrchestratorError::Render(format!("ffprobe failed: {err}")))?;
    if !output.status.success() {
        return Err(OrchestratorError::Render(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |name: &str| {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value.trim().parse::<i64>().ok())
    };
    match (field("nb_read_packets"), field("sample_rate")) {
        (Some(packets), Some(sample_rate)) if packets > 0 && sample_rate > 0 => {
            Ok(mp3_packets_ms(packets, sample_rate))
        }
        _ => Err(OrchestratorError::Render(
            "ffprobe returned no packet count".into(),
        )),
    }
}

fn prepend_gap(listing: &str, gap: &Path) -> String {
    let mut out = String::from("ffconcat version 1.0\n");
    out.push_str(&format!("file '{}'\n", concat_escape(gap)));
    for line in listing.lines().skip(1) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

const ARTIFACT_DURATION_TOLERANCE_MS: i64 = 100;
const CHAPTER_PROGRESS_START: f64 = 0.5;
const CHAPTER_PROGRESS_END: f64 = 0.9;

async fn artifact_is_usable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.len() == 0 {
        return false;
    }
    let Some(decoded) = decoded_duration_ms(path).await else {
        return false;
    };
    let Ok(declared) = ffprobe_duration_ms(path).await else {
        return false;
    };
    (declared - decoded).abs() <= ARTIFACT_DURATION_TOLERANCE_MS
}

async fn decoded_duration_ms(path: &Path) -> Option<i64> {
    let mut command = crate::render::ffmpeg::probe_command();
    command.args([
        "-v",
        "error",
        "-count_frames",
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=nb_read_frames,sample_rate",
        "-of",
        "default=nw=1",
    ]);
    command.arg(path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut frames = None;
    let mut sample_rate = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "nb_read_frames" => frames = value.trim().parse::<i64>().ok(),
            "sample_rate" => sample_rate = value.trim().parse::<f64>().ok(),
            _ => {}
        }
    }
    let frames = frames?;
    let sample_rate = sample_rate?;
    if frames <= 0 || sample_rate <= 0.0 {
        return None;
    }
    let samples_per_frame = if sample_rate < 32_000.0 {
        576.0
    } else {
        1152.0
    };
    Some((frames as f64 * samples_per_frame / sample_rate * 1000.0).round() as i64)
}

pub fn chapter_groups(
    playable: &[RenderSegment],
) -> Result<Vec<(String, Vec<RenderSegment>)>, OrchestratorError> {
    let mut groups: Vec<(String, Vec<RenderSegment>)> = Vec::new();
    for segment in playable {
        match groups.last_mut() {
            Some((chapter_id, items)) if *chapter_id == segment.chapter_id => {
                items.push(segment.clone())
            }
            _ => {
                if groups
                    .iter()
                    .any(|(chapter_id, _)| *chapter_id == segment.chapter_id)
                {
                    return Err(OrchestratorError::Render(format!(
                        "chapter {} is interleaved with another chapter in segment order, so it \
                         cannot be described by one chapter marker",
                        segment.chapter_id
                    )));
                }
                groups.push((segment.chapter_id.clone(), vec![segment.clone()]))
            }
        }
    }
    Ok(groups)
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodedChapter {
    pub chapter_id: String,
    pub encoded_ms: i64,
    pub leading_gap_ms: i64,
}

pub fn markers_from_encoded(
    chapters: &[EncodedChapter],
    title: impl Fn(&str) -> String,
) -> Vec<crate::render::mp3::ChapterMarker> {
    let mut cursor = 0i64;
    let mut markers = Vec::new();
    for chapter in chapters {
        markers.push(crate::render::mp3::ChapterMarker {
            title: title(&chapter.chapter_id),
            start_ms: cursor + chapter.leading_gap_ms,
            end_ms: cursor + chapter.encoded_ms,
        });
        cursor += chapter.encoded_ms;
    }
    markers
}

impl Stage for RenderingStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }

            let started = std::time::Instant::now();
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let book_row = book::get_book(&ctx.pool, &record.book_id)
                .await?
                .ok_or_else(|| OrchestratorError::BookNotFound(record.book_id.clone()))?;
            let chapters: HashMap<String, book::Chapter> =
                book::list_chapters(&ctx.pool, &record.book_id)
                    .await?
                    .into_iter()
                    .map(|c| (c.id.clone(), c))
                    .collect();
            let segments = job::list_segments(&ctx.pool, &ctx.job_id).await?;

            let playable = crate::render::mp3::playable_segments(&segments);
            let mut flagged = Vec::new();
            let mut flag_counts: std::collections::BTreeMap<String, u64> =
                std::collections::BTreeMap::new();
            for segment in &segments {
                let flags: Vec<String> = segment
                    .flags_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or_default();
                for flag in &flags {
                    *flag_counts.entry(flag.clone()).or_default() += 1;
                }
                if !flags.is_empty() {
                    flagged.push(json!({"segment_id": segment.id, "flags": flags}));
                }
            }
            if playable.is_empty() {
                return Err(OrchestratorError::InvalidRequest(
                    "job has no synthesized segments to render".into(),
                ));
            }

            let mut missing_segment_ids: Vec<&str> = Vec::new();
            let mut first_missing_path = String::new();
            for segment in &playable {
                if !crate::render::mp3::is_valid_wav_artifact(&self.cache_dir, &segment.audio_hash)
                {
                    if first_missing_path.is_empty() {
                        first_missing_path = crate::render::mp3::cache_wav_path(
                            &self.cache_dir,
                            &segment.audio_hash,
                        )
                        .display()
                        .to_string();
                    }
                    missing_segment_ids.push(segment.segment_id.as_str());
                }
            }
            if !missing_segment_ids.is_empty() {
                job::clear_segments_audio(&ctx.pool, &missing_segment_ids).await?;
                return Err(OrchestratorError::MissingAudioArtifact {
                    stage: JobState::Rendering,
                    count: missing_segment_ids.len(),
                    message: format!("cached wav missing: {first_missing_path}"),
                });
            }

            for segment in &playable {
                super::align::claim_wav(ctx, &segment.audio_hash, &self.cache_dir).await?;
            }

            let timeline = build_timeline(&playable, &self.cache_dir, |chapter_id| {
                chapters
                    .get(chapter_id)
                    .map(|c| c.title.clone())
                    .unwrap_or_else(|| chapter_id.to_string())
            });
            for item in &timeline.items {
                if let crate::render::mp3::TimelineItem::Wav(path) = item {
                    if !crate::render::mp3::is_valid_wav_file(path) {
                        return Err(OrchestratorError::MissingAudioArtifact {
                            stage: JobState::Rendering,
                            count: 1,
                            message: format!("cached wav missing: {}", path.display()),
                        });
                    }
                }
            }

            let work = tempfile::tempdir()
                .map_err(|err| OrchestratorError::Render(format!("tempdir failed: {err}")))?;
            let (list_path, meta_path) = write_work_files(
                work.path(),
                &timeline,
                &book_row.title,
                book_row.author.as_deref(),
            )
            .map_err(|err| OrchestratorError::Render(format!("write work files failed: {err}")))?;
            let list_arg = list_path.to_string_lossy().to_string();
            let meta_arg = meta_path.to_string_lossy().to_string();

            ctx.emit_progress(0.1, None, "measuring loudness");
            let measure = measure_filter();
            let pass1 = match run_ffmpeg(
                &[
                    "-hide_banner",
                    "-nostdin",
                    "-f",
                    "concat",
                    "-safe",
                    "0",
                    "-i",
                    &list_arg,
                    "-af",
                    &measure,
                    "-f",
                    "null",
                    "-",
                ],
                &mut ctx.control,
            )
            .await?
            {
                FfmpegRun::Finished(output) => output,
                FfmpegRun::Interrupted(outcome) => return Ok(outcome),
            };
            if !pass1.status.success() {
                return Err(OrchestratorError::Render(format!(
                    "loudness measurement failed: {}",
                    stderr_tail(&pass1)
                )));
            }
            let measurement = parse_loudnorm_json(&String::from_utf8_lossy(&pass1.stderr))
                .ok_or_else(|| {
                    OrchestratorError::Render("loudnorm measurement missing from ffmpeg".into())
                })?;
            let gate = linear_gate(&measurement);
            match &gate {
                Some(gate) => tracing::info!(
                    job = %ctx.job_id,
                    "loudnorm decision: linear={} input_i={} input_tp={} input_lra={} projected_tp={}",
                    gate.holds,
                    gate.input_i,
                    gate.input_tp,
                    gate.input_lra,
                    gate.projected_tp
                ),
                None => tracing::info!(
                    job = %ctx.job_id,
                    "loudnorm decision: linear=false (measurement did not gate)"
                ),
            }
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            ctx.checkpoint.done_chunks = 1;
            ctx.save_checkpoint().await?;
            ctx.emit_progress(CHAPTER_PROGRESS_START, None, "encoding mp3");

            let scope: Scope = serde_json::from_str(&record.scope_json)?;
            let cast_dir = self.exports_dir.join(cast_key(
                &record.book_id,
                &scope,
                &record.voice_profile_id,
                &record.mode,
            ));
            std::fs::create_dir_all(&cast_dir).map_err(|err| {
                OrchestratorError::Render(format!("export dir failed for this cast: {err}"))
            })?;
            let filename = output_filename(&book_row.title, &scope_label(&scope, &chapters));
            let output_path = cast_dir.join(&filename);
            let partial_path = export::partial_sibling(&output_path);
            export::remove_partial_within(&self.exports_dir, &output_path);
            let output_arg = output_path.to_string_lossy().to_string();
            let partial_arg = partial_path.to_string_lossy().to_string();
            let filter = measured_filter(&measurement);

            let encoder = encoder_signature(
                CHAPTER_ENCODER_ARGS,
                &encoder_version_identity(&ffmpeg_version_output().await?),
            );
            let linear = gate.as_ref().is_some_and(|gate| gate.holds);
            let groups = chapter_groups(&playable)?;
            let mut chapter_rows: Vec<chapter_render::ChapterRender> = Vec::new();
            let mut chapter_files: Vec<PathBuf> = Vec::new();
            let mut encoded_chapters: Vec<EncodedChapter> = Vec::new();
            for (position, (chapter_id, items)) in groups.iter().enumerate() {
                if let Some(outcome) = ctx.control.interruption() {
                    return Ok(outcome);
                }
                if linear {
                    let title = chapters
                        .get(chapter_id)
                        .map(|c| c.title.as_str())
                        .unwrap_or(chapter_id.as_str());
                    ctx.emit_progress(
                        CHAPTER_PROGRESS_START
                            + (CHAPTER_PROGRESS_END - CHAPTER_PROGRESS_START) * position as f64
                                / groups.len() as f64,
                        None,
                        &format!("encoding {title}"),
                    );
                }
                let audio_hashes: Vec<String> =
                    items.iter().map(|item| item.audio_hash.clone()).collect();
                let nominal: i64 = items.iter().map(|item| item.duration_ms).sum::<i64>()
                    + SEGMENT_GAP_MS * (items.len() as i64 - 1).max(0);
                let artifact = if linear {
                    chapter_render::ARTIFACT_MP3
                } else {
                    chapter_render::ARTIFACT_NONE
                };
                let leading_gap_ms = if position == 0 { 0 } else { CHAPTER_GAP_MS };
                let hash = chapter_render::chapter_hash(
                    &audio_hashes,
                    SEGMENT_GAP_MS,
                    leading_gap_ms,
                    &encoder,
                );
                let artifact_key = chapter_render::artifact_key(&hash, &filter);
                let mut encoded_ms = None;
                if linear {
                    let target = cache_wav_path(&self.cache_dir, &artifact_key);
                    let chapter_encode_started = std::time::Instant::now();
                    let cached = artifact_is_usable(&target).await;
                    if !cached {
                        let chapter_timeline = build_timeline(items, &self.cache_dir, |id| {
                            chapters
                                .get(id)
                                .map(|c| c.title.clone())
                                .unwrap_or_else(|| id.to_string())
                        });
                        let chapter_list = work.path().join(format!("chapter_{position}.txt"));
                        let mut listing = concat_list(
                            &chapter_timeline,
                            &work.path().join("gap_segment.wav"),
                            &work.path().join("gap_chapter.wav"),
                        );
                        if leading_gap_ms > 0 {
                            listing = prepend_gap(&listing, &work.path().join("gap_chapter.wav"));
                        }
                        std::fs::write(&chapter_list, listing).map_err(|err| {
                            OrchestratorError::Render(format!("chapter list failed: {err}"))
                        })?;
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).map_err(|err| {
                                OrchestratorError::Render(format!("cache dir failed: {err}"))
                            })?;
                        }
                        let chapter_arg = chapter_list.to_string_lossy().to_string();
                        let pending = target.with_extension(format!("partial{position}"));
                        let target_arg = pending.to_string_lossy().to_string();
                        let encode_args = chapter_encode_args(&chapter_arg, &filter, &target_arg);
                        let encode = match run_ffmpeg(&encode_args, &mut ctx.control).await? {
                            FfmpegRun::Finished(output) => output,
                            FfmpegRun::Interrupted(outcome) => {
                                let _ = std::fs::remove_file(&pending);
                                return Ok(outcome);
                            }
                        };
                        if !encode.status.success() {
                            let _ = std::fs::remove_file(&pending);
                            return Err(OrchestratorError::Render(format!(
                                "chapter encode failed: {}",
                                stderr_tail(&encode)
                            )));
                        }
                        if !artifact_is_usable(&pending).await {
                            let _ = std::fs::remove_file(&pending);
                            return Err(OrchestratorError::Render(
                                "chapter encode produced an unusable artifact".into(),
                            ));
                        }
                        std::fs::rename(&pending, &target).map_err(|err| {
                            OrchestratorError::Render(format!("chapter publish failed: {err}"))
                        })?;
                    }
                    let bytes = std::fs::metadata(&target)
                        .map(|m| m.len() as i64)
                        .unwrap_or(0);
                    crate::storage::cache::upsert_entry(
                        &ctx.pool,
                        &artifact_key,
                        "chapter_mp3",
                        bytes,
                    )
                    .await?;
                    let measured = encoded_chapter_ms(&target).await?;
                    tracing::debug!(
                        job = %ctx.job_id,
                        "chapter artifact: idx={position} cached={cached} encoded_ms={measured} \
                         bytes={bytes} encode_ms={}",
                        chapter_encode_started.elapsed().as_millis()
                    );
                    encoded_ms = Some(measured);
                    encoded_chapters.push(EncodedChapter {
                        chapter_id: chapter_id.clone(),
                        encoded_ms: measured,
                        leading_gap_ms,
                    });
                    chapter_files.push(target);
                }
                chapter_rows.push(chapter_render::ChapterRender {
                    job_id: ctx.job_id.clone(),
                    chapter_id: chapter_id.clone(),
                    idx: position as i64,
                    hash,
                    artifact: artifact.to_string(),
                    duration_ms: nominal,
                    encoded_ms,
                });
            }
            chapter_render::replace_all(&ctx.pool, &ctx.job_id, &chapter_rows).await?;
            if linear {
                let markers = markers_from_encoded(&encoded_chapters, |id| {
                    chapters
                        .get(id)
                        .map(|c| c.title.clone())
                        .unwrap_or_else(|| id.to_string())
                });
                std::fs::write(
                    &meta_path,
                    ffmetadata(&book_row.title, book_row.author.as_deref(), &markers),
                )
                .map_err(|err| {
                    OrchestratorError::Render(format!("chapter metadata failed: {err}"))
                })?;
                let mut listing = String::from("ffconcat version 1.0\n");
                for path in &chapter_files {
                    listing.push_str(&format!("file '{}'\n", concat_escape(path)));
                }
                std::fs::write(&list_path, listing).map_err(|err| {
                    OrchestratorError::Render(format!("chapter concat list failed: {err}"))
                })?;
            }

            let cover = book_row
                .cover_path
                .as_deref()
                .filter(|path| Path::new(path).is_file())
                .map(|path| path.to_string());
            let mut args: Vec<&str> = vec![
                "-hide_banner",
                "-nostdin",
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                &list_arg,
                "-i",
                &meta_arg,
            ];
            if let Some(cover_path) = cover.as_deref() {
                args.extend(["-i", cover_path]);
            }
            args.extend(["-map", "0:a", "-map_metadata", "1", "-map_chapters", "1"]);
            if cover.is_some() {
                args.extend([
                    "-map",
                    "2:0",
                    "-c:v",
                    "mjpeg",
                    "-vf",
                    COVER_SCALE_FILTER,
                    "-pix_fmt",
                    "yuvj420p",
                    "-q:v",
                    COVER_JPEG_QUALITY,
                    "-disposition:v:0",
                    "attached_pic",
                ]);
            }
            if linear {
                args.extend(["-c:a", "copy"]);
            } else {
                args.extend(["-af", &filter, "-c:a", "libmp3lame", "-b:a", "128k"]);
            }
            args.extend(["-id3v2_version", "3", "-f", "mp3", &partial_arg]);
            if !chapter_files.is_empty() {
                let mut needed: u64 = 0;
                for chapter in &chapter_files {
                    needed = needed.saturating_add(
                        std::fs::metadata(chapter)
                            .map(|meta| meta.len())
                            .unwrap_or(0),
                    );
                }
                needed = needed
                    .saturating_mul(5)
                    .saturating_div(4)
                    .saturating_add(64 * 1024 * 1024);
                let available = fs4::available_space(&self.exports_dir).unwrap_or(u64::MAX);
                if available < needed {
                    return Err(OrchestratorError::Render(format!(
                        "not enough disk space for the export: about {} MB needed, {} MB free; \
                         free space and start the job again",
                        needed / (1024 * 1024),
                        available / (1024 * 1024)
                    )));
                }
            }
            ctx.emit_progress(CHAPTER_PROGRESS_END, None, "writing mp3");
            let pass2 = match run_ffmpeg(&args, &mut ctx.control).await? {
                FfmpegRun::Finished(output) => output,
                FfmpegRun::Interrupted(outcome) => {
                    let _ = std::fs::remove_file(&partial_path);
                    return Ok(outcome);
                }
            };
            if !pass2.status.success() {
                let _ = std::fs::remove_file(&partial_path);
                return Err(OrchestratorError::Render(format!(
                    "mp3 encode failed: {}",
                    stderr_tail(&pass2)
                )));
            }
            if let Err(err) = std::fs::rename(&partial_path, &output_path) {
                let _ = std::fs::remove_file(&partial_path);
                return Err(OrchestratorError::Render(format!(
                    "export could not be published and the previous export is untouched; the \
                     target may be open in another application: {err}"
                )));
            }

            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }

            let latest = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let mut report: serde_json::Value = latest
                .report_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| json!({}));
            if !report["durations_s"].is_object() {
                report["durations_s"] = json!({});
            }
            report["durations_s"]["render"] = json!(started.elapsed().as_secs());
            report["flagged_segments"] = json!(flagged);
            report["flag_counts"] = json!(flag_counts);
            report["segment_count"] = json!(segments.len());
            report["plan_revisions"] = json!(ctx.checkpoint.resource_plan_rev);
            report["loudness"] = match &gate {
                Some(gate) => json!({
                    "linear": gate.holds,
                    "input_i": gate.input_i,
                    "input_tp": gate.input_tp,
                    "input_lra": gate.input_lra,
                    "projected_tp": gate.projected_tp,
                    "lra_target": TARGET_LRA,
                    "tp_target": TARGET_TP,
                    "chapter_artifacts": if gate.holds { "mp3" } else { "none" },
                }),
                None => json!({"linear": false, "chapter_artifacts": "none"}),
            };
            report["recovery_log"] = json!(ctx.checkpoint.recovery_log);
            let output_paths = json!([output_arg]);
            job::set_job_report(
                &ctx.pool,
                &ctx.job_id,
                &serde_json::to_string(&report)?,
                &serde_json::to_string(&output_paths)?,
            )
            .await?;
            export::record(
                &ctx.pool,
                &record.book_id,
                &ctx.job_id,
                &output_arg,
                &record.output_format,
                &record.mode,
                &chapter_render::digest(&chapter_rows),
            )
            .await?;
            tracing::info!(
                job = %ctx.job_id,
                "export published: path={output_arg} bytes={} duration_ms={}",
                std::fs::metadata(&output_path)
                    .map(|m| m.len())
                    .unwrap_or(0),
                started.elapsed().as_millis()
            );

            ctx.checkpoint.done_chunks = 2;
            ctx.save_checkpoint().await?;
            ctx.emit_progress(1.0, Some(0), &filename);
            Ok(StageOutcome::Completed)
        })
    }
}

#[cfg(test)]
mod cast_identity_tests {
    use super::*;

    fn chapter(id: &str, idx: i64) -> book::Chapter {
        book::Chapter {
            id: id.to_string(),
            book_id: "b1".into(),
            idx,
            title: format!("Chapter {}", idx + 1),
            page_start: 1,
            page_end: 2,
            is_pseudo: false,
            word_count: None,
        }
    }

    fn chapters_of(idxs: &[i64]) -> HashMap<String, book::Chapter> {
        idxs.iter()
            .enumerate()
            .map(|(position, &idx)| {
                let chapter = chapter(&format!("c{position}"), idx);
                (chapter.id.clone(), chapter)
            })
            .collect()
    }

    fn ids(chapters: &HashMap<String, book::Chapter>, count: usize) -> Vec<String> {
        let mut all: Vec<String> = chapters.keys().cloned().collect();
        all.sort_by_key(|id| {
            chapters
                .get(id)
                .map(|chapter| chapter.idx)
                .unwrap_or_default()
        });
        all.truncate(count);
        all
    }

    fn chapters_scope(chapter_ids: Vec<String>) -> Scope {
        Scope::Chapters { chapter_ids }
    }

    fn pages_scope(page_start: i64, page_end: i64) -> Scope {
        Scope::Pages {
            page_start,
            page_end,
        }
    }

    /// The path is the whole identity, so this composes the path the publisher
    /// actually writes rather than either half of it. Asserting on the key or
    /// the filename alone would pass while the other half stayed broken — which
    /// is exactly the bug this replaces: the label used to be a count, so any
    /// two casts of the same cardinality named the same file.
    fn export_path(
        exports_dir: &Path,
        book_title: &str,
        book_id: &str,
        scope: &Scope,
        chapters: &HashMap<String, book::Chapter>,
        voice_profile_id: &str,
        mode: &str,
    ) -> PathBuf {
        exports_dir
            .join(cast_key(book_id, scope, voice_profile_id, mode))
            .join(output_filename(book_title, &scope_label(scope, chapters)))
    }

    /// Table-driven, and it asserts on the **cast directory**, because the
    /// directory is what the design makes responsible for identity. Comparing
    /// whole paths here would let the test pass on the visible label alone — and
    /// it did: an earlier version of this test went green with `cast_key`
    /// hashing nothing but the chapter *count*, which is the original defect,
    /// because `Ch 1-2` and `Ch 5, 7` differ as filenames even when the
    /// directory is identical. A test that cannot fail is not a test.
    #[test]
    fn the_cast_directory_carries_the_identity() {
        let first_two = vec!["c0".to_string(), "c1".to_string()];
        let other_two = vec!["c4".to_string(), "c6".to_string()];
        let unresolved_a = vec!["gone-a".to_string(), "gone-b".to_string()];
        let unresolved_b = vec!["gone-c".to_string(), "gone-d".to_string()];
        let cases: [(&str, &str, Scope, &str, &str); 8] = [
            (
                "two chapters",
                "b1",
                chapters_scope(first_two.clone()),
                "voice-a",
                "narrator",
            ),
            (
                "a different two chapters: the defect this fixes",
                "b1",
                chapters_scope(other_two),
                "voice-a",
                "narrator",
            ),
            (
                "the same two chapters in another voice",
                "b1",
                chapters_scope(first_two.clone()),
                "voice-b",
                "narrator",
            ),
            (
                "the same two chapters as a lecture",
                "b1",
                chapters_scope(first_two.clone()),
                "voice-a",
                "teacher",
            ),
            (
                "another book of the same title",
                "b2",
                chapters_scope(first_two.clone()),
                "voice-a",
                "narrator",
            ),
            (
                "two chapter ids that no longer resolve, one selection",
                "b1",
                chapters_scope(unresolved_a),
                "voice-a",
                "narrator",
            ),
            (
                "two chapter ids that no longer resolve, a different selection",
                "b1",
                chapters_scope(unresolved_b),
                "voice-a",
                "narrator",
            ),
            (
                "a page range where the other cast chose chapters",
                "b1",
                pages_scope(1, 2),
                "voice-a",
                "narrator",
            ),
        ];
        let mut seen: Vec<(String, &'static str)> = Vec::new();
        for (what, book_id, scope, voice, mode) in cases {
            let key = cast_key(book_id, &scope, voice, mode);
            for (earlier, earlier_what) in &seen {
                assert_ne!(
                    &key, earlier,
                    "{what} and {earlier_what} share a cast directory; the publisher renames \
                     over it before the row is written, so this is one user served the other's \
                     audio. The last two rows share a visible name too, so nothing else in the \
                     path distinguishes them."
                );
            }
            seen.push((key, what));
        }
    }

    /// The end-to-end statement: the composed path is what is stored and
    /// resolved, and it never repeats across casts.
    #[test]
    fn different_casts_never_collide() {
        let exports_dir = Path::new("/exports");
        let chapters = chapters_of(&[0, 1, 2, 3, 4, 5, 6, 7]);
        let cases: [(&str, &str, Scope, &str, &str); 6] = [
            (
                "two chapters",
                "b1",
                chapters_scope(vec!["c0".into(), "c1".into()]),
                "voice-a",
                "narrator",
            ),
            (
                "a different two chapters",
                "b1",
                chapters_scope(vec!["c4".into(), "c6".into()]),
                "voice-a",
                "narrator",
            ),
            (
                "the same chapters in another voice",
                "b1",
                chapters_scope(vec!["c0".into(), "c1".into()]),
                "voice-b",
                "narrator",
            ),
            (
                "another book whose title happens to be identical",
                "b2",
                chapters_scope(vec!["c0".into(), "c1".into()]),
                "voice-a",
                "narrator",
            ),
            (
                "chapter ids that no longer resolve, one selection",
                "b1",
                chapters_scope(vec!["gone-a".into(), "gone-b".into()]),
                "voice-a",
                "narrator",
            ),
            (
                "chapter ids that no longer resolve, a different selection",
                "b1",
                chapters_scope(vec!["gone-c".into(), "gone-d".into()]),
                "voice-a",
                "narrator",
            ),
        ];
        let mut seen: Vec<(PathBuf, &'static str)> = Vec::new();
        for (what, book_id, scope, voice, mode) in cases {
            let path = export_path(
                exports_dir,
                "Fixture: Book?",
                book_id,
                &scope,
                &chapters,
                voice,
                mode,
            );
            for (earlier, earlier_what) in &seen {
                assert_ne!(
                    &path, earlier,
                    "{what} and {earlier_what} produced one path: the publisher renames over \
                     it before the row is written, so this is one user served the other's audio"
                );
            }
            seen.push((path, what));
        }
    }

    #[test]
    fn the_same_cast_re_rendered_still_upserts_in_place() {
        let exports_dir = Path::new("/exports");
        let chapters = chapters_of(&[0, 1, 2, 3, 4, 5, 6, 7]);
        let cases: [(&str, Scope, &str); 6] = [
            (
                "single chapter",
                chapters_scope(vec!["c3".into()]),
                "voice-a",
            ),
            ("two chapters", chapters_scope(ids(&chapters, 2)), "voice-a"),
            ("whole book", chapters_scope(ids(&chapters, 8)), "voice-a"),
            (
                "non-contiguous selection",
                chapters_scope(vec!["c0".into(), "c2".into(), "c5".into(), "c6".into()]),
                "voice-a",
            ),
            ("page range", pages_scope(1, 10), "voice-a"),
            (
                "the same range in another voice",
                pages_scope(1, 10),
                "voice-b",
            ),
        ];
        for (what, scope, voice) in cases {
            let first = export_path(
                exports_dir,
                "Fixture: Book?",
                "b1",
                &scope,
                &chapters,
                voice,
                "narrator",
            );
            let rerender = export_path(
                exports_dir,
                "Fixture: Book?",
                "b1",
                &scope,
                &chapters,
                voice,
                "narrator",
            );
            assert_eq!(
                first, rerender,
                "{what}: DATA_MODELS' promise is that a re-render updates the row rather \
                 than duplicating it, which needs a byte-identical path"
            );
        }
    }

    #[test]
    fn a_cast_that_moves_keeps_its_name_and_only_the_voice_separates_it() {
        let exports_dir = Path::new("/exports");
        let chapters = chapters_of(&[0, 1, 2]);
        let narrated = export_path(
            exports_dir,
            "Fixture: Book?",
            "b1",
            &pages_scope(1, 10),
            &chapters,
            "voice-a",
            "narrator",
        );
        let lectured = export_path(
            exports_dir,
            "Fixture: Book?",
            "b1",
            &pages_scope(1, 10),
            &chapters,
            "voice-a",
            "teacher",
        );
        assert_ne!(
            narrated.parent(),
            lectured.parent(),
            "the mode is part of the identity, so it must move the cast"
        );
        assert_eq!(
            narrated.file_name(),
            lectured.file_name(),
            "the visible name says nothing about the mode, which is what keeps it short"
        );
        assert_eq!(
            narrated.file_name().unwrap().to_str(),
            Some("Fixture_ Book_ - Pages 1-10.mp3")
        );
        assert_eq!(
            narrated.components().count(),
            4,
            "/ + <exports_dir> + <cast_key> + <name>"
        );
        assert_eq!(
            narrated.parent().unwrap().components().count(),
            3,
            "/ + <exports_dir> + <cast_key>: exactly one level below the root"
        );
    }

    #[test]
    fn the_scope_is_normalized_so_selection_order_is_not_identity() {
        let exports_dir = Path::new("/exports");
        let chapters = chapters_of(&[0, 1, 2]);
        let forward = chapters_scope(vec!["c0".into(), "c1".into()]);
        let reversed = chapters_scope(vec!["c1".into(), "c0".into()]);
        let duplicated = chapters_scope(vec![
            "c0".into(),
            "c1".into(),
            "c0".into(),
            "c1".into(),
            "c1".into(),
        ]);
        assert_eq!(
            cast_key("b1", &forward, "voice-a", "narrator"),
            cast_key("b1", &reversed, "voice-a", "narrator"),
            "[a, b] and [b, a] select the same chapters, so they are one cast"
        );
        assert_eq!(
            cast_key("b1", &forward, "voice-a", "narrator"),
            cast_key("b1", &duplicated, "voice-a", "narrator"),
            "a repeated id selects the same chapters: the UI's Set-based picker can emit one"
        );
        assert_eq!(
            export_path(
                exports_dir,
                "B",
                "b1",
                &forward,
                &chapters,
                "voice-a",
                "narrator"
            ),
            export_path(
                exports_dir,
                "B",
                "b1",
                &duplicated,
                &chapters,
                "voice-a",
                "narrator"
            ),
            "the visible name must be stable under reordering too, or one cast would land in \
             one directory under two filenames"
        );
    }

    #[test]
    fn a_page_range_is_normalized_as_ordered_not_swapped() {
        assert_ne!(
            cast_key("b1", &pages_scope(1, 10), "voice-a", "narrator"),
            cast_key("b1", &pages_scope(10, 1), "voice-a", "narrator"),
            "Pages 1-10 and Pages 10-1 are two different spans of the book; folding them \
             into one key would put two different casts in one directory"
        );
    }

    /// The framing regression. Chapter ids are the only free-form bytes in the
    /// digest input, and a delimiter cannot separate bytes that may contain any
    /// byte, so under the old `\x1f` framing these two selections are one byte
    /// stream and one cast directory.
    ///
    /// The pair is built so the ambiguous ids land in the same sorted slot in
    /// both lists — the fillers sort before `"a"`, and `"c\x1f~"` sorts between
    /// `"a"` and the fillers while `"a\x1fc"` sorts before them — because
    /// `cast_key` sorts before hashing and a list that reordered the ambiguous
    /// tail would not collide. Both lists are 72 ids, so the `chapters:{n}`
    /// count cannot separate them either.
    #[test]
    fn an_id_containing_the_framing_byte_cannot_be_read_as_a_different_selection() {
        let mut left: Vec<String> = (0..70).map(|position| format!("A{position}")).collect();
        let mut right: Vec<String> = left.clone();
        left.push("a".into());
        left.push("c\x1f~".into());
        right.push("a\x1fc".into());
        right.push("~".into());
        assert_eq!(left.len(), 72);
        assert_eq!(right.len(), 72);
        let left_key = cast_key("b1", &chapters_scope(left.clone()), "voice-a", "narrator");
        let right_key = cast_key("b1", &chapters_scope(right.clone()), "voice-a", "narrator");
        assert_ne!(
            left_key, right_key,
            "{left:?} and {right:?} are 72 different chapter ids each and share a cast \
             directory; the publisher renames over it before the row is written, so this is \
             one user served the other's audio"
        );

        // The same ambiguity with nothing in the way: two ids against two ids.
        let split = cast_key(
            "b1",
            &chapters_scope(vec!["a".into(), "b\x1fc".into()]),
            "voice-a",
            "narrator",
        );
        let joined = cast_key(
            "b1",
            &chapters_scope(vec!["a\x1fb".into(), "c".into()]),
            "voice-a",
            "narrator",
        );
        assert_ne!(
            split, joined,
            "[a, \"b\\x1fc\"] and [\"a\\x1fb\", c] are the byte stream \\x1f a \\x1f c, which a \
             delimiter cannot tell apart from one selection"
        );
    }

    /// Every byte the framing could have mistaken for structure: the unit
    /// separator, the record separator that marks the field tags, and NUL. Each
    /// of these selections normalizes to a different set of ids, so each must get
    /// its own directory, and none of them may stop the function — an id is a
    /// `String`, so a NUL in it is an ordinary byte, not a terminator.
    #[test]
    fn framing_and_terminator_bytes_inside_an_id_are_carried_not_truncated() {
        let selections: [&[&str]; 10] = [
            &["a", "b\x1fc"],
            &["a\x1fb", "c"],
            &["a", "b\x1ec"],
            &["a\x1eb", "c"],
            &["a", "b\0c"],
            &["a\0b", "c"],
            &["\x1f"],
            &["\x1e"],
            &["\0"],
            &["a\x1f", "b\x1e", "\0c"],
        ];
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut compared = 0usize;
        for selection in selections {
            let key = cast_key(
                "b1",
                &chapters_scope(selection.iter().map(|id| (*id).to_string()).collect()),
                "voice-a",
                "narrator",
            );
            for (earlier, earlier_selection) in &seen {
                compared += 1;
                assert_ne!(
                    &key, earlier,
                    "{selection:?} and {earlier_selection:?} are different chapter selections \
                     that landed in one directory"
                );
            }
            seen.push((key, format!("{selection:?}")));
        }
        assert_eq!(
            compared, 45,
            "every pair of the ten selections was compared, not only adjacent ones"
        );
    }

    /// Exhausts every selection of one, two and three ids drawn from an alphabet
    /// built to be hostile to any framing, and requires the digest to separate
    /// them.
    ///
    /// The alphabet is multi-byte on purpose. An alphabet of single bytes cannot
    /// collide under a `\x1f` delimiter at all — each id is one byte and the count
    /// says how many there are, so a sweep built from it proves nothing and would
    /// pass against the broken encoding. `b\x1fc` and `a\x1fb` are the shape that
    /// does collide, and the check at the end requires them to be in scope.
    ///
    /// The oracle is the normalized selection, not the raw list: sorting and
    /// de-duplication are required behavior, so `[a, b]` and `[b, a]` are one cast
    /// and would be a false failure here. The property under test is therefore
    /// injectivity on normalized selections — two different sets of ids, one key
    /// each, and no key shared.
    #[test]
    fn no_two_distinct_selections_over_the_framing_alphabet_share_a_key() {
        const ALPHABET: [&str; 12] = [
            "a", "b", "c", "\x1f", "\x1e", "\0", "b\x1fc", "a\x1fb", "b\x1ec", "a\x1eb", "b\0c",
            "a\0b",
        ];

        fn normalized(list: &[String]) -> Vec<String> {
            let mut ids = list.to_vec();
            ids.sort();
            ids.dedup();
            ids
        }

        let mut lists: Vec<Vec<String>> = Vec::new();
        for a in ALPHABET {
            lists.push(vec![a.to_string()]);
            for b in ALPHABET {
                lists.push(vec![a.to_string(), b.to_string()]);
                for c in ALPHABET {
                    lists.push(vec![a.to_string(), b.to_string(), c.to_string()]);
                }
            }
        }

        let mut key_by_selection: HashMap<Vec<String>, String> = HashMap::new();
        for list in &lists {
            let selection = normalized(list);
            let key = cast_key(
                "b1",
                &chapters_scope(selection.clone()),
                "voice-a",
                "narrator",
            );
            match key_by_selection.get(&selection) {
                // The same normalized selection must keep the same key, or a
                // re-render would land in a second directory.
                Some(earlier) => assert_eq!(
                    &key, earlier,
                    "{selection:?} was reached as {list:?} too, but the two disagree on the \
                     cast directory"
                ),
                None => {
                    key_by_selection.insert(selection, key);
                }
            }
        }

        assert_eq!(
            lists.len(),
            12 + 12 * 12 + 12 * 12 * 12,
            "the sweep must cover every selection of one, two and three ids"
        );

        let keys: std::collections::HashSet<&String> = key_by_selection.values().collect();
        assert_eq!(
            keys.len(),
            key_by_selection.len(),
            "{} distinct selections produced only {} distinct cast directories",
            key_by_selection.len(),
            keys.len(),
        );

        // The sweep is only evidence if it actually contains the known collision,
        // so assert the pair is in scope rather than trusting the alphabet.
        for pair in [
            (
                vec!["a".to_string(), "b\x1fc".to_string()],
                vec!["a\x1fb".to_string(), "c".to_string()],
            ),
            (
                vec!["a".to_string(), "b\x1ec".to_string()],
                vec!["a\x1eb".to_string(), "c".to_string()],
            ),
            (
                vec!["a".to_string(), "b\0c".to_string()],
                vec!["a\0b".to_string(), "c".to_string()],
            ),
        ] {
            let (left, right) = pair;
            let left_key = key_by_selection.get(&left).expect("left is in the sweep");
            let right_key = key_by_selection.get(&right).expect("right is in the sweep");
            assert_ne!(
                left_key, right_key,
                "{left:?} and {right:?} are both in the sweep and share a cast directory"
            );
        }
    }

    /// The field marker that selects the scope branch is a literal, not something
    /// an id can spell, so no selection can be read as a page range or the other
    /// way round.
    #[test]
    fn the_scope_discriminant_is_a_literal_and_not_content() {
        let cases: [(&str, Scope); 6] = [
            ("a page range", pages_scope(1, 10)),
            ("another page range", pages_scope(10, 1)),
            (
                "one id spelled like the range",
                chapters_scope(vec!["1-10".into()]),
            ),
            (
                "one id spelled like the pages field",
                chapters_scope(vec!["pages:1-10".into()]),
            ),
            (
                "one id spelled like the chapters field",
                chapters_scope(vec!["chapters:1".into()]),
            ),
            (
                "an id carrying the record separator",
                chapters_scope(vec!["\x1epages:1-10".into()]),
            ),
        ];
        let mut seen: Vec<(String, &'static str)> = Vec::new();
        for (what, scope) in cases {
            let key = cast_key("b1", &scope, "voice-a", "narrator");
            for (earlier, earlier_what) in &seen {
                assert_ne!(
                    &key, earlier,
                    "{what} and {earlier_what} share a cast directory, so the branch marker is \
                     being taken from content"
                );
            }
            seen.push((key, what));
        }
    }

    /// The re-render invariant, on the fields that could carry state into a key:
    /// no randomness, no hash-map iteration order, and no dependence on how the
    /// caller happened to order the list.
    #[test]
    fn one_cast_has_one_key_however_the_selection_arrives() {
        let literal = ["c0", "c1", "c2", "c7"];
        let expected = cast_key(
            "b1",
            &chapters_scope(literal.iter().map(|id| (*id).to_string()).collect()),
            "voice-a",
            "narrator",
        );

        // Every permutation, exhaustively rather than by sampling.
        let permutations = permutations_of(&literal);
        assert_eq!(permutations.len(), 24, "4! permutations were generated");
        for order in &permutations {
            assert_eq!(
                cast_key("b1", &chapters_scope(order.clone()), "voice-a", "narrator"),
                expected,
                "{order:?} selects the same chapters, so it must be the same cast"
            );
        }

        // A fresh map per call, so the iteration order is not held fixed by the
        // process-wide seed and reused across the loop.
        for round in 0..16 {
            let map: HashMap<&str, ()> = literal.iter().map(|id| (*id, ())).collect();
            let from_map: Vec<String> = map.keys().map(|id| (*id).to_string()).collect();
            assert_eq!(
                cast_key("b1", &chapters_scope(from_map), "voice-a", "narrator"),
                expected,
                "round {round}: a hash map's iteration order must not reach the cast key"
            );
        }

        // Repeated identical calls, for both scope variants.
        for _ in 0..8 {
            assert_eq!(
                cast_key(
                    "b1",
                    &chapters_scope(literal.iter().map(|id| (*id).to_string()).collect()),
                    "voice-a",
                    "narrator"
                ),
                expected
            );
            assert_eq!(
                cast_key("b1", &pages_scope(3, 9), "voice-a", "narrator"),
                cast_key("b1", &pages_scope(3, 9), "voice-a", "narrator")
            );
        }
    }

    fn permutations_of(items: &[&str]) -> Vec<Vec<String>> {
        let mut orders: Vec<Vec<String>> = vec![Vec::new()];
        for item in items {
            let mut grown = Vec::new();
            for order in &orders {
                for position in 0..=order.len() {
                    let mut extended = order.clone();
                    extended.insert(position, (*item).to_string());
                    grown.push(extended);
                }
            }
            orders = grown;
        }
        orders
    }

    /// Sweeps the directory, not the composed path: the same-cardinality pairs
    /// below produce different *filenames* (their chapter numbers differ), so
    /// only the directory is what separates them and only the directory is
    /// worth sweeping.
    #[test]
    fn every_pair_of_same_cardinality_chapter_sets_gets_its_own_cast_directory() {
        let mut seen: Vec<(String, String)> = Vec::new();
        for start in 0..23 {
            let selection = vec![format!("c{start}"), format!("c{}", start + 1)];
            let key = cast_key(
                "b1",
                &chapters_scope(selection.clone()),
                "voice-a",
                "narrator",
            );
            for (earlier, earlier_selection) in &seen {
                assert_ne!(
                    &key, earlier,
                    "{selection:?} and {earlier_selection:?} are different two-chapter casts \
                     that landed in one directory"
                );
            }
            seen.push((key, format!("{selection:?}")));
        }
        assert_eq!(seen.len(), 23, "every adjacent pair was compared");

        let keys: std::collections::HashSet<&String> = seen.iter().map(|(key, _)| key).collect();
        assert_eq!(
            keys.len(),
            seen.len(),
            "no two selections share a directory at all, not only adjacent ones"
        );
    }

    #[test]
    fn a_cast_key_is_lowercase_hex_and_one_path_component() {
        let key = cast_key(
            "book id with spaces/and separators",
            &chapters_scope(vec!["c0".into()]),
            "voice id/with separators",
            "narrator",
        );
        assert_eq!(key.len(), 64, "sha256 rendered as hex");
        assert!(
            key.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "the key is a directory name and must be lowercase hex: {key}"
        );
        assert_eq!(Path::new(&key).components().count(), 1, "{key}");
        assert_eq!(
            Path::new(&key).file_name().and_then(|name| name.to_str()),
            Some(key.as_str()),
            "a separator in any input must not create a directory"
        );
    }

    fn label_of(scope: &Scope, chapters: &HashMap<String, book::Chapter>) -> String {
        scope_label(scope, chapters)
    }

    #[test]
    fn a_scope_label_names_chapters_by_their_position_in_the_book() {
        let chapters = chapters_of(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let cases: [(&str, Scope, &str); 8] = [
            ("single chapter", chapters_scope(vec!["c4".into()]), "Ch 5"),
            (
                "two adjacent chapters collapse to a run",
                chapters_scope(vec!["c0".into(), "c1".into()]),
                "Ch 1-2",
            ),
            (
                "two scattered chapters stay separate",
                chapters_scope(vec!["c2".into(), "c6".into()]),
                "Ch 3, 7",
            ),
            (
                "the whole book is one run",
                chapters_scope(ids(&chapters, 10)),
                "Ch 1-10",
            ),
            (
                "a non-contiguous selection mixes runs",
                chapters_scope(vec![
                    "c0".into(),
                    "c1".into(),
                    "c3".into(),
                    "c7".into(),
                    "c8".into(),
                ]),
                "Ch 1-2, 4, 8-9",
            ),
            ("a page range", pages_scope(1, 10), "Pages 1-10"),
            ("a single page", pages_scope(7, 7), "Pages 7-7"),
            (
                "an empty chapter list",
                chapters_scope(vec![]),
                NO_CHAPTER_SCOPE_LABEL,
            ),
        ];
        for (what, scope, expected) in cases {
            assert_eq!(
                label_of(&scope, &chapters),
                expected,
                "{what}: the label is built from chapter.idx + 1 because idx is the engine's \
                 0-based reading order, and from nothing else"
            );
        }
    }

    #[test]
    fn a_contiguous_run_collapses_before_the_cap_ever_considers_it() {
        let chapters = chapters_of(&(0..400).collect::<Vec<i64>>());
        let label = label_of(&chapters_scope(ids(&chapters, 400)), &chapters);
        assert_eq!(
            label, "Ch 1-400",
            "four hundred contiguous chapters are one run, so the whole book stays a name a \
             person reads and no remainder is needed"
        );
        assert!(
            !label.contains(TRUNCATION_MARKER),
            "nothing was elided: {label}"
        );
    }

    #[test]
    fn an_unresolvable_chapter_is_named_honestly_rather_than_counted() {
        let chapters = chapters_of(&[0, 1]);
        assert_eq!(
            label_of(&chapters_scope(vec!["gone".into()]), &chapters),
            UNRESOLVED_SCOPE_LABEL,
            "a chapter with no row has no position in the book; the cast is still distinct \
             because its key is built from the ids"
        );
        assert_eq!(
            label_of(&chapters_scope(vec!["gone".into(), "c0".into()]), &chapters),
            "Ch 1, …+1",
            "an unresolvable id counts as unshown rather than vanishing silently"
        );
    }

    #[test]
    fn a_scattered_selection_too_long_for_the_budget_is_capped_with_a_visible_remainder() {
        // Every other chapter: 400 runs of one, so nothing collapses and the cap
        // is the only thing that can shorten the label.
        let chapters = chapters_of(&(0..400).map(|i| i * 2).collect::<Vec<i64>>());
        let label = label_of(&chapters_scope(ids(&chapters, 400)), &chapters);
        assert!(
            label.len() <= SCOPE_LABEL_BUDGET_BYTES,
            "the label must fit the budget output_filename reserves for it, got {} of {} \
             bytes: {label}",
            label.len(),
            SCOPE_LABEL_BUDGET_BYTES
        );
        assert!(label.starts_with("Ch 1, 3, 5"), "{label}");
        let (shown, remainder) = label
            .rsplit_once(", ")
            .expect("a capped label shows a tail");
        assert!(
            remainder.starts_with(TRUNCATION_MARKER),
            "the cap must be visible to the user: {label}"
        );
        let elided: usize = remainder[TRUNCATION_MARKER.len_utf8() + 1..]
            .parse()
            .expect("the remainder is a count");
        assert_eq!(
            shown.split(", ").count() + elided,
            400,
            "the shown prefix and the remainder must account for the same 400 chapters: {label}"
        );
        assert!(
            shown.len() < 400,
            "a 400-chapter selection must not produce a 400-item name"
        );
    }

    #[test]
    fn the_list_cap_and_the_byte_trim_never_both_fire() {
        let chapters = chapters_of(&(0..2_000).map(|i| i * 2).collect::<Vec<i64>>());
        let label = label_of(&chapters_scope(ids(&chapters, 2_000)), &chapters);
        assert_eq!(
            label.matches(TRUNCATION_MARKER).count(),
            1,
            "the label elides its own chapters exactly once: {label}"
        );
        let name = output_filename("A Very Long Book Title", &label);
        assert_eq!(
            name.matches(TRUNCATION_MARKER).count(),
            1,
            "and the byte trim in output_filename must find nothing left to cut: {name}"
        );
        assert!(name.ends_with(".mp3"), "{name}");
    }
}
#[cfg(test)]
mod drift_tests {
    use super::*;

    #[test]
    fn a_packet_lasts_24_ms_at_either_rate_the_encoder_picks() {
        assert_eq!(mp3_packets_ms(148, 24_000), 3552);
        assert_eq!(mp3_packets_ms(148, 48_000), 3552);
        assert_eq!(mp3_packets_ms(1, 44_100), 26);
    }

    const MEASURED_NOMINAL_MS: [i64; 12] = [
        180434, 194994, 202789, 217447, 158219, 173866, 203023, 209676, 237253, 178444, 184231,
        207903,
    ];
    const MEASURED_ENCODED_MS: [i64; 12] = [
        180480, 195024, 202824, 217488, 158256, 173904, 203064, 209712, 237288, 178488, 184272,
        207936,
    ];
    const MEASURED_CONCAT_ACTUAL_MS: i64 = 2_348_736;
    const NOMINAL_MARKER_ERROR_MS: i64 = 457;
    const R2_WORD_ONSET_BUDGET_MS: i64 = 120;

    fn chapters(lengths: &[i64; 12]) -> Vec<EncodedChapter> {
        lengths
            .iter()
            .enumerate()
            .map(|(idx, length)| EncodedChapter {
                chapter_id: format!("c{idx}"),
                encoded_ms: *length,
                leading_gap_ms: 0,
            })
            .collect()
    }

    fn segment(chapter: &str, idx: usize) -> RenderSegment {
        RenderSegment {
            segment_id: format!("seg{idx}"),
            chapter_id: chapter.into(),
            audio_hash: format!("h{idx}"),
            duration_ms: 900,
        }
    }

    #[test]
    fn consecutive_segments_of_one_chapter_become_a_single_group() {
        let groups = chapter_groups(&[
            segment("c0", 0),
            segment("c0", 1),
            segment("c1", 2),
            segment("c2", 3),
        ])
        .expect("contiguous chapters");
        assert_eq!(
            groups
                .iter()
                .map(|(id, items)| (id.as_str(), items.len()))
                .collect::<Vec<_>>(),
            vec![("c0", 2), ("c1", 1), ("c2", 1)]
        );
    }

    #[test]
    fn an_interleaved_chapter_is_rejected_instead_of_collapsing_into_two_rows() {
        let err = chapter_groups(&[segment("c0", 0), segment("c1", 1), segment("c0", 2)])
            .expect_err(
                "two runs of one chapter share a primary key, so one would overwrite the other",
            );
        match err {
            OrchestratorError::Render(message) => assert!(
                message.contains("c0") && message.contains("interleaved"),
                "the error must name the chapter that broke the ordering: {message}"
            ),
            other => panic!("expected a render error, got {other}"),
        }
    }

    #[test]
    fn markers_built_from_encoded_durations_match_the_concatenated_file_exactly() {
        let markers = markers_from_encoded(&chapters(&MEASURED_ENCODED_MS), |id| id.to_string());
        assert_eq!(
            markers.last().expect("markers").end_ms,
            MEASURED_CONCAT_ACTUAL_MS
        );
    }

    #[test]
    fn nominal_durations_would_miss_the_end_by_the_measured_drift() {
        let markers = markers_from_encoded(&chapters(&MEASURED_NOMINAL_MS), |id| id.to_string());
        let error = MEASURED_CONCAT_ACTUAL_MS - markers.last().expect("markers").end_ms;
        assert_eq!(error, NOMINAL_MARKER_ERROR_MS);
        assert!(
            error > R2_WORD_ONSET_BUDGET_MS,
            "the measured nominal error is inside the highlight budget, so this rule would be pointless"
        );
    }

    #[test]
    fn every_chapter_boundary_is_contiguous_when_no_gap_is_inside_the_artifact() {
        let markers = markers_from_encoded(&chapters(&MEASURED_ENCODED_MS), |id| id.to_string());
        assert_eq!(markers[0].start_ms, 0);
        for pair in markers.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms);
        }
    }

    #[test]
    fn a_leading_gap_belongs_to_the_silence_before_a_chapter_not_to_the_chapter() {
        let encoded = vec![
            EncodedChapter {
                chapter_id: "c0".into(),
                encoded_ms: 2200,
                leading_gap_ms: 0,
            },
            EncodedChapter {
                chapter_id: "c1".into(),
                encoded_ms: CHAPTER_GAP_MS + 1500,
                leading_gap_ms: CHAPTER_GAP_MS,
            },
        ];
        let markers = markers_from_encoded(&encoded, |id| id.to_string());
        assert_eq!((markers[0].start_ms, markers[0].end_ms), (0, 2200));
        assert_eq!(
            (markers[1].start_ms, markers[1].end_ms),
            (2200 + CHAPTER_GAP_MS, 2200 + CHAPTER_GAP_MS + 1500),
            "the gap is inside the second artifact's bytes, but seeking to chapter 2 must \
             land on speech, exactly as it does on the dynamic path"
        );
        assert_eq!(
            markers[1].start_ms - markers[0].end_ms,
            CHAPTER_GAP_MS,
            "the two markers must be separated by the gap, not abut across it"
        );
    }
}
