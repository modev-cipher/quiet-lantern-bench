//! Interactive live-voice mic plane (R2): continuous open-mic capture with
//! Mute/Unmute, client-side VAD, and an echo-cancel seam.
//!
//! The input half of the 03-audio-wiring assembly, mirroring
//! [`super::output`]'s task shape (task owns the cpal stream, unbounded
//! events back-channel to the actor, dropping the handle ends the task):
//!
//! ```text
//! mic ── AudioBackend (cpal) ──► GatedCaptureSink ──► SpscProducer ─┐
//!            │                                                      │ tokio side
//!            └── Mute gate: muted means ZERO bytes           SpscConsumer
//!                leave the device handler — privacy law             │
//!                (enforced IN the capture callback)                 ▼
//!                                              EchoCanceller (Passthrough seam)
//!                                                                   │
//!                                              UpResampler ──► 16 kHz mono
//!                                                                   │
//!                                              20 ms chunks ──► MicEvent::Audio
//!                                                                   │
//!                                     actor ──► channel.send_audio (realtimeInput)
//! ```
//!
//! Owner canon: R2 is continuous open-mic Interactive mode with Mute/Unmute
//! (the PTT wording in VOICE_AGENT §6 is superseded). The VAD emits
//! [`VadSpan`] metering events as a SEAM only — no billing logic lives in
//! this crate (OSS boundary).
//!
//! Honest gaps: device death is inferred from unmuted starvation only
//! ([`MIC_STALL_TICKS`]) — cpal's error callback is not plumbed, same as
//! the output plane; MicRing (30 s replay window / `drop_through`
//! watermark) is not in the path — it belongs to the composed-channel
//! `UtteranceEnd` wiring, deferred to v1.1 (TC-VAUD-02).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::audio::device::{AudioBackend, AudioError, AudioStream, CaptureSink};
use super::audio::resample::MonoResampler;
use super::audio::ring::{spsc_pcm, SpscConsumer, SpscProducer};
use super::audio::{frames_per_chunk, frames_to_ms, MIC_CHUNK_MS, PROVIDER_INPUT_RATE};

/// Where the mic plane is, display-honest (mirrors the audio-output plane's
/// `AudioState` vocabulary): the UI renders these, nothing retries silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicState {
    /// Interactive mode not entered; no capture stream exists.
    Idle,
    /// Capture stream open, frames flowing to the channel.
    Listening,
    /// Capture gated: the stream stays open (no reopen latency) but ZERO
    /// bytes leave the device handler while muted (privacy law — enforced
    /// at the capture callback, not downstream).
    Muted,
    /// No capture device could be opened, or the device died mid-session.
    /// The session continues text-first and says so (fail-honest, VA6
    /// audio-output pattern).
    Unavailable,
}

/// A voice-activity edge from the detector, in mic-plane time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// Speech onset. While assistant audio is active this is the barge-in
    /// trigger (shared `barge_in()`, R4-2 voice half).
    SpeechStart,
    /// Speech offset, closing the span that began at the last onset. Carries
    /// the closed span — the metering seam's whole payload.
    SpeechEnd {
        span: VadSpan,
        voiceprint: Option<Voiceprint>,
    },
}

pub const VOICEPRINT_BARS: usize = 20;

const VOICEPRINT_MAX_CHUNKS: usize = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Voiceprint(pub [u8; VOICEPRINT_BARS]);

impl Voiceprint {
    pub fn from_levels(levels: &[u16]) -> Option<Self> {
        let peak = levels.iter().copied().max().filter(|&peak| peak > 0)?;
        let mut bars = [0u8; VOICEPRINT_BARS];
        for (bar, slot) in bars.iter_mut().enumerate() {
            let start = bar * levels.len() / VOICEPRINT_BARS;
            let end = ((bar + 1) * levels.len() / VOICEPRINT_BARS)
                .max(start + 1)
                .min(levels.len());
            let level = levels[start..end].iter().copied().max().unwrap_or(0);
            *slot = (u32::from(level) * 255 / u32::from(peak)) as u8;
        }
        Some(Self(bars))
    }
}

/// A closed span of detected speech — the metering seam. Consumers may
/// count these; this crate never prices, caps, or bills them (OSS boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VadSpan {
    /// Duration of the detected speech, in milliseconds of mic time.
    pub ms: u32,
}

/// What the mic task tells the session actor (same idiom as
/// `voice::output::OutputEvent`). Unbounded on the task side — the task
/// must never block on the actor; one `Audio` event per 20 ms chunk.
#[derive(Debug, PartialEq, Eq)]
pub enum MicEvent {
    /// One mic chunk, 16 kHz mono PCM16, ready for `send_audio` verbatim.
    Audio(Vec<i16>),
    /// A voice-activity edge from the detector. `SpeechStart` while the
    /// output plane is active is the actor's barge-in trigger (R4-2 voice
    /// half); `SpeechEnd` carries the closed [`VadSpan`] — the metering
    /// seam. No billing logic rides this event in this crate (OSS boundary).
    Vad(VadEvent),
    /// The mic task hit an unrecoverable seam error and ended. The actor
    /// must drop the plane and carry on — the session survives its mic
    /// (fail-honest, VA6 pattern).
    Died { detail: String },
}

/// Echo-cancel seam: mic frames plus the far-end (playback) reference in,
/// echo-reduced mic frames out. The seam exists so a real canceller can
/// drop in later without rewiring the plane; [`PassthroughEchoCanceller`]
/// proves the seam carries both signals.
pub trait EchoCanceller: Send {
    /// Process one mic chunk against the far-end reference covering the
    /// same interval. `far_end` may be empty (nothing playing). Returns the
    /// frames to forward; length must equal `mic.len()`.
    fn process(&mut self, mic: &[i16], far_end: &[i16]) -> Vec<i16>;
}

