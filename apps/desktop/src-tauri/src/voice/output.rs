//! VA6 audio-output plane: the tokio task that makes live sessions audible.
//!
//! Assembly of existing VA1 components (all proven deviceless in
//! [`super::audio`]); the wiring diagram is the one in
//! [`super::audio::cpal_backend`]'s module doc, output half:
//!
//! ```text
//! ChannelEvent::AudioDelta (pcm @ 24 kHz, gen)
//!   ─mpsc─► output task: MonoResampler::to_device_rate ─► JitterBuffer
//!             ─► TaggedPlaybackWriter
//!   ─rtrb─► GatedPlaybackSource (─► echo-reference tee, when attached)
//!             ─► AudioBackend::open_output ─► speaker
//! ```
//!
//! The task is paced exactly like the proven loopback rig in
//! [`super::audio::cpal_backend`]: a [`TICK_MS`] interval pulls one tick's
//! worth of frames from the jitter buffer into the playback ring, so the
//! jitter buffer's frame-count clock tracks wall time and its adaptation
//! logic sees the same cadence the device drains at.
//!
//! Barge-in (VOICE_AGENT §2 rule 2) is layered so it never depends on any
//! single signal arriving:
//!
//! 1. The actor does `Generation::bump` and `channel.interrupt()`, and nudges
//!    this plane with [`OutputPlane::barge_in`].
//! 2. Frames are tagged with the generation they *arrived* under
//!    (`jitter_gen`), so anything pulled between the actor's bump and this
//!    task's flush is refused by [`TaggedPlaybackWriter::push_block`] — the
//!    window where stale audio could ride a fresh tag is closed by
//!    construction, not by timing.
//! 3. The task also detects the bump itself (generation is a shared atomic)
//!    on the next delta or tick and performs `JitterBuffer::flush` →
//!    `MonoResampler::reset` even if the [`OutputCommand::BargeIn`] nudge was
//!    lost to a full mailbox.
//! 4. Whatever already crossed into the playback ring is refused at drain by
//!    [`GatedPlaybackSource`] — the ≤ 20 ms bound proven in
//!    [`super::audio::generation`].
//!
//! Teardown: the output *task* owns the device stream (`dyn AudioStream` is
//! `Send` but not `Sync`); dropping [`OutputPlane`] closes the mailbox, the
//! task drains, ends, and drops the stream — cpal closes the device. The
//! actor owns the plane, so `Stop`/actor-end tears audio down with the
//! session.
//!
//! Full-duplex seam (owner ruling R2): the next slice is a continuous
//! open-mic session with acoustic echo cancellation, which needs the frames
//! *actually handed to the device* as a time-aligned far-end reference.
//! [`TeePlaybackSource`] is that seam — it wraps the gated source in the
//! realtime callback and mirrors every filled frame into an optional SPSC
//! ring (`rtrb` push, RT-safe). Nothing in this module assumes push-to-talk
//! or half-duplex: the plane runs whether or not a mic plane exists.

use tokio::sync::mpsc;

use super::audio::device::{AudioBackend, AudioError, AudioStream, PlaybackSource};
use super::audio::generation::{tagged_playback, GatedPlaybackSource, Generation};
use super::audio::jitter::JitterBuffer;
use super::audio::resample::MonoResampler;
use super::audio::ring::SpscProducer;
use super::audio::{frames_per_chunk, ms_to_frames, MIC_CHUNK_MS, PROVIDER_OUTPUT_RATE};

/// Messages the session actor sends into the output task.
#[derive(Debug)]
pub enum OutputCommand {
    /// Provider response PCM at [`PROVIDER_OUTPUT_RATE`], tagged with its
    /// response generation for the stale-audio gate.
    Pcm { pcm: Vec<i16>, gen: u64 },
    /// Barge-in nudge: flush the jitter buffer and reset the down-resampler
    /// now rather than on the next tick. Ordering and correctness do not
    /// depend on this arriving (module doc, layering) — it trims up to one
    /// tick of latency off the flush.
    BargeIn,
}

