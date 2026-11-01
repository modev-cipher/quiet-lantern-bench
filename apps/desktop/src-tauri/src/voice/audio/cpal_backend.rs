//! Provenance (AGENTS.md §5a): cpal 0.18 capture and playback behind the unit-1 trait.
//! Source: crate source read at ~/.cargo/registry/.../cpal-0.18.1/src/{lib.rs,traits.rs,
//!   platform/mod.rs}. Checked: 2026-08-19. API facts this module depends on:
//!   - `SampleRate` is a plain `u32` and `StreamConfig { channels, sample_rate,
//!     buffer_size }` (lib.rs:452) is built directly; `BufferSize::Default` defers the
//!     period choice to the host (lib.rs:388).
//!   - `build_input_stream` / `build_output_stream` (traits.rs:239, 295) take
//!     `StreamConfig` by value plus a timeout argument; the output callback receives
//!     `&mut [T]` it must fill completely.
//!   - `SupportedStreamConfig::{channels, sample_rate, sample_format}` (lib.rs:547-559)
//!     describe the device default; the device name is its `Display` impl.
//!   - `StreamTrait::{play, pause}` (traits.rs:427, 445); dropping the `Stream` closes it.
//!
//! This file is the **only** one in `voice::audio` allowed to name cpal. Everything the
//! spec calls latency-critical — rings, resamplers, jitter buffer, generation gate —
//! lives above [`super::device::AudioBackend`] and is proven in CI with synthetic
//! buffers and simulated time; this adapter only moves PCM between cpal callbacks and
//! that trait. Host enumeration happens inside the trait methods, never at startup, for
//! the same reason as `crate::voice_capture`: a machine with no sound server must still
//! open the app.
//!
//! Wiring (VOICE_AGENT §2 data plane), tokio side in the middle:
//!
//! ```text
//! open_input(fmt, sink = ring::SpscProducer)
//!   mic ─► downmix to mono i16 ─► sink.on_capture ─► SpscConsumer ─► MicRing +
//!     resample::MonoResampler::to_provider_rate ─► 16 kHz ─► channel.send_audio
//!
//! provider 24 kHz ─► MonoResampler::to_device_rate ─► jitter::JitterBuffer ─►
//!   generation::TaggedPlaybackWriter ─► open_output(fmt, source =
//!     generation::GatedPlaybackSource) ─► fan out mono ─► speaker
//! ```
//!
//! Barge-in order on the tokio side: `Generation::bump` → `JitterBuffer::flush` →
//! down `MonoResampler::reset` → `channel.interrupt()`.
//!
//! The 30-minute acceptance run is the `#[ignore]` test at the bottom (ROADMAP note ³
//! manual-rig split; the CI-run tests in this module touch no device). To run it, wear
//! headphones (the rig plays the mic straight back, open speakers howl), then:
//!
//! ```text
//! cd apps/desktop/src-tauri
//! cargo test --lib voice::audio::cpal_backend -- --ignored --nocapture
//! LOOPBACK_SECS=60 cargo test --lib voice::audio::cpal_backend -- --ignored --nocapture
//! ```
//!
//! The second form shortens the soak for a smoke pass; the acceptance figure is the
//! default 1800 s. Glitch-free means the printed summary ends with zero underruns and
//! zero dropped frames on every seam.

use std::sync::Mutex;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::device::{
    AudioBackend, AudioError, AudioStream, CaptureSink, PlaybackSource, StreamFormat,
};

const SCRATCH_FRAMES: usize = 4_096;

fn classify(detail: String) -> AudioError {
    let lowered = detail.to_ascii_lowercase();
    if lowered.contains("permission") || lowered.contains("denied") || lowered.contains("access") {
        AudioError::PermissionDenied
    } else {
        AudioError::Unavailable(detail)
    }
}

fn i16_from_f32(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32_767.0) as i16
}