/// No-op canceller: forwards mic frames untouched. Exists to prove the
/// seam, not to cancel echo — the honest-gap register says so.
pub struct PassthroughEchoCanceller;

impl EchoCanceller for PassthroughEchoCanceller {
    fn process(&mut self, mic: &[i16], _far_end: &[i16]) -> Vec<i16> {
        mic.to_vec()
    }
}

/// Voice-activity seam: fed post-AEC mic chunks, emits speech edges. A
/// model-based detector can replace [`EnergyVad`] behind this trait later.
pub trait VoiceActivityDetector: Send {
    /// Consume one mic chunk; return the edge it produced, if any.
    fn push(&mut self, frames: &[i16]) -> Option<VadEvent>;
}

/// Consecutive voiced chunks before speech opens: 2 × 20 ms = 40 ms of
/// sustained energy, so a single click or pop is not a barge-in.
const VAD_START_CHUNKS: u32 = 2;

/// Consecutive silent chunks before speech closes: 20 × 20 ms = 400 ms
/// hangover, riding out natural intra-utterance pauses.
const VAD_END_CHUNKS: u32 = 20;

/// v1 mean-absolute-amplitude threshold (~1.5 % of full scale ≈ −36 dBFS).
/// A constant, not adaptive — a model-based detector replaces this whole
/// struct behind the trait, not this number.
const VAD_THRESHOLD_MEAN_ABS: i32 = 500;

/// v1 energy/threshold detector: a chunk is voiced when its mean absolute
/// amplitude crosses [`VAD_THRESHOLD_MEAN_ABS`]; speech opens after
/// [`VAD_START_CHUNKS`] consecutive voiced chunks and closes after
/// [`VAD_END_CHUNKS`] consecutive silent ones. The reported span runs from
/// the first triggering chunk to the last voiced chunk — the trailing
/// hangover silence is not counted (metering honesty).
///
/// Time is chunk-counted, not wall-clocked: while muted no frames arrive,
/// so an open span freezes rather than closing (honest-gap register).
pub struct EnergyVad {
    in_speech: bool,
    voiced_run: u32,
    silent_run: u32,
    span_chunks: u32,
    levels: Vec<u16>,
}

impl EnergyVad {
    pub fn new() -> Self {
        Self {
            in_speech: false,
            voiced_run: 0,
            silent_run: 0,
            span_chunks: 0,
            levels: Vec::new(),
        }
    }
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceActivityDetector for EnergyVad {
    fn push(&mut self, frames: &[i16]) -> Option<VadEvent> {
        if frames.is_empty() {
            return None;
        }
        let mean_abs = (frames
            .iter()
            .map(|&s| i64::from(s).unsigned_abs())
            .sum::<u64>()
            / frames.len() as u64) as i32;
        let voiced = mean_abs >= VAD_THRESHOLD_MEAN_ABS;
        let chunk_ms = frames_to_ms(frames.len(), PROVIDER_INPUT_RATE);
        if (self.in_speech || voiced) && self.levels.len() < VOICEPRINT_MAX_CHUNKS {
            self.levels
                .push(mean_abs.clamp(0, i32::from(u16::MAX)) as u16);
        }
        if self.in_speech {
            self.span_chunks += 1;
            if voiced {
                self.silent_run = 0;
            } else {
                self.silent_run += 1;
                if self.silent_run >= VAD_END_CHUNKS {
                    self.in_speech = false;
                    let speech_chunks = self.span_chunks.saturating_sub(self.silent_run);
                    self.span_chunks = 0;
                    self.silent_run = 0;
                    self.voiced_run = 0;
                    self.levels.truncate(speech_chunks as usize);
                    let voiceprint = Voiceprint::from_levels(&self.levels);
                    self.levels.clear();
                    return Some(VadEvent::SpeechEnd {
                        span: VadSpan {
                            ms: speech_chunks * chunk_ms,
                        },
                        voiceprint,
                    });
                }
            }
            None
        } else if voiced {
            self.voiced_run += 1;
            if self.voiced_run >= VAD_START_CHUNKS {
                self.in_speech = true;
                // The span includes the chunks that triggered the open.
                self.span_chunks = self.voiced_run;
                self.silent_run = 0;
                self.voiced_run = 0;
                return Some(VadEvent::SpeechStart);
            }
            None
        } else {
            self.voiced_run = 0;
            self.levels.clear();
            None
        }
    }
}

/// Pull cadence of the mic task, mirroring the output plane's pacing.
pub const TICK_MS: u32 = 10;

/// Capture-side SPSC ring depth: one second of device-rate audio between
/// the realtime callback and the tokio task — two orders of magnitude over
/// the 10 ms drain cadence, and drop-oldest-counted beyond it.
const CAPTURE_RING_SECONDS: usize = 1;

/// Per-pop scratch for draining the far-end tap. The tap carries frames at
/// the OUTPUT device rate, which this task doesn't know — the pop loop
/// drains whatever is there, so the scratch is just a transfer window.
const FAR_END_SCRATCH_FRAMES: usize = 4_096;

/// Consecutive UNMUTED ticks with zero captured frames before the device is
/// declared dead: two seconds. A healthy capture device delivers frames
/// continuously — silence is frames OF zeros, not an absence of frames —
/// so a starved ring while the gate is open means the callback stopped
/// (unplugged, host died). Muted ticks never count: the gate legitimately
/// starves the ring. cpal's error callback is not plumbed here (same gap as
/// the output plane's `STALL_TICKS`).
pub(crate) const MIC_STALL_TICKS: u32 = 200;

/// Grace before the FIRST frame: five seconds. Stream start-up latency on
/// some hosts is hundreds of milliseconds; a device that never produces a
/// single frame in five seconds is dead, honestly reported rather than
/// listened to forever.
const MIC_STARTUP_GRACE_TICKS: u32 = 500;

/// The realtime-callback capture gate. When muted, `on_capture` returns
/// without touching the producer: ZERO bytes leave the device handler
/// (privacy law) — nothing downstream ever sees muted audio, so nothing
/// downstream can leak it. The stream itself stays open (no reopen latency
/// on unmute; the OS mic-in-use indicator stays lit, which is honest — the
/// device IS open).
struct GatedCaptureSink {
    muted: Arc<AtomicBool>,
    producer: SpscProducer,
}

impl CaptureSink for GatedCaptureSink {
    fn on_capture(&mut self, frames: &[i16]) {
        if self.muted.load(Ordering::Relaxed) {
            return;
        }
        // Drop-on-full by construction (SpscProducer counts drops); the RT
        // thread never waits.
        self.producer.push_frames(frames);
    }
}

/// A running mic plane: the actor-side handle. Dropping it closes the
/// liveness channel; the task drains, ends, and drops the capture stream —
/// cpal closes the device (same teardown contract as `OutputPlane`).
pub struct MicPlane {
    muted: Arc<AtomicBool>,
    events: Option<mpsc::UnboundedReceiver<MicEvent>>,
    /// Held only so the task can observe every handle being gone; no
    /// messages ride it. Mute rides the atomic instead of a mailbox so the
    /// gate flips synchronously — a queued Mute would be a privacy bug.
    _liveness: mpsc::Sender<()>,
}

impl MicPlane {
    /// The task → actor event stream, takeable once (same idiom as
    /// `SessionChannel::take_events`).
    pub fn take_events(&mut self) -> Option<mpsc::UnboundedReceiver<MicEvent>> {
        self.events.take()
    }