/// What the output task tells the session actor (the reverse direction of
/// [`OutputCommand`]). Unbounded sender on the task side: at most one event
/// per tick edge, and the task must never block on the actor.
#[derive(Debug, PartialEq, Eq)]
pub enum OutputEvent {
    /// The device is (or stopped) actually playing response audio — the
    /// jitter buffer's playing flag, sampled per tick. Edge-triggered.
    Speaking(bool),
    /// The device stopped draining ([`STALL_TICKS`] of refused pushes with
    /// audio queued). The task has already torn the stream down; the actor
    /// must drop the plane and carry on text-first.
    Died { detail: String },
    /// The jitter buffer's memory guard refused frames: the response is
    /// longer than [`super::audio::jitter::CAPACITY_MS`] and its tail is
    /// being dropped. A backstop that should never fire on a sane provider
    /// (#209) — emitted once per response, sized by the first refusal, so
    /// the figure is an *at least*: later refusals in the same response are
    /// counted in the ledger but not re-reported.
    Truncated { dropped_ms: u32 },
}

/// Depth of the actor → output-task mailbox. Each message is one provider
/// delta (~hundreds of ms of audio); 32 is seconds of headroom before
/// backpressure, and backpressure here means "drop the delta and count it",
/// never "stall the actor loop".
pub const OUTPUT_MAILBOX_DEPTH: usize = 32;

/// Consecutive ticks of "audio ready but the playback ring refused it all"
/// before the device is declared dead: one full second. A live callback
/// drains the ring hundreds of times over in that window, so a false
/// positive would need the device to stall a whole second — at which point
/// the audio experience is gone anyway.
///
/// Honest limit: a response shorter than the playback ring (~4 s) fits
/// entirely, so a device that dies silently mid-short-response is only
/// detected when a later, longer backlog jams. cpal's error callback is not
/// plumbed to this task (gap register).
pub const STALL_TICKS: u32 = 100;

/// Pull cadence of the output task, mirroring the loopback rig's pacing.
pub const TICK_MS: u32 = 10;

/// Playback-ring sizing carried over from the proven loopback rig: four
/// seconds of PCM and 2 048 outstanding tags.
const PLAYBACK_RING_SECONDS: usize = 4;
const PLAYBACK_MAX_BLOCKS: usize = 2_048;

/// The realtime-callback tee for the AEC far-end reference (module doc,
/// full-duplex seam). Wraps [`GatedPlaybackSource`]; when a tap is attached,
/// every frame written to the device buffer is mirrored into it.
pub struct TeePlaybackSource {
    inner: GatedPlaybackSource,
    tap: Option<SpscProducer>,
}

impl TeePlaybackSource {
    pub fn new(inner: GatedPlaybackSource, tap: Option<SpscProducer>) -> Self {
        Self { inner, tap }
    }
}

impl PlaybackSource for TeePlaybackSource {
    fn fill_playback(&mut self, out: &mut [i16]) -> usize {
        let filled = self.inner.fill_playback(out);
        if let Some(tap) = &mut self.tap {
            // Drop-on-full by construction (SpscProducer counts drops); the
            // reference consumer owns keeping up, the RT thread never waits.
            tap.push_frames(&out[..filled]);
        }
        filled
    }
}

/// A running output plane: the actor-side handle (module doc, teardown).
pub struct OutputPlane {
    commands: mpsc::Sender<OutputCommand>,
    events: Option<mpsc::UnboundedReceiver<OutputEvent>>,
}

impl OutputPlane {
    /// The task → actor event stream, takeable once (same idiom as
    /// `SessionChannel::take_events`).
    pub fn take_events(&mut self) -> Option<mpsc::UnboundedReceiver<OutputEvent>> {
        self.events.take()
    }
    /// Forward one provider delta. Non-blocking: if the mailbox is full the
    /// delta is dropped (the jitter buffer's adaptive target is the real
    /// smoothing; a full mailbox means the task or device stopped draining).
    pub fn forward(&self, pcm: Vec<i16>, gen: u64) {
        let _ = self.commands.try_send(OutputCommand::Pcm { pcm, gen });
    }

    /// Barge-in nudge, sent by the actor's `Interrupt` handler after
    /// `Generation::bump`, before `channel.interrupt()`. Best-effort by
    /// design — see the module doc's barge-in layering.
    pub fn barge_in(&self) {
        let _ = self.commands.try_send(OutputCommand::BargeIn);
    }
}

