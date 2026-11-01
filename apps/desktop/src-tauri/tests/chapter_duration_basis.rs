use std::path::{Path, PathBuf};
use std::process::Command;

use txtcaster_lib::render::mp3::{
    chapter_encode_args, measure_filter, measured_filter, parse_loudnorm_json, silence_wav,
    CHAPTER_GAP_MS, SAMPLE_RATE, SEGMENT_GAP_MS,
};

const SAMPLES_PER_FRAME: i64 = 576;
const CHAPTERS: usize = 6;
const SEGMENT_MS: i64 = 900;
const ONSET_BUDGET_MS: i64 = 120;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn tone_wav(duration_ms: i64, freq: f64) -> Vec<u8> {
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
    for index in 0..frames {
        let t = index as f64 / SAMPLE_RATE as f64;
        let value = (0.5 * 32767.0 * (2.0 * std::f64::consts::PI * freq * t).sin()) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn ffmpeg(args: &[&str]) -> std::process::Output {
    Command::new("ffmpeg").args(args).output().expect("ffmpeg")
}

fn ffprobe_field(args: &[&str], path: &Path) -> String {
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error"]).args(args).arg(path);
    let out = command.output().expect("ffprobe");
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn declared_ms(path: &Path) -> i64 {
    let raw = ffprobe_field(
        &[
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ],
        path,
    );
    (raw.parse::<f64>().expect("duration") * 1000.0).round() as i64
}

fn packet_ms(path: &Path) -> i64 {
    let raw = ffprobe_field(
        &[
            "-select_streams",
            "a:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "default=nw=1:nk=1",
        ],
        path,
    );
    raw.parse::<i64>().expect("packets") * SAMPLES_PER_FRAME * 1000 / SAMPLE_RATE as i64
}

fn decode_pcm(path: &Path) -> Vec<i16> {
    let out = ffmpeg(&[
        "-v",
        "error",
        "-nostdin",
        "-i",
        &path.to_string_lossy(),
        "-f",
        "s16le",
        "-acodec",
        "pcm_s16le",
        "-",
    ]);
    assert!(out.status.success(), "decode failed");
    out.stdout
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn speech_onsets_ms(pcm: &[i16]) -> Vec<i64> {
    let step = (SAMPLE_RATE / 1000) as usize;
    let envelope: Vec<i32> = pcm
        .chunks(step)
        .map(|window| window.iter().map(|v| (*v as i32).abs()).max().unwrap_or(0))
        .collect();
    let gate = envelope.iter().copied().max().unwrap_or(0) / 5;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (index, peak) in envelope.iter().enumerate() {
        match (*peak > gate, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                spans.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        spans.push((begin, envelope.len()));
    }
    spans
        .into_iter()
        .map(|(begin, end)| {
            let local = envelope[begin..end].iter().copied().max().unwrap_or(0);
            let floor = local / 100;
            let mut onset = begin;
            while onset > 0 && envelope[onset - 1] > floor {
                onset -= 1;
            }
            onset as i64
        })
        .collect()
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write fixture");
}

fn listing(files: &[String]) -> String {
    let mut out = String::from("ffconcat version 1.0\n");
    for name in files {
        out.push_str(&format!("file '{name}'\n"));
    }
    out
}

struct Book {
    chapters: Vec<PathBuf>,
    nominal: Vec<i64>,
    export: PathBuf,
}

fn build_book(work: &Path) -> Book {
    write(&work.join("gap_segment.wav"), &silence_wav(SEGMENT_GAP_MS));
    write(&work.join("gap_chapter.wav"), &silence_wav(CHAPTER_GAP_MS));

    let mut whole: Vec<String> = Vec::new();
    let mut per_chapter: Vec<Vec<String>> = Vec::new();
    let mut nominal: Vec<i64> = Vec::new();
    for chapter in 0..CHAPTERS {
        let mut files: Vec<String> = Vec::new();
        if chapter > 0 {
            files.push("gap_chapter.wav".into());
            whole.push("gap_chapter.wav".into());
        }
        for segment in 0..3 {
            let name = format!("c{chapter}s{segment}.wav");
            write(
                &work.join(&name),
                &tone_wav(SEGMENT_MS, 300.0 + 37.0 * (chapter * 3 + segment) as f64),
            );
            if segment > 0 {
                files.push("gap_segment.wav".into());
                whole.push("gap_segment.wav".into());
            }
            files.push(name.clone());
            whole.push(name);
        }
        nominal.push(
            if chapter == 0 { 0 } else { CHAPTER_GAP_MS } + 3 * SEGMENT_MS + 2 * SEGMENT_GAP_MS,
        );
        per_chapter.push(files);
    }

    let whole_list = work.join("whole.txt");
    write(&whole_list, listing(&whole).as_bytes());
    let measure = ffmpeg(&[
        "-hide_banner",
        "-nostdin",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &whole_list.to_string_lossy(),
        "-af",
        &measure_filter(),
        "-f",
        "null",
        "-",
    ]);
    let measurement = parse_loudnorm_json(&String::from_utf8_lossy(&measure.stderr))
        .expect("the fixture must yield a loudnorm measurement");
    let filter = measured_filter(&measurement);

    let mut chapters: Vec<PathBuf> = Vec::new();
    for (index, files) in per_chapter.iter().enumerate() {
        let list_path = work.join(format!("chapter{index}.txt"));
        write(&list_path, listing(files).as_bytes());
        let target = work.join(format!("chapter{index}.mp3"));
        let list_arg = list_path.to_string_lossy().to_string();
        let target_arg = target.to_string_lossy().to_string();
        let args = chapter_encode_args(&list_arg, &filter, &target_arg);
        let encoded = ffmpeg(&args);
        assert!(
            encoded.status.success(),
            "chapter encode failed: {}",
            String::from_utf8_lossy(&encoded.stderr)
        );
        chapters.push(target);
    }

    let export_list = work.join("book.txt");
    let names: Vec<String> = (0..CHAPTERS).map(|i| format!("chapter{i}.mp3")).collect();
    write(&export_list, listing(&names).as_bytes());
    let export = work.join("book.mp3");
    let concat = ffmpeg(&[
        "-hide_banner",
        "-nostdin",
        "-y",
        "-v",
        "error",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &export_list.to_string_lossy(),
        "-c",
        "copy",
        &export.to_string_lossy(),
    ]);
    assert!(concat.status.success(), "concat failed");

    Book {
        chapters,
        nominal,
        export,
    }
}

fn cumulative_starts(lengths: &[i64]) -> Vec<i64> {
    let mut out = Vec::with_capacity(lengths.len());
    let mut cursor = 0;
    for (index, length) in lengths.iter().enumerate() {
        out.push(cursor + if index == 0 { 0 } else { CHAPTER_GAP_MS });
        cursor += length;
    }
    out
}

#[test]
fn the_declared_chapter_duration_is_the_frame_count_a_concat_player_hears() {
    if !ffmpeg_available() {
        assert!(
            std::env::var("TXTCASTER_ALLOW_MISSING_FFMPEG").is_ok(),
            "this guard is the only thing measuring the basis every chapter marker is built on, \
             so a machine without ffmpeg must fail it rather than report green. Set \
             TXTCASTER_ALLOW_MISSING_FFMPEG=1 to acknowledge the gap locally; CI has ffmpeg."
        );
        eprintln!("skipping: ffmpeg not on PATH and the gap was acknowledged");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let book = build_book(dir.path());

    let declared: Vec<i64> = book.chapters.iter().map(|p| declared_ms(p)).collect();
    let packets: Vec<i64> = book.chapters.iter().map(|p| packet_ms(p)).collect();
    assert_eq!(
        declared, packets,
        "encoded_ms is the declared duration, and it is trusted as a chapter origin because it \
         equals the frame count a player of the -c copy concat actually decodes. If an ffmpeg or \
         libmp3lame build breaks that equality, every chapter marker moves by whatever the \
         difference is, silently. Re-measure with scripts/probe_encoded_ms.py and update \
         PIPELINE_SPEC 8.1 before changing this test."
    );

    let onsets = speech_onsets_ms(&decode_pcm(&book.export));
    assert_eq!(
        onsets.len(),
        CHAPTERS * 3,
        "the fixture must present one detectable onset per segment"
    );
    let truth: Vec<i64> = onsets.iter().step_by(3).copied().collect();

    let from_declared = cumulative_starts(&declared);
    let mut worst = 0;
    for (index, expected) in truth.iter().enumerate() {
        let error = (from_declared[index] - expected).abs();
        worst = worst.max(error);
        assert!(
            error <= ONSET_BUDGET_MS,
            "chapter {index} origin from encoded_ms is {} ms off the audio a player hears \
             (budget {ONSET_BUDGET_MS} ms)",
            from_declared[index] - expected
        );
    }

    let from_nominal = cumulative_starts(&book.nominal);
    let nominal_worst = truth
        .iter()
        .enumerate()
        .map(|(index, expected)| (from_nominal[index] - expected).abs())
        .max()
        .unwrap_or(0);
    assert!(
        nominal_worst > worst,
        "this fixture must show why markers come from encoded_ms rather than nominal durations; \
         nominal worst {nominal_worst} ms vs measured worst {worst} ms"
    );
}