    /// Flip the capture gate. Synchronous and instant: the next realtime
    /// callback observes the new value; muted means zero bytes leave the
    /// device handler from that callback on.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Current gate position (display state, not a data-plane promise —
    /// the promise is enforced in the callback).
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
}

/// Build and start the mic plane against `backend`'s default input device.
/// Fails honestly ([`AudioError`]) when there is no usable device — the
/// caller keeps the session alive text-first and tells the UI.
///
/// `far_end` is the output plane's `TeePlaybackSource` tap: the frames
/// actually handed to the playback device, the time-aligned far-end
/// reference the [`EchoCanceller`] seam consumes. The tap has been filling
/// (keep-oldest) since session start, so the task drains the backlog once
/// on attach — a stale reference is worse than none (AEC tee freshness,
/// PR #211 verifier note). `None` runs the seam with an empty reference.
pub fn spawn_mic_plane(
    backend: &dyn AudioBackend,
    far_end: Option<SpscConsumer>,
) -> Result<MicPlane, AudioError> {
    tracing::info!(canceller = "passthrough", "mic aec seam");
    spawn_mic_plane_with(backend, far_end, Box::new(PassthroughEchoCanceller))
}

/// [`spawn_mic_plane`] with an injectable canceller — the seam tests hand
/// in a recording fake; production always passes the Passthrough.
pub(crate) fn spawn_mic_plane_with(
    backend: &dyn AudioBackend,
    far_end: Option<SpscConsumer>,
    aec: Box<dyn EchoCanceller>,
) -> Result<MicPlane, AudioError> {
    let format = backend.default_input_format()?;
    let device_rate = format.sample_rate;
    let (producer, consumer) = spsc_pcm(device_rate as usize * CAPTURE_RING_SECONDS);
    let muted = Arc::new(AtomicBool::new(false));
    let stream = backend.open_input(
        format,
        Box::new(GatedCaptureSink {
            muted: muted.clone(),
            producer,
        }),
    )?;
    stream.play()?;
    tracing::info!(
        device_rate,
        chunk_ms = MIC_CHUNK_MS,
        far_end = far_end.is_some(),
        "mic plane spawned"
    );

    let up =
        MonoResampler::to_provider_rate(device_rate, frames_per_chunk(device_rate, MIC_CHUNK_MS))
            .map_err(|err| AudioError::UnsupportedFormat(err.to_string()))?;

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (liveness_tx, liveness_rx) = mpsc::channel(1);
    tokio::spawn(run_mic_task(
        liveness_rx,
        events_tx,
        MicChain {
            stream,
            consumer,
            far_end,
            aec,
            vad: Box::new(EnergyVad::new()),
            up,
            device_rate,
            muted: muted.clone(),
        },
    ));
    Ok(MicPlane {
        muted,
        events: Some(events_rx),
        _liveness: liveness_tx,
    })
}

/// Everything the mic task owns: the tokio-side seam chain plus the device
/// stream. Built by [`spawn_mic_plane`], consumed by [`run_mic_task`],
/// never shared.
struct MicChain {
    stream: Box<dyn AudioStream>,
    consumer: SpscConsumer,
    /// The output plane's tee tap (far-end reference), when attached.
    far_end: Option<SpscConsumer>,
    aec: Box<dyn EchoCanceller>,
    vad: Box<dyn VoiceActivityDetector>,
    up: MonoResampler,
    device_rate: u32,
    /// The capture gate, shared with [`GatedCaptureSink`]: the starvation
    /// detector must know the difference between a muted ring (legitimate)
    /// and a dead device (report it).
    muted: Arc<AtomicBool>,
}

/// The mic task: a [`TICK_MS`] interval drains the capture ring through the
/// AEC seam and the up-resampler, and forwards complete 20 ms 16 kHz chunks
/// to the actor. Ends when every [`MicPlane`] handle is gone or the actor
/// side of the event stream closes.
async fn run_mic_task(
    mut liveness: mpsc::Receiver<()>,
    events: mpsc::UnboundedSender<MicEvent>,
    chain: MicChain,
) {
    let MicChain {
        stream,
        mut consumer,
        mut far_end,
        mut aec,
        mut vad,
        mut up,
        device_rate,
        muted,
    } = chain;
    let mut seen_any_frames = false;
    let mut starved_ticks: u32 = 0;
    let chunk_16k = frames_per_chunk(PROVIDER_INPUT_RATE, MIC_CHUNK_MS);
    // One tick of device-rate frames per pop; the inner loop drains
    // whatever accumulated if a tick ran long.
    let mut scratch = vec![0i16; frames_per_chunk(device_rate, TICK_MS).max(1)];
    let mut far_scratch = vec![0i16; FAR_END_SCRATCH_FRAMES];
    let mut far_buf: Vec<i16> = Vec::new();
    let mut out_16k: Vec<i16> = Vec::with_capacity(chunk_16k * 4);
    // Drain the tap backlog: everything in it predates this mic plane, and
    // a time-lagged far-end reference is worse than none (freshness note in
    // [`spawn_mic_plane`]'s doc).
    if let Some(far) = far_end.as_mut() {
        let stale = far.available_frames();
        far.discard_frames(stale);
    }
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(u64::from(TICK_MS)));
    'run: loop {
        tokio::select! {
            // No messages ever arrive; `None` means every handle is gone.
            _ = liveness.recv() => break 'run,
            _ = interval.tick() => {
                // The far-end frames that played since the last tick, all
                // mic pops this tick share them. Seam-grade alignment: a
                // real canceller additionally needs rate matching and
                // per-frame delay estimation (honest-gap register) — the
                // passthrough only proves both signals reach the trait.
                far_buf.clear();
                if let Some(far) = far_end.as_mut() {
                    loop {
                        let n = far.pop_frames(&mut far_scratch);
                        if n == 0 {
                            break;
                        }
                        far_buf.extend_from_slice(&far_scratch[..n]);
                    }
                }
                let mut tick_frames = 0usize;
                loop {
                    let popped = consumer.pop_frames(&mut scratch);
                    if popped == 0 {
                        break;
                    }
                    tick_frames += popped;
                    let cancelled = aec.process(&scratch[..popped], &far_buf);
                    if let Err(error) = up.push(&cancelled, &mut out_16k) {
                        // A resampler seam error is unrecoverable state
                        // corruption — end honestly rather than stream
                        // garbage to the provider.
                        tracing::warn!(
                            seam = "mic",
                            reason = "resampler",
                            "mic plane ended: {error}"
                        );
                        let _ = events.send(MicEvent::Died {
                            detail: format!("mic resampler failed: {error}"),
                        });
                        break 'run;
                    }
                }
                while out_16k.len() >= chunk_16k {
                    let chunk: Vec<i16> = out_16k.drain(..chunk_16k).collect();
                    // VAD edge first, chunk second: a barge-in should never
                    // queue behind the very chunk that triggered it.
                    if let Some(edge) = vad.push(&chunk) {
                        if events.send(MicEvent::Vad(edge)).is_err() {
                            break 'run;
                        }
                    }
                    if events.send(MicEvent::Audio(chunk)).is_err() {
                        break 'run; // actor gone; teardown
                    }
                }
                // Starvation = death (module consts): a healthy device
                // delivers frames even in silence, so an open gate over an
                // empty ring means the callback stopped. Muted ticks reset
                // the count — the gate starves the ring legitimately.
                if tick_frames > 0 {
                    seen_any_frames = true;
                    starved_ticks = 0;
                } else if muted.load(Ordering::Relaxed) {
                    starved_ticks = 0;
                } else {
                    starved_ticks += 1;
                    let limit = if seen_any_frames {
                        MIC_STALL_TICKS
                    } else {
                        MIC_STARTUP_GRACE_TICKS
                    };
                    if starved_ticks >= limit {
                        tracing::warn!(
                            seam = "mic",
                            reason = if seen_any_frames { "device_stalled" } else { "no_first_frame" },
                            starved_ms = starved_ticks * TICK_MS,
                            "mic capture starved"
                        );
                        let _ = events.send(MicEvent::Died {
                            detail: if seen_any_frames {
                                format!(
                                    "the capture device stopped delivering frames \
                                     ({}+ ms with the mic unmuted)",
                                    MIC_STALL_TICKS * TICK_MS
                                )
                            } else {
                                format!(
                                    "the capture device never delivered a frame \
                                     ({}+ ms after open)",
                                    MIC_STARTUP_GRACE_TICKS * TICK_MS
                                )
                            },
                        });
                        break 'run;
                    }
                }
            }
        }
    }
    drop(stream);
}