/// Build and start the output plane against `backend`'s default output
/// device. Fails honestly ([`AudioError`]) when there is no usable device —
/// the caller keeps the session alive text-first (§3's no-mic path already
/// covers a session with no audio).
pub fn spawn_output_plane(
    backend: &dyn AudioBackend,
    generation: Generation,
    echo_tap: Option<SpscProducer>,
) -> Result<OutputPlane, AudioError> {
    let format = backend.default_output_format()?;
    let device_rate = format.sample_rate;
    let echo_tap_attached = echo_tap.is_some();
    let (writer, source) = tagged_playback(
        generation.clone(),
        device_rate as usize * PLAYBACK_RING_SECONDS,
        PLAYBACK_MAX_BLOCKS,
    );
    let stream = backend.open_output(format, Box::new(TeePlaybackSource::new(source, echo_tap)))?;
    stream.play()?;
    tracing::info!(
        device_rate,
        echo_tap = echo_tap_attached,
        "voice output plane spawned"
    );

    let down = MonoResampler::to_device_rate(
        device_rate,
        frames_per_chunk(PROVIDER_OUTPUT_RATE, MIC_CHUNK_MS),
    )
    .map_err(|err| AudioError::UnsupportedFormat(err.to_string()))?;
    let jitter = JitterBuffer::for_rate(device_rate);

    let (commands_tx, commands_rx) = mpsc::channel(OUTPUT_MAILBOX_DEPTH);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_output_task(
        commands_rx,
        events_tx,
        OutputChain {
            stream,
            down,
            jitter,
            writer,
            generation,
            device_rate,
        },
    ));
    Ok(OutputPlane {
        commands: commands_tx,
        events: Some(events_rx),
    })
}

/// Everything the output task owns: the tokio-side seam chain plus the
/// device stream. Built by [`spawn_output_plane`], consumed by
/// [`run_output_task`], never shared.
struct OutputChain {
    stream: Box<dyn AudioStream>,
    down: MonoResampler,
    jitter: JitterBuffer,
    writer: super::audio::generation::TaggedPlaybackWriter,
    generation: Generation,
    device_rate: u32,
}