fn f32_from_i16(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

fn downmix_i16_into(interleaved: &[i16], channels: usize, mono: &mut [i16]) -> usize {
    let frames = (interleaved.len() / channels).min(mono.len());
    for (frame, slot) in mono[..frames].iter_mut().enumerate() {
        let start = frame * channels;
        let sum: i32 = interleaved[start..start + channels]
            .iter()
            .map(|&s| i32::from(s))
            .sum();
        *slot = (sum / channels as i32) as i16;
    }
    frames
}

fn downmix_f32_into(interleaved: &[f32], channels: usize, mono: &mut [i16]) -> usize {
    let frames = (interleaved.len() / channels).min(mono.len());
    for (frame, slot) in mono[..frames].iter_mut().enumerate() {
        let start = frame * channels;
        let sum: f32 = interleaved[start..start + channels].iter().sum();
        *slot = i16_from_f32(sum / channels as f32);
    }
    frames
}

fn fan_out_i16(mono: &[i16], channels: usize, interleaved: &mut [i16]) {
    for (frame, &sample) in mono.iter().enumerate() {
        let start = frame * channels;
        interleaved[start..start + channels].fill(sample);
    }
}

fn fan_out_f32(mono: &[i16], channels: usize, interleaved: &mut [f32]) {
    for (frame, &sample) in mono.iter().enumerate() {
        let start = frame * channels;
        interleaved[start..start + channels].fill(f32_from_i16(sample));
    }
}

struct CpalStream {
    inner: Mutex<cpal::Stream>,
}

// SAFETY: cpal::Stream is not Send/Sync on every backend, but the only pointer to it
// lives inside this Mutex and every use goes through &self methods that take the lock,
// so no two threads ever touch the stream concurrently. Same argument and precedent as
// crate::voice_capture::Recorder.
unsafe impl Send for CpalStream {}

impl AudioStream for CpalStream {
    fn play(&self) -> Result<(), AudioError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| AudioError::Unavailable("stream lock poisoned".into()))?;
        guard.play().map_err(|err| classify(err.to_string()))
    }

    fn pause(&self) -> Result<(), AudioError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| AudioError::Unavailable("stream lock poisoned".into()))?;
        guard.pause().map_err(|err| classify(err.to_string()))
    }
}

/// The real-device implementation of the unit-1 seam. Stateless: every call looks up
/// the host's current default device, so a device change between sessions needs no
/// invalidation here.
pub struct CpalBackend;

impl CpalBackend {
    fn input_device() -> Result<cpal::Device, AudioError> {
        cpal::default_host()
            .default_input_device()
            .ok_or(AudioError::NoDevice)
    }

    fn output_device() -> Result<cpal::Device, AudioError> {
        cpal::default_host()
            .default_output_device()
            .ok_or(AudioError::NoDevice)
    }
}

impl AudioBackend for CpalBackend {
    fn default_input_format(&self) -> Result<StreamFormat, AudioError> {
        let config = Self::input_device()?
            .default_input_config()
            .map_err(|err| classify(err.to_string()))?;
        Ok(StreamFormat::new(config.sample_rate(), config.channels()))
    }

    fn default_output_format(&self) -> Result<StreamFormat, AudioError> {
        let config = Self::output_device()?
            .default_output_config()
            .map_err(|err| classify(err.to_string()))?;
        Ok(StreamFormat::new(config.sample_rate(), config.channels()))
    }