/// Deviceless mic test rig (mirror of `voice::output::testing`): the fake
/// backend hands the test the `CaptureSink` the plane wired, so the test
/// injects frames exactly like a device callback would.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::{Arc, Mutex};

    use super::super::audio::device::{PlaybackSource, StreamFormat};
    use super::*;

    #[derive(Clone, Default)]
    pub(crate) struct FakeMicBackend {
        sink: Arc<Mutex<Option<Box<dyn CaptureSink>>>>,
        /// Set: every open fails like a missing device (a broken mic).
        refuse: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeMicBackend {
        /// From now on the "device" will not open.
        pub(crate) fn refuse_opens(&self) {
            self.refuse.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct FakeStream;

    impl AudioStream for FakeStream {
        fn play(&self) -> Result<(), AudioError> {
            Ok(())
        }
        fn pause(&self) -> Result<(), AudioError> {
            Ok(())
        }
    }

    impl AudioBackend for FakeMicBackend {
        // 16 kHz input = passthrough up-resampler, so injected samples reach
        // the chunker verbatim and assertions can match exact values.
        fn default_input_format(&self) -> Result<StreamFormat, AudioError> {
            Ok(StreamFormat::mono(PROVIDER_INPUT_RATE))
        }
        fn default_output_format(&self) -> Result<StreamFormat, AudioError> {
            Err(AudioError::NoDevice)
        }
        fn open_input(
            &self,
            _format: StreamFormat,
            sink: Box<dyn CaptureSink>,
        ) -> Result<Box<dyn AudioStream>, AudioError> {
            if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(AudioError::NoDevice);
            }
            *self.sink.lock().expect("test lock") = Some(sink);
            Ok(Box::new(FakeStream))
        }
        fn open_output(
            &self,
            _format: StreamFormat,
            _source: Box<dyn PlaybackSource>,
        ) -> Result<Box<dyn AudioStream>, AudioError> {
            Err(AudioError::NoDevice)
        }
    }

    /// Inject frames as the device callback would.
    pub(crate) fn capture(backend: &FakeMicBackend, frames: &[i16]) {
        let mut guard = backend.sink.lock().expect("test lock");
        guard
            .as_mut()
            .expect("plane opened an input")
            .on_capture(frames);
    }

    /// Let the paused-clock runtime run the mic task through `ticks` pull
    /// cycles.
    pub(crate) async fn run_ticks(ticks: u32) {
        for _ in 0..ticks {
            tokio::time::advance(std::time::Duration::from_millis(u64::from(TICK_MS))).await;
            tokio::task::yield_now().await;
        }
    }

    /// An [`EchoCanceller`] that records the (mic, far-end) lengths of every
    /// call and forwards mic verbatim — the seam proof instrument.
    #[derive(Clone, Default)]
    pub(crate) struct RecordingCanceller {
        pub(crate) calls: Arc<Mutex<Vec<(usize, usize)>>>,
    }

    impl EchoCanceller for RecordingCanceller {
        fn process(&mut self, mic: &[i16], far_end: &[i16]) -> Vec<i16> {
            self.calls
                .lock()
                .expect("test lock")
                .push((mic.len(), far_end.len()));
            mic.to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{capture, run_ticks, FakeMicBackend, RecordingCanceller};
    use super::*;

    const CHUNK: usize = frames_per_chunk(PROVIDER_INPUT_RATE, MIC_CHUNK_MS);

    fn voiced() -> Vec<i16> {
        vec![2_000; CHUNK]
    }

    fn silent() -> Vec<i16> {
        vec![0; CHUNK]
    }

    #[test]
    fn silence_produces_no_events() {
        let mut vad = EnergyVad::new();
        for _ in 0..100 {
            assert_eq!(vad.push(&silent()), None);
        }
    }

    #[test]
    fn a_single_voiced_chunk_is_a_pop_not_a_speech_start() {
        let mut vad = EnergyVad::new();
        assert_eq!(vad.push(&voiced()), None);
        // The run resets on silence: the next lone voiced chunk doesn't
        // accumulate with the first.
        assert_eq!(vad.push(&silent()), None);
        assert_eq!(vad.push(&voiced()), None);
    }

    #[test]
    fn sustained_energy_opens_speech_and_hangover_closes_it_with_the_span() {
        let mut vad = EnergyVad::new();
        assert_eq!(vad.push(&voiced()), None);
        assert_eq!(vad.push(&voiced()), Some(VadEvent::SpeechStart));
        // 8 more voiced chunks: 10 voiced total = 200 ms of speech.
        for _ in 0..8 {
            assert_eq!(vad.push(&voiced()), None);
        }
        // Hangover: silence short of the close threshold emits nothing…
        for _ in 0..(VAD_END_CHUNKS - 1) {
            assert_eq!(vad.push(&silent()), None);
        }
        // …and the closing chunk reports the span WITHOUT the hangover.
        assert_eq!(
            vad.push(&silent()),
            Some(VadEvent::SpeechEnd {
                span: VadSpan { ms: 200 },
                voiceprint: Some(Voiceprint([255; VOICEPRINT_BARS])),
            })
        );
    }

    #[test]
    fn a_voiceprint_keeps_the_shape_of_speech_and_never_its_loudness() {
        let quiet = Voiceprint::from_levels(&[500, 1_000, 250, 1_000]);
        let loud = Voiceprint::from_levels(&[5_000, 10_000, 2_500, 10_000]);
        assert_eq!(quiet, loud);
        assert_eq!(quiet.map(|print| print.0[0]), Some(127));
        assert_eq!(quiet.map(|print| print.0[VOICEPRINT_BARS - 1]), Some(255));
    }

    #[test]
    fn no_levels_or_pure_silence_yield_no_voiceprint() {
        assert_eq!(Voiceprint::from_levels(&[]), None);
        assert_eq!(Voiceprint::from_levels(&[0, 0, 0]), None);
    }

    #[test]
    fn a_long_utterance_folds_into_twenty_bars() {
        let levels: Vec<u16> = (1..=400).collect();
        let print = Voiceprint::from_levels(&levels).expect("voiced");
        assert_eq!(print.0[VOICEPRINT_BARS - 1], 255);
        assert!(print.0.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn a_pop_before_speech_does_not_leak_into_the_next_voiceprint() {
        let mut vad = EnergyVad::new();
        vad.push(&vec![30_000; CHUNK]);
        vad.push(&silent());
        vad.push(&voiced());
        assert_eq!(vad.push(&voiced()), Some(VadEvent::SpeechStart));
        for _ in 0..VAD_END_CHUNKS - 1 {
            vad.push(&silent());
        }
        assert_eq!(
            vad.push(&silent()),
            Some(VadEvent::SpeechEnd {
                span: VadSpan { ms: 40 },
                voiceprint: Some(Voiceprint([255; VOICEPRINT_BARS])),
            })
        );
    }

    #[test]
    fn an_intra_utterance_pause_shorter_than_the_hangover_does_not_split_the_span() {
        let mut vad = EnergyVad::new();
        vad.push(&voiced());
        assert_eq!(vad.push(&voiced()), Some(VadEvent::SpeechStart));
        // A 100 ms pause (5 chunks) then more speech: no close, no re-open.
        for _ in 0..5 {
            assert_eq!(vad.push(&silent()), None);
        }
        for _ in 0..3 {
            assert_eq!(vad.push(&voiced()), None);
        }
    }

    /// The voicing boundary is inclusive at 500 mean-abs (−36 dBFS), BY
    /// LITERAL VALUE: a chunk of exactly 500s is voiced; 499s are silence
    /// however sustained. Deliberately NOT written against the constant —
    /// a pin that moves with [`VAD_THRESHOLD_MEAN_ABS`] would stay green
    /// under a threshold sabotage and pin nothing. Retuning the detector
    /// means touching this test, eyes open: every span in the metering
    /// seam and every barge-in trigger changes shape with this number.
    #[test]
    fn the_voicing_threshold_boundary_is_inclusive_at_500() {
        let mut vad = EnergyVad::new();
        let at = vec![500i16; CHUNK];
        assert_eq!(vad.push(&at), None, "first voiced chunk arms the run");
        assert_eq!(
            vad.push(&at),
            Some(VadEvent::SpeechStart),
            "mean abs exactly at the threshold is voiced"
        );

        let mut vad = EnergyVad::new();
        let under = vec![499i16; CHUNK];
        for _ in 0..10 {
            assert_eq!(
                vad.push(&under),
                None,
                "one unit under the threshold is silence, however sustained"
            );
        }
    }

    /// Metering counts intra-utterance pauses INSIDE the span: a pause
    /// shorter than the hangover is part of the speech it interrupts, so
    /// the closed span covers voiced chunks AND the swallowed pause — only
    /// the trailing hangover is excluded. 2 voiced + 5 pause + 3 voiced =
    /// 10 chunks = 200 ms, whatever the hangover added after.
    #[test]
    fn an_intra_utterance_pause_is_metered_inside_the_span() {
        let mut vad = EnergyVad::new();
        vad.push(&voiced());
        assert_eq!(vad.push(&voiced()), Some(VadEvent::SpeechStart));
        for _ in 0..5 {
            assert_eq!(vad.push(&silent()), None);
        }
        for _ in 0..3 {
            assert_eq!(vad.push(&voiced()), None);
        }
        for _ in 0..(VAD_END_CHUNKS - 1) {
            assert_eq!(vad.push(&silent()), None);
        }
        assert_eq!(
            vad.push(&silent()),
            Some(VadEvent::SpeechEnd {
                span: VadSpan { ms: 200 },
                voiceprint: Some(Voiceprint([
                    255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255
                ])),
            }),
            "the swallowed pause is span time; the trailing hangover is not"
        );
    }

    /// One onset per span, however long the user speaks: after SpeechStart,
    /// sustained voice emits NO second edge until a close intervenes — the
    /// once-only law the actor's barge-in relies on (a re-firing SpeechStart
    /// would re-interrupt on every chunk).
    #[test]
    fn a_span_emits_speech_start_exactly_once_until_it_closes() {
        let mut vad = EnergyVad::new();
        vad.push(&voiced());
        assert_eq!(vad.push(&voiced()), Some(VadEvent::SpeechStart));
        // Five seconds of continuous speech: silence on the edge channel.
        for _ in 0..250 {
            assert_eq!(vad.push(&voiced()), None);
        }
        // Close, then a fresh utterance re-arms a fresh onset.
        for _ in 0..VAD_END_CHUNKS {
            let _ = vad.push(&silent());
        }
        vad.push(&voiced());
        assert_eq!(
            vad.push(&voiced()),
            Some(VadEvent::SpeechStart),
            "a closed span re-arms the next onset"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn captured_frames_come_out_as_16k_chunks_through_the_seams() {
        let backend = FakeMicBackend::default();
        let mut plane = spawn_mic_plane(&backend, None).expect("fake backend opens");
        let mut events = plane.take_events().expect("fresh plane");
        run_ticks(1).await; // interval's immediate first tick
        capture(&backend, &vec![7i16; CHUNK * 2]);
        run_ticks(2).await;
        assert_eq!(
            events.try_recv().expect("first chunk"),
            MicEvent::Audio(vec![7i16; CHUNK])
        );
        assert_eq!(
            events.try_recv().expect("second chunk"),
            MicEvent::Audio(vec![7i16; CHUNK])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_far_end_reference_reaches_the_canceller_and_the_stale_backlog_does_not() {
        let backend = FakeMicBackend::default();
        let (mut far_producer, far_consumer) = spsc_pcm(48_000);
        // Everything in the tap before the mic plane attaches predates it —
        // a time-lagged reference is worse than none (PR #211 verifier
        // note), so the task must drain it, not feed it to the canceller.
        far_producer.push_frames(&[9i16; 1_000]);
        let recorder = RecordingCanceller::default();
        let calls = recorder.calls.clone();
        let mut plane = spawn_mic_plane_with(&backend, Some(far_consumer), Box::new(recorder))
            .expect("fake backend opens");
        let _events = plane.take_events();
        run_ticks(1).await;
        capture(&backend, &vec![7i16; CHUNK]);
        run_ticks(1).await;
        {
            let seen = calls.lock().expect("test lock");
            assert!(!seen.is_empty(), "mic frames reached the canceller");
            assert!(
                seen.iter().all(|&(_, far)| far == 0),
                "the stale backlog must never reach the canceller: {seen:?}"
            );
        }
        // Fresh far-end frames — played since the last tick — do arrive.
        far_producer.push_frames(&[3i16; 480]);
        capture(&backend, &vec![7i16; CHUNK]);
        run_ticks(1).await;
        let seen = calls.lock().expect("test lock");
        assert!(
            seen.iter().any(|&(mic, far)| mic > 0 && far == 480),
            "fresh far-end frames must ride the seam alongside mic frames: {seen:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_never_delivers_a_frame_dies_honestly_after_the_grace() {
        let backend = FakeMicBackend::default();
        let mut plane = spawn_mic_plane(&backend, None).expect("fake backend opens");
        let mut events = plane.take_events().expect("fresh plane");
        run_ticks(MIC_STARTUP_GRACE_TICKS + 2).await;
        match events.try_recv() {
            Ok(MicEvent::Died { detail }) => {
                assert!(
                    detail.contains("never delivered"),
                    "honest detail: {detail}"
                )
            }
            other => panic!("expected Died, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_stops_mid_session_dies_after_the_stall_window() {
        let backend = FakeMicBackend::default();
        let mut plane = spawn_mic_plane(&backend, None).expect("fake backend opens");
        let mut events = plane.take_events().expect("fresh plane");
        run_ticks(1).await;
        capture(&backend, &vec![7i16; CHUNK]);
        run_ticks(1).await;
        assert_eq!(
            events.try_recv().expect("live chunk"),
            MicEvent::Audio(vec![7i16; CHUNK])
        );
        run_ticks(MIC_STALL_TICKS + 2).await;
        match events.try_recv() {
            Ok(MicEvent::Died { detail }) => assert!(
                detail.contains("stopped delivering"),
                "honest detail: {detail}"
            ),
            other => panic!("expected Died, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_muted_mic_starving_the_ring_is_not_a_dead_device() {
        let backend = FakeMicBackend::default();
        let mut plane = spawn_mic_plane(&backend, None).expect("fake backend opens");
        let mut events = plane.take_events().expect("fresh plane");
        run_ticks(1).await;
        capture(&backend, &vec![7i16; CHUNK]);
        run_ticks(1).await;
        let _ = events.try_recv().expect("live chunk");
        // Muted for far longer than the stall window: the gate starves the
        // ring legitimately; no death, and unmuting resumes the flow.
        plane.set_muted(true);
        run_ticks(MIC_STALL_TICKS * 3).await;
        assert!(events.try_recv().is_err(), "no event while muted");
        plane.set_muted(false);
        capture(&backend, &vec![5i16; CHUNK]);
        run_ticks(1).await;
        assert_eq!(
            events.try_recv().expect("post-unmute chunk"),
            MicEvent::Audio(vec![5i16; CHUNK])
        );
    }

    /// The Passthrough is byte-transparent — the baseline a real canceller
    /// (sonora / webrtc-audio-processing, its own slice) will be diffed
    /// against. Arbitrary mic content comes out identical, far-end present
    /// or not; the far-end reference never bleeds into the output.
    #[test]
    fn the_passthrough_canceller_is_byte_transparent() {
        let mut aec = PassthroughEchoCanceller;
        let mic: Vec<i16> = (0..320).map(|i| (i * 37 % 1_000 - 500) as i16).collect();
        assert_eq!(aec.process(&mic, &[]), mic, "transparent with no far end");
        assert_eq!(
            aec.process(&mic, &[9_999i16; 480]),
            mic,
            "transparent with a far-end reference present"
        );
        assert_eq!(aec.process(&[], &[1i16; 10]), Vec::<i16>::new());
    }

    /// An [`EchoCanceller`] that returns silence — the seam-ORDER
    /// instrument: if anything downstream (VAD, chunker, wire) still sees
    /// voice, it is reading raw mic frames instead of the canceller's
    /// output.
    struct MutingCanceller;

    impl EchoCanceller for MutingCanceller {
        fn process(&mut self, mic: &[i16], _far_end: &[i16]) -> Vec<i16> {
            vec![0; mic.len()]
        }
    }

    /// The seam ORDER pin: the canceller runs BEFORE the VAD and BEFORE the
    /// chunks that ride to the actor — the design's choice (a VAD reading
    /// raw mic frames would barge in on the tutor's own echo, defeating the
    /// canceller's whole purpose). With a canceller that mutes everything,
    /// sustained voice produces zero VAD edges and zero-valued chunks. A
    /// reorder that feeds the VAD or the resampler raw frames reds here.
    #[tokio::test(start_paused = true)]
    async fn the_canceller_runs_before_the_vad_and_the_chunker() {
        let backend = FakeMicBackend::default();
        let mut plane = spawn_mic_plane_with(&backend, None, Box::new(MutingCanceller))
            .expect("fake backend opens");
        let mut events = plane.take_events().expect("fresh plane");
        run_ticks(1).await;
        // Loud, sustained speech — many chunks past the VAD open threshold.
        capture(&backend, &vec![2_000i16; CHUNK * 10]);
        run_ticks(3).await;
        let mut audio_chunks = 0;
        while let Ok(event) = events.try_recv() {
            match event {
                MicEvent::Audio(pcm) => {
                    audio_chunks += 1;
                    assert!(
                        pcm.iter().all(|&s| s == 0),
                        "the wire must carry the canceller's output, not raw mic"
                    );
                }
                MicEvent::Vad(edge) => {
                    panic!("the VAD must hear post-AEC frames; raw voice leaked: {edge:?}")
                }
                MicEvent::Died { detail } => panic!("unexpected death: {detail}"),
            }
        }
        assert_eq!(audio_chunks, 10, "every chunk crossed, all silenced");
    }

    /// Far-end freshness holds ACROSS a muted stretch: the tick loop drains
    /// the tap even when no mic frames flow, so the first canceller call
    /// after unmute sees only the far-end frames played since the last
    /// tick — never the muted-window backlog (the PR #211 stale-reference
    /// rule, extended over Mute).
    #[tokio::test(start_paused = true)]
    async fn the_muted_window_far_end_backlog_never_reaches_the_canceller() {
        let backend = FakeMicBackend::default();
        let (mut far_producer, far_consumer) = spsc_pcm(48_000);
        let recorder = RecordingCanceller::default();
        let calls = recorder.calls.clone();
        let mut plane = spawn_mic_plane_with(&backend, Some(far_consumer), Box::new(recorder))
            .expect("fake backend opens");
        let _events = plane.take_events();
        run_ticks(1).await;

        // A long muted stretch while the tutor keeps talking: 10 000
        // far-end frames arrive during the mute, drained tick by tick.
        plane.set_muted(true);
        for _ in 0..10 {
            far_producer.push_frames(&[9i16; 1_000]);
            run_ticks(2).await;
        }
        assert!(
            calls.lock().expect("test lock").is_empty(),
            "no mic frames while muted, so no canceller calls"
        );

        // Unmute; 480 fresh far-end frames land in the same tick window as
        // the first mic chunk. The canceller must see exactly those.
        plane.set_muted(false);
        far_producer.push_frames(&[3i16; 480]);
        capture(&backend, &vec![7i16; CHUNK]);
        run_ticks(1).await;
        let seen = calls.lock().expect("test lock");
        assert!(
            seen.iter().any(|&(mic, far)| mic > 0 && far == 480),
            "the first post-unmute call sees only the fresh window: {seen:?}"
        );
        assert!(
            seen.iter().all(|&(_, far)| far <= 480),
            "the muted-window backlog (10 000 frames) must be gone: {seen:?}"
        );
    }

    /// Metering honesty across Mute: an open VAD span freezes (documented
    /// gap — chunk-counted time, no frames while muted) and the muted
    /// window contributes ZERO ms to the closed span. However long the user
    /// stays muted mid-utterance, the meter sees only chunks that actually
    /// crossed the gate — mute time can never be metered, and no VAD edge
    /// fires while the gate is closed.
    #[tokio::test(start_paused = true)]
    async fn a_span_frozen_across_mute_meters_only_the_chunks_that_crossed() {
        let backend = FakeMicBackend::default();
        let mut plane = spawn_mic_plane(&backend, None).expect("fake backend opens");
        let mut events = plane.take_events().expect("fresh plane");
        run_ticks(1).await;
        // Two voiced chunks open the span (40 ms of real speech).
        capture(&backend, &vec![2_000i16; CHUNK * 2]);
        run_ticks(2).await;
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event);
        }
        assert!(
            seen.contains(&MicEvent::Vad(VadEvent::SpeechStart)),
            "the span opened: {seen:?}"
        );

        // Mute mid-span, far past the hangover in wall time: no close, no
        // edge, no event of any kind — the span is frozen, not ended.
        plane.set_muted(true);
        capture(&backend, &vec![2_000i16; CHUNK * 40]);
        run_ticks(VAD_END_CHUNKS * 4).await;
        assert!(
            events.try_recv().is_err(),
            "a muted mic emits nothing — the open span freezes"
        );

        // Unmute into the hangover: 20 silent chunks close the span, and
        // the meter reads 40 ms — the two real chunks, not the mute.
        plane.set_muted(false);
        capture(&backend, &vec![0i16; CHUNK * VAD_END_CHUNKS as usize]);
        run_ticks(3).await;
        let mut span_ms = None;
        while let Ok(event) = events.try_recv() {
            if let MicEvent::Vad(VadEvent::SpeechEnd { span, .. }) = event {
                span_ms = Some(span.ms);
            }
        }
        assert_eq!(
            span_ms,
            Some(40),
            "the muted window contributes zero metered milliseconds"
        );
    }

    #[test]
    fn the_muted_capture_gate_pushes_zero_frames() {
        let (producer, mut consumer) = spsc_pcm(PROVIDER_INPUT_RATE as usize);
        let muted = Arc::new(AtomicBool::new(true));
        let mut sink = GatedCaptureSink {
            muted: muted.clone(),
            producer,
        };
        sink.on_capture(&voiced());
        let mut out = vec![0i16; CHUNK];
        assert_eq!(
            consumer.pop_frames(&mut out),
            0,
            "muted frames must not cross"
        );
        // Unmute: the very next callback flows.
        muted.store(false, Ordering::Relaxed);
        sink.on_capture(&voiced());
        assert_eq!(consumer.pop_frames(&mut out), CHUNK);
    }
}