/// The output task (module doc). Owns the whole tokio-side seam chain plus
/// the device stream; ends when every [`OutputPlane`] handle is gone.
async fn run_output_task(
    mut commands: mpsc::Receiver<OutputCommand>,
    events: mpsc::UnboundedSender<OutputEvent>,
    chain: OutputChain,
) {
    let OutputChain {
        stream,
        mut down,
        mut jitter,
        mut writer,
        generation,
        device_rate,
    } = chain;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(u64::from(TICK_MS)));
    // The generation the audio inside `jitter` (and `down`'s pending tail)
    // arrived under. Pulls are tagged with THIS, never `generation.current()`,
    // so pre-barge-in audio can never ride a post-barge-in tag (module doc,
    // barge-in layering, point 2).
    let mut jitter_gen = generation.current();
    let mut pace = vec![0i16; ms_to_frames(TICK_MS, device_rate)];
    let mut at_device: Vec<i16> = Vec::new();
    let mut was_speaking = false;
    let mut stalled_ticks: u32 = 0;
    // The response generation that already got its cap-hit warning, so a
    // guard trip is loud exactly once per response instead of once per
    // delta (#209: the bug class is silence, the fix is not log spam).
    let mut truncation_warned_gen: Option<u64> = None;
    let mut gen_first_delta: Option<(u64, std::time::Instant)> = None;

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break,
                Some(OutputCommand::Pcm { pcm, gen }) => {
                    if gen != generation.current() {
                        // Stale before it ever reached the resampler; the
                        // writer-side counters only see what got queued, so
                        // trace here for the honest ledger.
                        tracing::trace!(
                            "voice output: dropped {} stale samples (gen {gen})",
                            pcm.len()
                        );
                        continue;
                    }
                    if gen != jitter_gen {
                        // First delta of a new response: anything older in
                        // the chain is dead by definition.
                        flush_chain(&mut jitter, &mut down);
                        jitter_gen = gen;
                    }
                    if gen_first_delta.map_or(true, |(g, _)| g != gen) {
                        gen_first_delta = Some((gen, std::time::Instant::now()));
                    }
                    at_device.clear();
                    match down.push(&pcm, &mut at_device) {
                        Ok(_) => {
                            let refused = jitter.push(&at_device);
                            if refused > 0 && truncation_warned_gen != Some(jitter_gen) {
                                truncation_warned_gen = Some(jitter_gen);
                                // The memory guard is a backstop, never a
                                // governor: a trip means the provider outran
                                // a whole-reply-sized buffer, and the
                                // listener is losing this response's tail.
                                // Silent eating was #209; this path stays
                                // loud.
                                tracing::warn!(
                                    "voice output: jitter memory guard hit — dropping this \
                                     response's tail (at least {refused} frames refused, \
                                     capacity {} ms, {} frames dropped since the plane opened)",
                                    jitter.config().capacity_ms,
                                    jitter.dropped_frames(),
                                );
                                let _ = events.send(OutputEvent::Truncated {
                                    dropped_ms: super::audio::frames_to_ms(refused, device_rate),
                                });
                            }
                        }
                        Err(err) => {
                            tracing::warn!("voice output: resample failed: {err}");
                        }
                    }
                }
                Some(OutputCommand::BargeIn) => {
                    flush_chain(&mut jitter, &mut down);
                    jitter_gen = generation.current();
                }
            },
            _ = tick.tick() => {
                if jitter_gen != generation.current() {
                    // The actor bumped and the nudge hasn't landed (or was
                    // lost): self-detect (module doc, barge-in layering,
                    // point 3). Pending stale audio is dropped before it can
                    // be pulled; the ring-side gate already refuses what
                    // crossed earlier.
                    flush_chain(&mut jitter, &mut down);
                    jitter_gen = generation.current();
                }
                let (paced, _outcome) = jitter.pull(&mut pace);
                if paced > 0 {
                    let accepted = writer.push_block(jitter_gen, &pace[..paced]);
                    if accepted > 0 {
                        if let Some((gen, at)) = gen_first_delta.take_if(|(g, _)| *g == jitter_gen)
                        {
                            tracing::info!(
                                gen,
                                since_first_delta_ms = at.elapsed().as_millis() as u64,
                                "voice output: first sample of generation at device"
                            );
                        }
                    }
                    // Audio is ready and the ring refused every frame: the
                    // callback stopped draining. One second of that is a
                    // dead device (STALL_TICKS doc), not a hiccup.
                    stalled_ticks = if accepted == 0 { stalled_ticks + 1 } else { 0 };
                    if stalled_ticks >= STALL_TICKS {
                        tracing::warn!(
                            seam = "audio_out",
                            reason = "ring_stalled",
                            stalled_ms = STALL_TICKS * TICK_MS,
                            "voice output: device stopped draining"
                        );
                        let _ = events.send(OutputEvent::Died {
                            detail: format!(
                                "playback ring refused audio for {STALL_TICKS} ticks; \
                                 the output device stopped draining"
                            ),
                        });
                        break;
                    }
                }
                let speaking = jitter.is_playing();
                if speaking != was_speaking {
                    was_speaking = speaking;
                    tracing::debug!(gen = jitter_gen, speaking, "voice output: speaking edge");
                    if !speaking {
                        // Re-arm the guard warning: generations only advance
                        // on barge-in, so consecutive typed turns share one —
                        // "once per response" must key on this end edge, not
                        // on the generation.
                        truncation_warned_gen = None;
                        // Response-end ledger: the only live window into the
                        // jitter buffer's adaptation (target drift, the
                        // planned turn-boundary underrun, drop counters).
                        tracing::debug!(
                            "voice output: response end: target_ms={} underruns={} \
                             underrun_frames={} raises={} lowers={} primed_silence={} \
                             dropped={} pulled={}",
                            jitter.target_ms(),
                            jitter.underruns(),
                            jitter.underrun_frames(),
                            jitter.target_raises(),
                            jitter.target_lowers(),
                            jitter.primed_silence_frames(),
                            jitter.dropped_frames(),
                            jitter.frames_pulled(),
                        );
                    }
                    let _ = events.send(OutputEvent::Speaking(speaking));
                }
            }
        }
    }
    // Explicit: the device closes when the task ends, not before.
    drop(stream);
}

/// Barge-in steps 2+3 of the pinned order (`bump → flush → reset →
/// interrupt`): drop queued response audio, forget resampler history so the
/// next response doesn't open on a stale filter tail.
fn flush_chain(jitter: &mut JitterBuffer, down: &mut MonoResampler) {
    let dropped = jitter.flush();
    down.reset();
    if dropped > 0 {
        tracing::debug!("voice output: barge-in flushed {dropped} queued frames");
    }
}

