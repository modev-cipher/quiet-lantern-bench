//! Voice-tutor audio I/O core (VA1, VOICE_AGENT.md §2 data plane, §6 audio notes).
//!
//! Layering, from the OS realtime thread inwards:
//!
//! ```text
//! mic  ── AudioBackend (cpal) ──► CaptureSink ──► SpscProducer ─┐
//!                                                               │ tokio side
//!                                                     SpscConsumer ──► MicRing (30 s,
//!                                                               │        drop-oldest)
//!                                                               └──► UpResampler ──► 16 kHz
//!
//! 24 kHz ──► DownResampler ──► JitterBuffer ──► SpscProducer ─┐
//!                                                             │ realtime thread
//!  speaker ◄── AudioBackend (cpal) ◄── PlaybackSource ◄── SpscConsumer + Generation
//! ```
//!
//! Everything above [`device::AudioBackend`] is device-free and deterministic, so the
//! rings, resamplers, jitter buffer and barge-in counter are all unit-testable with
//! synthetic buffers and simulated time. [`cpal_backend::CpalBackend`] is the one real
//! implementation of that trait and the only file here allowed to name cpal; its
//! module header carries the seam-by-seam wiring and the instructions for the manual
//! loopback rig (ROADMAP note ³ pattern) that exercises the real-device path.

pub mod cpal_backend;
pub mod device;
pub mod generation;
pub mod jitter;
pub mod resample;
pub mod ring;

/// Sample rate every channel receives mic audio at (VOICE_AGENT §2, §3 `send_audio`).
pub const PROVIDER_INPUT_RATE: u32 = 16_000;

/// Sample rate provider response audio arrives at (VOICE_AGENT §3.1, Gemini Live
/// 16 kHz in / 24 kHz out; OpenAI Realtime PCM16 24 kHz).
pub const PROVIDER_OUTPUT_RATE: u32 = 24_000;

/// Mic chunking granularity of the latency budget (VOICE_AGENT §2 rule 4).
pub const MIC_CHUNK_MS: u32 = 20;

/// Bounded mic replay window (VOICE_AGENT §2 rule 3, §9 transient-drop row).
pub const MIC_REPLAY_SECONDS: u32 = 30;

/// Frames of 16 kHz mono audio in one mic chunk.
pub const fn frames_per_chunk(rate: u32, chunk_ms: u32) -> usize {
    (rate as usize * chunk_ms as usize) / 1000
}

/// Milliseconds of audio a frame count represents at `rate`, truncating.
/// u64 intermediates: whole-reply figures (120 s × 48 kHz, #209) overflow
/// 32-bit arithmetic, so the math must not ride a 32-bit `usize`.
pub const fn frames_to_ms(frames: usize, rate: u32) -> u32 {
    ((frames as u64 * 1000) / rate as u64) as u32
}

/// Frames of audio `ms` represents at `rate`, truncating. Same u64
/// intermediate rationale as [`frames_to_ms`].
pub const fn ms_to_frames(ms: u32, rate: u32) -> usize {
    ((ms as u64 * rate as u64) / 1000) as usize
}