    fn open_input(
        &self,
        format: StreamFormat,
        mut sink: Box<dyn CaptureSink>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let device = Self::input_device()?;
        let sample_format = device
            .default_input_config()
            .map_err(|err| classify(err.to_string()))?
            .sample_format();
        let channels = usize::from(format.channels.max(1));
        let config = cpal::StreamConfig {
            channels: format.channels.max(1),
            sample_rate: format.sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let err_fn = |err| tracing::warn!("voice tutor capture stream error: {err}");
        let mut mono = vec![0i16; SCRATCH_FRAMES];

        let stream = match sample_format {
            cpal::SampleFormat::I16 => device.build_input_stream(
                config,
                move |data: &[i16], _: &_| {
                    for chunk in data.chunks(SCRATCH_FRAMES * channels) {
                        let frames = downmix_i16_into(chunk, channels, &mut mono);
                        sink.on_capture(&mono[..frames]);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], _: &_| {
                    for chunk in data.chunks(SCRATCH_FRAMES * channels) {
                        let frames = downmix_f32_into(chunk, channels, &mut mono);
                        sink.on_capture(&mono[..frames]);
                    }
                },
                err_fn,
                None,
            ),
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "input sample format {other:?}"
                )))
            }
        }
        .map_err(|err| classify(err.to_string()))?;

        Ok(Box::new(CpalStream {
            inner: Mutex::new(stream),
        }))
    }

    fn open_output(
        &self,
        format: StreamFormat,
        mut source: Box<dyn PlaybackSource>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let device = Self::output_device()?;
        let sample_format = device
            .default_output_config()
            .map_err(|err| classify(err.to_string()))?
            .sample_format();
        let channels = usize::from(format.channels.max(1));
        let config = cpal::StreamConfig {
            channels: format.channels.max(1),
            sample_rate: format.sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let err_fn = |err| tracing::warn!("voice tutor playback stream error: {err}");
        let mut mono = vec![0i16; SCRATCH_FRAMES];

        let stream = match sample_format {
            cpal::SampleFormat::I16 => device.build_output_stream(
                config,
                move |data: &mut [i16], _: &_| {
                    for chunk in data.chunks_mut(SCRATCH_FRAMES * channels) {
                        let frames = chunk.len() / channels;
                        let filled = source.fill_playback(&mut mono[..frames]);
                        mono[filled..frames].fill(0);
                        fan_out_i16(&mono[..frames], channels, chunk);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::F32 => device.build_output_stream(
                config,
                move |data: &mut [f32], _: &_| {
                    for chunk in data.chunks_mut(SCRATCH_FRAMES * channels) {
                        let frames = chunk.len() / channels;
                        let filled = source.fill_playback(&mut mono[..frames]);
                        mono[filled..frames].fill(0);
                        fan_out_f32(&mono[..frames], channels, chunk);
                    }
                },
                err_fn,
                None,
            ),
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "output sample format {other:?}"
                )))
            }
        }
        .map_err(|err| classify(err.to_string()))?;

        Ok(Box::new(CpalStream {
            inner: Mutex::new(stream),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_downmix_averages_the_channels_of_each_frame() {
        let interleaved = [100i16, 200, -50, 50, 32_000, 32_000];
        let mut mono = [0i16; 3];
        assert_eq!(downmix_i16_into(&interleaved, 2, &mut mono), 3);
        assert_eq!(mono, [150, 0, 32_000]);
    }

    #[test]
    fn f32_downmix_averages_then_clamps_to_pcm16_range() {
        let interleaved = [0.5f32, 0.5, 2.0, 2.0, -2.0, -2.0];
        let mut mono = [0i16; 3];
        assert_eq!(downmix_f32_into(&interleaved, 2, &mut mono), 3);
        assert_eq!(
            mono,
            [16_383, 32_767, -32_767],
            "an out-of-range float frame must saturate, not wrap"
        );
    }

    #[test]
    fn downmix_never_writes_past_the_scratch_it_was_given() {
        let interleaved = [10i16; 12];
        let mut mono = [0i16; 2];
        assert_eq!(downmix_i16_into(&interleaved, 2, &mut mono), 2);
        assert_eq!(mono, [10, 10]);
    }

    #[test]
    fn fan_out_duplicates_each_mono_frame_across_every_channel() {
        let mono = [7i16, -8, 9];
        let mut out_i16 = [0i16; 6];
        fan_out_i16(&mono, 2, &mut out_i16);
        assert_eq!(out_i16, [7, 7, -8, -8, 9, 9]);

        let mut out_f32 = [0f32; 6];
        fan_out_f32(&mono, 2, &mut out_f32);
        assert_eq!(out_f32[0], out_f32[1]);
        assert_eq!(out_f32[4], f32_from_i16(9));
    }

    #[test]
    fn pcm16_float_round_trip_is_the_identity_within_one_step() {
        for sample in [-32_768i16, -1, 0, 1, 12_345, 32_767] {
            let back = i16_from_f32(f32_from_i16(sample));
            assert!(
                (i32::from(back) - i32::from(sample)).abs() <= 1,
                "{sample} came back as {back}"
            );
        }
    }

    #[test]
    #[ignore]
    fn manual_loopback_thirty_minutes_glitch_free() {
        use super::super::generation::{tagged_playback, Generation};
        use super::super::jitter::JitterBuffer;
        use super::super::resample::MonoResampler;
        use super::super::ring::spsc_pcm;
        use super::super::{
            frames_per_chunk, MIC_CHUNK_MS, PROVIDER_INPUT_RATE, PROVIDER_OUTPUT_RATE,
        };
        use std::time::{Duration, Instant};

        let soak_seconds: u64 = std::env::var("LOOPBACK_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1_800);

        let backend = CpalBackend;
        let input = backend.default_input_format().expect("input format");
        let output = backend.default_output_format().expect("output format");
        println!(
            "loopback rig: mic {} Hz x{} -> 16 kHz -> 24 kHz -> {} Hz x{}, {soak_seconds} s",
            input.sample_rate, input.channels, output.sample_rate, output.channels
        );

        let (mic_producer, mut mic_consumer) = spsc_pcm(input.sample_rate as usize);
        let generation = Generation::new();
        let (mut writer, source) =
            tagged_playback(generation.clone(), output.sample_rate as usize * 4, 2_048);

        let mic_stream = backend
            .open_input(input, Box::new(mic_producer))
            .expect("open input");
        let speaker_stream = backend
            .open_output(output, Box::new(source))
            .expect("open output");
        mic_stream.play().expect("start capture");
        speaker_stream.play().expect("start playback");

        let mic_chunk = frames_per_chunk(input.sample_rate, MIC_CHUNK_MS);
        let mut up =
            MonoResampler::to_provider_rate(input.sample_rate, mic_chunk).expect("up resampler");
        let mut bridge = MonoResampler::new(
            PROVIDER_INPUT_RATE,
            PROVIDER_OUTPUT_RATE,
            frames_per_chunk(PROVIDER_INPUT_RATE, MIC_CHUNK_MS),
        )
        .expect("bridge resampler");
        let mut down = MonoResampler::to_device_rate(
            output.sample_rate,
            frames_per_chunk(PROVIDER_OUTPUT_RATE, MIC_CHUNK_MS),
        )
        .expect("down resampler");
        let mut jitter = JitterBuffer::for_rate(output.sample_rate);

        let deadline = Instant::now() + Duration::from_secs(soak_seconds);
        let mut mic_buf = vec![0i16; mic_chunk * 4];
        let mut at_16k = Vec::new();
        let mut at_24k = Vec::new();
        let mut at_device = Vec::new();
        let mut pace = vec![0i16; output.sample_rate as usize / 100];
        let mut last_report = Instant::now();

        // Deadline pacing, not bare sleep(10ms): the loop body costs real time
        // (three resampler stages in a debug build ≈ 2–3 ms), and an
        // uncompensated sleep makes the pull side run ~20 % slower than the
        // capture side — the jitter memory guard then "drops" a fifth of the
        // soak by construction. The production plane paces with a tokio
        // interval whose missed ticks fire immediately (catch-up); this is
        // the thread-blocking equivalent. First-ever run of this rig (it
        // shipped as a manual item in VA1) caught it: 534 k frames dropped
        // in a 60 s smoke, underruns/seams all clean.
        let mut next_tick = Instant::now();
        while Instant::now() < deadline {
            next_tick += Duration::from_millis(10);
            if let Some(gap) = next_tick.checked_duration_since(Instant::now()) {
                std::thread::sleep(gap);
            }

            let captured = mic_consumer.pop_frames(&mut mic_buf);
            at_16k.clear();
            up.push(&mic_buf[..captured], &mut at_16k).expect("up");
            at_24k.clear();
            bridge.push(&at_16k, &mut at_24k).expect("bridge");
            at_device.clear();
            down.push(&at_24k, &mut at_device).expect("down");
            jitter.push(&at_device);

            let (paced, _outcome) = jitter.pull(&mut pace);
            writer.push_block(generation.current(), &pace[..paced]);

            if last_report.elapsed() >= Duration::from_secs(60) {
                println!(
                    "t+{}s underruns={} jitter_drops={} seam_drops={} mic_drops={} skipped={}",
                    soak_seconds - (deadline - Instant::now()).as_secs(),
                    jitter.underruns(),
                    jitter.dropped_frames(),
                    writer.dropped_frames(),
                    mic_consumer.dropped_frames(),
                    writer.skipped_frames(),
                );
                last_report = Instant::now();
            }
        }

        println!(
            "final: underruns={} jitter_drops={} seam_drops={} mic_drops={}",
            jitter.underruns(),
            jitter.dropped_frames(),
            writer.dropped_frames(),
            mic_consumer.dropped_frames(),
        );
        assert_eq!(jitter.underruns(), 0, "audible gaps during the soak");
        assert_eq!(jitter.dropped_frames(), 0, "jitter memory guard tripped");
        assert_eq!(writer.dropped_frames(), 0, "playback seam overflowed");
        assert_eq!(mic_consumer.dropped_frames(), 0, "capture seam overflowed");
    }
}