/// Deviceless test fixtures, shared with the session-actor rig in
/// [`crate::voice_session`] so the actor → plane → "device" path is testable
/// end to end without a sound server.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::{Arc, Mutex};

    use super::super::audio::device::{CaptureSink, StreamFormat};
    use super::*;

    /// A deviceless backend: hands the test the `PlaybackSource` the plane
    /// wired, so the test drains it exactly like a device callback would.
    #[derive(Clone, Default)]
    pub(crate) struct FakeBackend {
        source: Arc<Mutex<Option<Box<dyn PlaybackSource>>>>,
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

    impl AudioBackend for FakeBackend {
        fn default_input_format(&self) -> Result<StreamFormat, AudioError> {
            Err(AudioError::NoDevice)
        }
        // 24 kHz output = passthrough resampler, so pushed samples come out
        // of the device side verbatim and assertions can match exact values.
        fn default_output_format(&self) -> Result<StreamFormat, AudioError> {
            Ok(StreamFormat::mono(PROVIDER_OUTPUT_RATE))
        }
        fn open_input(
            &self,
            _format: StreamFormat,
            _sink: Box<dyn CaptureSink>,
        ) -> Result<Box<dyn AudioStream>, AudioError> {
            Err(AudioError::NoDevice)
        }
        fn open_output(
            &self,
            _format: StreamFormat,
            source: Box<dyn PlaybackSource>,
        ) -> Result<Box<dyn AudioStream>, AudioError> {
            *self.source.lock().expect("test lock") = Some(source);
            Ok(Box::new(FakeStream))
        }
    }

    /// Drain up to `frames` from the captured device side, returning only
    /// the really-filled samples.
    pub(crate) fn drain_device(backend: &FakeBackend, frames: usize) -> Vec<i16> {
        let mut guard = backend.source.lock().expect("test lock");
        let source = guard.as_mut().expect("plane opened an output");
        let mut buf = vec![0i16; frames];
        let filled = source.fill_playback(&mut buf);
        buf.truncate(filled);
        buf
    }

    /// Let the paused-clock runtime run the output task through `ticks`
    /// pull cycles.
    pub(crate) async fn run_ticks(ticks: u32) {
        for _ in 0..ticks {
            tokio::time::advance(std::time::Duration::from_millis(u64::from(TICK_MS))).await;
            tokio::task::yield_now().await;
        }
    }

    pub(crate) fn second_of(value: i16) -> Vec<i16> {
        vec![value; PROVIDER_OUTPUT_RATE as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::super::audio::ring::spsc_pcm;
    use super::testing::{drain_device, run_ticks, second_of, FakeBackend};
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn response_pcm_reaches_the_device_after_priming() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        plane.forward(second_of(1_000), generation.current());
        // 1 s queued; the 150 ms cushion primes, then every tick moves 10 ms
        // into the playback ring.
        run_ticks(30).await;

        let heard = drain_device(&backend, 4_096);
        assert!(!heard.is_empty(), "audio crossed the whole chain");
        assert!(
            heard.iter().all(|&s| s == 1_000),
            "device heard exactly what the provider sent (passthrough rate)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn barge_in_stops_stale_audio_and_lets_the_next_response_through() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        let stale_gen = generation.current();
        plane.forward(second_of(1_000), stale_gen);
        run_ticks(20).await;
        // Some gen-0 audio is already in the playback ring; a full second is
        // still queued in the jitter buffer.

        // Pinned order: bump → flush(+reset) → interrupt (channel side not
        // present in this rig).
        let fresh_gen = generation.bump();
        plane.barge_in();
        plane.forward(second_of(2_000), fresh_gen);
        run_ticks(40).await;

        let heard = drain_device(&backend, PROVIDER_OUTPUT_RATE as usize * 2);
        // The gate refuses the ring's stale remainder at drain, so nothing
        // gen-0 may follow the flush; only the fresh response plays.
        assert!(
            heard.iter().all(|&s| s == 2_000),
            "no stale tail: heard {} samples, first {:?}",
            heard.len(),
            heard.first()
        );
        assert!(!heard.is_empty(), "the fresh response is audible");
    }

    #[tokio::test(start_paused = true)]
    async fn generation_bump_alone_flushes_even_if_the_nudge_is_lost() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        plane.forward(second_of(1_000), generation.current());
        run_ticks(20).await;

        // No barge_in() call at all: the task must self-detect the bump.
        generation.bump();
        run_ticks(40).await;

        let heard = drain_device(&backend, PROVIDER_OUTPUT_RATE as usize * 2);
        assert!(
            heard.iter().all(|&s| s == 1_000),
            "whatever played predates the bump"
        );
        // The second of queued gen-0 audio must NOT keep draining for its
        // full length after the bump: at most the pre-bump pulls plus the
        // ring-side skip window.
        assert!(
            heard.len() < PROVIDER_OUTPUT_RATE as usize / 2,
            "stale audio kept flowing after the bump: {} samples",
            heard.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stale_deltas_never_enter_the_chain() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        let old = generation.current();
        generation.bump();
        plane.forward(second_of(3_000), old);
        run_ticks(40).await;

        assert!(
            drain_device(&backend, 4_096).is_empty(),
            "a delta from a superseded response produced audible frames"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn echo_tap_mirrors_exactly_what_the_device_played() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let (tap_tx, mut tap_rx) = spsc_pcm(PROVIDER_OUTPUT_RATE as usize * 4);
        let plane =
            spawn_output_plane(&backend, generation.clone(), Some(tap_tx)).expect("plane spawns");

        plane.forward(second_of(1_000), generation.current());
        run_ticks(30).await;

        let heard = drain_device(&backend, 2_048);
        let mut reference = vec![0i16; 4_096];
        let mirrored = tap_rx.pop_frames(&mut reference);
        assert_eq!(
            &reference[..mirrored],
            &heard[..],
            "the AEC reference is time-aligned with the device signal"
        );
    }

    fn drain_events(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutputEvent>,
    ) -> Vec<OutputEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn truncations(events: &[OutputEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, OutputEvent::Truncated { .. }))
            .count()
    }

    #[tokio::test(start_paused = true)]
    async fn speaking_edges_are_reported_once_per_transition() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let mut plane =
            spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");
        let mut events = plane.take_events().expect("events takeable once");

        plane.forward(second_of(1_000), generation.current());
        run_ticks(5).await;
        assert_eq!(
            drain_events(&mut events),
            vec![OutputEvent::Speaking(true)],
            "playback start is one speaking edge"
        );

        // 1 s of audio drains in ~100 ticks; the planned dry-out at the end
        // of a response is the idle edge.
        run_ticks(120).await;
        assert_eq!(
            drain_events(&mut events),
            vec![OutputEvent::Speaking(false)],
            "response end is one idle edge"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_stops_draining_is_declared_dead() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let mut plane =
            spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");
        let mut events = plane.take_events().expect("events takeable once");

        // A long response keeps streaming while nobody ever runs the device
        // callback: the jitter buffer refills the playback ring until the
        // ring jams (4 s), then STALL_TICKS of refused pushes follow.
        for _ in 0..10 {
            plane.forward(second_of(1_000), generation.current());
            run_ticks(60).await;
        }
        run_ticks(STALL_TICKS + 10).await;

        let seen = drain_events(&mut events);
        assert!(
            seen.iter()
                .any(|event| matches!(event, OutputEvent::Died { .. })),
            "a jammed ring for a full second must be reported as device death: {seen:?}"
        );
    }

    /// #209's fix contract, plane level: a memory-guard trip is loud — one
    /// [`OutputEvent::Truncated`] per response, not per delta, and a fresh
    /// response gets a fresh warning.
    #[tokio::test(start_paused = true)]
    async fn a_memory_guard_trip_is_reported_once_per_response() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let mut plane =
            spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");
        let mut events = plane.take_events().expect("events takeable once");
        // 30 s of provider audio per delta; five of them overrun the 120 s
        // capacity (the drained handful of ticks is negligible next to it).
        let thirty_seconds = PROVIDER_OUTPUT_RATE as usize * 30;
        let gen = generation.current();

        for _ in 0..5 {
            plane.forward(vec![9i16; thirty_seconds], gen);
            run_ticks(1).await;
        }
        let seen = drain_events(&mut events);
        assert_eq!(
            truncations(&seen),
            1,
            "an overrun response warns exactly once: {seen:?}"
        );

        // More refusals inside the SAME response stay quiet — the loudness
        // contract is once per response, never log spam.
        plane.forward(vec![9i16; thirty_seconds], gen);
        run_ticks(1).await;
        assert_eq!(truncations(&drain_events(&mut events)), 0);

        // A new response (barge-in flushes the backlog) earns its own
        // warning when it overruns too.
        let fresh_gen = generation.bump();
        plane.barge_in();
        run_ticks(1).await;
        for _ in 0..5 {
            plane.forward(vec![4i16; thirty_seconds], fresh_gen);
            run_ticks(1).await;
        }
        assert_eq!(
            truncations(&drain_events(&mut events)),
            1,
            "the per-response warning latch resets on a new response"
        );
    }

    /// Review finding on #209's first cut: generations only advance on explicit
    /// barge-in, so ordinary typed turns all share one generation — the pump stamps
    /// each turn with `generation.current()` and `turnComplete` never bumps. The
    /// warning latch must therefore re-arm on the response-end edge the task already
    /// detects, or the SECOND overlong reply of a session is eaten silently — the
    /// exact bug class #209 closes.
    #[tokio::test(start_paused = true)]
    async fn a_second_overlong_response_in_the_same_generation_warns_again() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let mut plane =
            spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");
        let mut events = plane.take_events().expect("events takeable once");
        let thirty_seconds = PROVIDER_OUTPUT_RATE as usize * 30;
        let gen = generation.current();

        for _ in 0..5 {
            plane.forward(vec![9i16; thirty_seconds], gen);
            run_ticks(1).await;
        }
        assert_eq!(truncations(&drain_events(&mut events)), 1);

        // Play response 1 out to its natural end — the device callback keeps
        // draining, no generation bump, no barge-in. The ~120 s backlog takes
        // ~12 000 ticks; draining the ring every 40 ticks keeps it far from
        // both the jam and the stall detector.
        let mut ended = false;
        for _ in 0..350 {
            run_ticks(40).await;
            drain_device(&backend, PROVIDER_OUTPUT_RATE as usize * 8);
            let seen = drain_events(&mut events);
            assert_eq!(truncations(&seen), 0, "no pushes, so no new warnings");
            if seen.contains(&OutputEvent::Speaking(false)) {
                ended = true;
                break;
            }
        }
        assert!(ended, "response 1 must reach its natural end edge");

        // Response 2, SAME generation (a typed follow-up turn): its overrun
        // must be loud too.
        for _ in 0..5 {
            plane.forward(vec![6i16; thirty_seconds], gen);
            run_ticks(1).await;
        }
        assert_eq!(
            truncations(&drain_events(&mut events)),
            1,
            "the latch re-arms at response end, not only on a generation change"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_mailbox_drops_the_newest_delta_and_never_stalls_the_sender() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        // 33 forwards with no await between them: on the current-thread test
        // runtime the task is never scheduled mid-loop, so the mailbox
        // (depth 32) is genuinely full for the last send. `try_send` must
        // drop that delta — never block the actor, never evict the backlog.
        let gen = generation.current();
        let delta = ms_to_frames(50, PROVIDER_OUTPUT_RATE);
        for value in 1..=(OUTPUT_MAILBOX_DEPTH as i16 + 1) {
            plane.forward(vec![value; delta], gen);
        }
        // 32 × 50 ms = 1.6 s queued (well inside jitter's whole-reply capacity).
        run_ticks(200).await;

        let heard = drain_device(&backend, PROVIDER_OUTPUT_RATE as usize * 4);
        assert_eq!(
            heard.len(),
            OUTPUT_MAILBOX_DEPTH * delta,
            "exactly the 32 mailbox-resident deltas played"
        );
        assert_eq!(
            heard.first(),
            Some(&1),
            "the oldest delta survived the overflow"
        );
        assert!(
            !heard.contains(&(OUTPUT_MAILBOX_DEPTH as i16 + 1)),
            "the overflow delta was dropped, not someone else's backlog"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn priming_holds_the_ring_empty_until_the_cushion_fills() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        // 100 ms queued < the 150 ms cushion: every pull is Priming and the
        // playback ring must stay EMPTY — priming silence is the device's
        // own zero-fill, never zeros pushed through the tagged writer.
        let gen = generation.current();
        plane.forward(vec![600; ms_to_frames(100, PROVIDER_OUTPUT_RATE)], gen);
        run_ticks(50).await;
        assert!(
            drain_device(&backend, 4_096).is_empty(),
            "frames (or zeros) reached the device ring while priming"
        );

        // Cross the cushion and the held audio becomes audible — proves the
        // empty ring above was priming, not a dead chain.
        plane.forward(vec![600; ms_to_frames(100, PROVIDER_OUTPUT_RATE)], gen);
        run_ticks(50).await;
        let heard = drain_device(&backend, 8_192);
        assert!(!heard.is_empty(), "the cushion filled and playback started");
        assert!(
            heard.iter().all(|&s| s == 600),
            "what plays is the held audio, not priming zeros"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_jammed_echo_tap_never_costs_the_device_a_single_frame() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        // A 10 ms tap that nobody drains: it jams on the first callback.
        let (tap_tx, mut tap_rx) = spsc_pcm(ms_to_frames(10, PROVIDER_OUTPUT_RATE));
        let plane =
            spawn_output_plane(&backend, generation.clone(), Some(tap_tx)).expect("plane spawns");

        plane.forward(second_of(700), generation.current());
        run_ticks(120).await;

        let heard = drain_device(&backend, PROVIDER_OUTPUT_RATE as usize * 2);
        assert_eq!(
            heard.len(),
            PROVIDER_OUTPUT_RATE as usize,
            "the full second reached the device despite the jammed tap"
        );
        assert!(
            heard.iter().all(|&s| s == 700),
            "device frames untouched by the tap"
        );
        assert!(
            tap_rx.dropped_frames() > 0,
            "the tap really jammed (drop-on-full is the policy under test)"
        );
        let mut reference = vec![0i16; 512];
        let kept = tap_rx.pop_frames(&mut reference);
        assert!(kept > 0, "the tap kept its oldest frames");
        assert!(
            reference[..kept].iter().all(|&s| s == 700),
            "what the tap kept is the device signal, oldest first"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_straggler_from_the_superseded_response_neither_plays_nor_harms_the_fresh_one() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");

        let stale_gen = generation.current();
        plane.forward(second_of(1_000), stale_gen);
        run_ticks(20).await;
        // Gen-0 is playing. Barge-in lands, and the provider — cancel still
        // in flight — keeps streaming the cancelled response for a while.
        // The straggler therefore arrives AFTER the bump and AFTER the fresh
        // response's first delta: the exact window where a missing stale-gen
        // check at the mailbox would let it flush the fresh audio it trails.
        let fresh_gen = generation.bump();
        plane.barge_in();
        plane.forward(second_of(2_000), fresh_gen);
        plane.forward(second_of(3_000), stale_gen);
        run_ticks(60).await;

        let heard = drain_device(&backend, PROVIDER_OUTPUT_RATE as usize * 3);
        assert!(!heard.is_empty(), "the fresh response is audible");
        assert!(
            !heard.contains(&3_000),
            "a straggler delta of the superseded response reached the device"
        );
        assert!(
            !heard.contains(&1_000),
            "the pre-bump backlog leaked past the flush and the ring gate"
        );
        assert!(
            heard.iter().all(|&s| s == 2_000),
            "only the fresh response plays, undamaged by the straggler behind it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_nudge_is_self_detected_and_never_mistaken_for_a_dead_device() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let mut plane =
            spawn_output_plane(&backend, generation.clone(), None).expect("plane spawns");
        let mut events = plane.take_events().expect("events takeable once");

        // 1.6 s queued (well inside jitter's whole-reply capacity), 20 ticks pulled: ~1.4 s
        // of gen-0 audio still sits in the jitter buffer when the bump lands.
        let gen = generation.current();
        plane.forward(vec![900; ms_to_frames(800, PROVIDER_OUTPUT_RATE)], gen);
        plane.forward(vec![900; ms_to_frames(800, PROVIDER_OUTPUT_RATE)], gen);
        run_ticks(20).await;

        // The nudge is lost (no barge_in call). Without the task's own
        // atomic check, those 1.4 s keep getting pulled and refused by the
        // tagged writer — >= STALL_TICKS of refusals, indistinguishable from
        // a jammed device — so the plane would declare a healthy device dead
        // over a barge-in. Self-detection must flush instead: playback ends
        // (one idle edge) and no death is reported.
        generation.bump();
        run_ticks(STALL_TICKS + 20).await;

        let seen = drain_events(&mut events);
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, OutputEvent::Died { .. })),
            "a lost barge-in nudge was misreported as device death: {seen:?}"
        );
        assert!(
            seen.contains(&OutputEvent::Speaking(false)),
            "the self-detected flush ends playback with an idle edge: {seen:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_plane_ends_the_task_and_closes_the_stream() {
        let backend = FakeBackend::default();
        let generation = Generation::new();
        let plane = spawn_output_plane(&backend, generation, None).expect("plane spawns");
        drop(plane);
        run_ticks(2).await;
        // The task ended without panicking and the source side still exists
        // for the backend to close; nothing further to observe deviceless —
        // the stream drop is the teardown (cpal closes on drop).
    }
}
