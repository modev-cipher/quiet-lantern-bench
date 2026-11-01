//! The barge-in generation gate (VOICE_AGENT §2 rule 2, §3 `AudioDelta.gen`).
//!
//! Barge-in is two ordered actions: bump the atomic generation counter, then send the
//! provider's async cancel. This module is the first action — the *local* half that
//! makes the stop feel instant. The network round-trip only stops future deltas; every
//! frame of the cancelled response that is already decoded and queued on this machine
//! is killed here.
//!
//! The gate sits at the playback seam. Every block of response audio enters through
//! [`TaggedPlaybackWriter::push_block`] carrying the `gen` its `AudioDelta` arrived
//! with, and travels as two rtrb writes: samples into the PCM ring first, then one
//! [`BlockTag`] into the tag ring. rtrb's push is a release and its pop an acquire, so
//! a consumer that observes a tag is guaranteed to find the tag's samples already in
//! the PCM ring — which is why [`GatedPlaybackSource::fill_playback`] can pop exact
//! counts and never spins.
//!
//! The output callback reads the live generation **once per device buffer** (the §2
//! diagram's "checks atomic generation counter per buffer") and discards every stale
//! block it meets, whether or not the buffer still has room. One callback therefore
//! drains the entire stale backlog without emitting a sample of it, so the only stale
//! audio that can reach the speaker after a bump is the single device buffer whose
//! fill had already begun when the bump landed. At a 20 ms device buffer that bounds
//! the perceived stop at ≤ 20 ms — the §2 rule 2 figure. The proof with concrete
//! numbers is `barge_in_perceived_stop_is_within_20ms_by_arithmetic` below.
//!
//! What the bump deliberately does **not** touch: the jitter buffer keeps its earned
//! cushion ([`super::jitter::JitterBuffer::flush`] — the network did not change, only
//! the turn did) and the down-resampler's overlap is cleared by
//! [`super::resample::MonoResampler::reset`]. The tokio-side barge-in sequence is
//! bump → flush → reset → `channel.interrupt()`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::device::PlaybackSource;
use super::ring::{spsc_pcm, SpscConsumer, SpscProducer};

/// The shared atomic generation counter. Clones observe the same counter; the control
/// plane holds one clone to bump, the playback callback holds another to read.
#[derive(Clone)]
pub struct Generation {
    live: Arc<AtomicU64>,
}

impl Generation {
    pub fn new() -> Self {
        Self {
            live: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn current(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }

    /// The local half of barge-in. Returns the new live generation, which is the id
    /// the next response's `AudioDelta`s must carry to be heard.
    pub fn bump(&self) -> u64 {
        self.live.fetch_add(1, Ordering::AcqRel) + 1
    }
}

impl Default for Generation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockTag {
    generation: u64,
    frames: usize,
}

/// Tokio-side writer: tags each response block with its generation before it crosses
/// to the realtime thread. Refuses blocks whose generation is already dead, so a slow
/// decoder cannot keep feeding a cancelled response into the ring.
pub struct TaggedPlaybackWriter {
    tags: rtrb::Producer<BlockTag>,
    pcm: SpscProducer,
    generation: Generation,
    refused_stale_frames: u64,
    refused_untagged_frames: u64,
    skipped: Arc<AtomicU64>,
}

impl TaggedPlaybackWriter {
    /// Queues one block of response audio under `generation`. Returns how many frames
    /// were accepted; a stale generation or a full tag ring accepts zero, and a full
    /// PCM ring accepts a counted prefix (the tail is counted by the seam's own
    /// dropped-frames counter).
    pub fn push_block(&mut self, generation: u64, frames: &[i16]) -> usize {
        if frames.is_empty() {
            return 0;
        }
        if generation != self.generation.current() {
            self.refused_stale_frames += frames.len() as u64;
            return 0;
        }
        if self.tags.is_full() {
            self.refused_untagged_frames += frames.len() as u64;
            return 0;
        }
        let outcome = self.pcm.push_frames(frames);
        if outcome.accepted == 0 {
            return 0;
        }
        self.tags
            .push(BlockTag {
                generation,
                frames: outcome.accepted,
            })
            .expect("a free tag slot was checked before the samples were written");
        outcome.accepted
    }

    /// Frames refused because their generation was already bumped past at push time.
    pub fn refused_stale_frames(&self) -> u64 {
        self.refused_stale_frames
    }

    /// Frames refused because no tag slot was free; untagged samples must never enter
    /// the PCM ring or every later block would play under the wrong generation.
    pub fn refused_untagged_frames(&self) -> u64 {
        self.refused_untagged_frames
    }

    /// Frames the playback callback discarded as stale, visible from the tokio side.
    pub fn skipped_frames(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    pub fn dropped_frames(&self) -> u64 {
        self.pcm.dropped_frames()
    }
}

/// Realtime-side reader. Implements the playback seam of the unit-1 trait and applies
/// the generation gate; allocation-free and wait-free like every callback path.
pub struct GatedPlaybackSource {
    tags: rtrb::Consumer<BlockTag>,
    pcm: SpscConsumer,
    generation: Generation,
    in_flight: Option<BlockTag>,
    skipped: Arc<AtomicU64>,
}

impl GatedPlaybackSource {
    fn next_block(&mut self) -> Option<BlockTag> {
        self.in_flight.take().or_else(|| self.tags.pop().ok())
    }

    pub fn skipped_frames(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
}

impl PlaybackSource for GatedPlaybackSource {
    fn fill_playback(&mut self, out: &mut [i16]) -> usize {
        let live = self.generation.current();
        let mut filled = 0;
        while let Some(block) = self.next_block() {
            if block.generation != live {
                let released = self.pcm.discard_frames(block.frames);
                self.skipped.fetch_add(released as u64, Ordering::Relaxed);
                continue;
            }
            if filled == out.len() {
                self.in_flight = Some(block);
                break;
            }
            let take = block.frames.min(out.len() - filled);
            let popped = self.pcm.pop_frames(&mut out[filled..filled + take]);
            filled += popped;
            let rest = block.frames - popped;
            if rest > 0 {
                self.in_flight = Some(BlockTag {
                    generation: block.generation,
                    frames: rest,
                });
            }
            if popped < take {
                break;
            }
        }
        filled
    }
}

/// Builds the gated playback seam: `capacity_frames` of mono PCM16 and up to
/// `max_blocks` outstanding tagged blocks.
pub fn tagged_playback(
    generation: Generation,
    capacity_frames: usize,
    max_blocks: usize,
) -> (TaggedPlaybackWriter, GatedPlaybackSource) {
    assert!(
        capacity_frames > 0,
        "playback ring capacity must be positive"
    );
    assert!(max_blocks > 0, "the seam needs at least one tag slot");
    let (tag_producer, tag_consumer) = rtrb::RingBuffer::<BlockTag>::new(max_blocks);
    let (pcm_producer, pcm_consumer) = spsc_pcm(capacity_frames);
    let skipped = Arc::new(AtomicU64::new(0));
    (
        TaggedPlaybackWriter {
            tags: tag_producer,
            pcm: pcm_producer,
            generation: generation.clone(),
            refused_stale_frames: 0,
            refused_untagged_frames: 0,
            skipped: Arc::clone(&skipped),
        },
        GatedPlaybackSource {
            tags: tag_consumer,
            pcm: pcm_consumer,
            generation,
            in_flight: None,
            skipped,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(from: i16, count: usize) -> Vec<i16> {
        (0..count).map(|i| from + i as i16).collect()
    }

    fn pull(source: &mut GatedPlaybackSource, frames: usize) -> (Vec<i16>, usize) {
        let mut out = vec![0i16; frames];
        let filled = source.fill_playback(&mut out);
        (out, filled)
    }

    #[test]
    fn generation_counter_is_shared_and_monotonic() {
        let generation = Generation::new();
        let observer = generation.clone();
        assert_eq!(generation.current(), 0);
        assert_eq!(generation.bump(), 1);
        assert_eq!(observer.current(), 1);
        assert_eq!(observer.bump(), 2);
        assert_eq!(generation.current(), 2);
    }

    #[test]
    fn live_generation_flows_through_in_order_across_ragged_blocks_and_callbacks() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation, 64, 8);

        assert_eq!(writer.push_block(0, &ramp(100, 7)), 7);
        assert_eq!(writer.push_block(0, &ramp(200, 13)), 13);
        assert_eq!(writer.push_block(0, &ramp(300, 5)), 5);

        let mut received = Vec::new();
        for want in [4usize, 9, 12] {
            let (out, filled) = pull(&mut source, want);
            received.extend_from_slice(&out[..filled]);
        }

        let mut expected = ramp(100, 7);
        expected.extend(ramp(200, 13));
        expected.extend(ramp(300, 5));
        assert_eq!(
            received, expected,
            "live audio must come out frame-for-frame in push order across block and \
             callback boundaries"
        );
        assert_eq!(source.skipped_frames(), 0);
        assert_eq!(writer.refused_stale_frames(), 0);
    }

    #[test]
    fn stale_frames_are_dropped_and_the_sink_receives_only_the_live_payload() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation.clone(), 256, 8);
        let gen0_payload = vec![1111i16; 40];
        let gen1_payload = vec![2222i16; 24];

        assert_eq!(writer.push_block(0, &gen0_payload), 40);
        let (out, filled) = pull(&mut source, 16);
        assert_eq!(filled, 16);
        assert_eq!(&out[..16], &vec![1111i16; 16][..]);

        generation.bump();
        assert_eq!(writer.push_block(1, &gen1_payload), 24);

        let (out, filled) = pull(&mut source, 32);
        assert_eq!(filled, 24);
        assert_eq!(
            &out[..24],
            &vec![2222i16; 24][..],
            "after the bump the sink must receive the new generation's payload and \
             nothing of the old one"
        );
        assert!(
            !out[..filled].contains(&1111),
            "not one stale frame may reach the sink after the bump"
        );
        assert_eq!(
            source.skipped_frames(),
            24,
            "the 40 queued gen-0 frames minus the 16 played before the bump were \
             discarded, exactly"
        );
    }

    #[test]
    fn a_block_straddling_the_bump_loses_its_unplayed_remainder() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation.clone(), 64, 4);

        writer.push_block(0, &[1111i16; 10]);
        let (out, filled) = pull(&mut source, 4);
        assert_eq!((&out[..4], filled), (&vec![1111i16; 4][..], 4));

        generation.bump();
        writer.push_block(1, &[2222i16; 6]);

        let (out, filled) = pull(&mut source, 8);
        assert_eq!(filled, 6);
        assert_eq!(
            &out[..6],
            &vec![2222i16; 6][..],
            "the six unplayed frames of the straddling gen-0 block must be discarded, \
             not played ahead of the live audio"
        );
        assert_eq!(source.skipped_frames(), 6);
    }

    #[test]
    fn frames_for_a_dead_generation_are_refused_at_the_writer() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation.clone(), 64, 4);

        generation.bump();
        assert_eq!(
            writer.push_block(0, &[1111i16; 12]),
            0,
            "a cancelled response still being decoded must not enter the ring at all"
        );
        assert_eq!(writer.refused_stale_frames(), 12);

        let (_, filled) = pull(&mut source, 16);
        assert_eq!(filled, 0);
        assert_eq!(source.skipped_frames(), 0);
    }

    #[test]
    fn a_block_tagged_for_a_future_generation_is_dropped_and_never_reaches_the_sink() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation.clone(), 64, 4);
        let future_payload = vec![7777i16; 24];

        assert_eq!(generation.current(), 0);
        assert_eq!(
            writer.push_block(1, &future_payload),
            0,
            "only the current generation plays; a future generation must be refused, \
             not queued"
        );

        let (out, filled) = pull(&mut source, 32);
        assert_eq!(filled, 0);
        assert!(
            !out.contains(&7777),
            "not one future-generation frame may reach the sink"
        );
        assert_eq!(
            source.skipped_frames(),
            0,
            "the future block must never have entered the ring at all"
        );
    }

    #[test]
    fn a_full_tag_ring_refuses_the_block_whole_rather_than_letting_it_ride_untagged() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation, 64, 1);

        assert_eq!(writer.push_block(0, &ramp(500, 6)), 6);
        assert_eq!(writer.push_block(0, &ramp(600, 6)), 0);
        assert_eq!(writer.refused_untagged_frames(), 6);

        let (out, filled) = pull(&mut source, 16);
        assert_eq!(filled, 6);
        assert_eq!(
            &out[..6],
            &ramp(500, 6)[..],
            "only the tagged block may play; untagged samples in the PCM ring would \
             corrupt every later block's generation accounting"
        );
    }

    #[test]
    fn two_bumps_kill_both_intermediate_generations() {
        let generation = Generation::new();
        let (mut writer, mut source) = tagged_playback(generation.clone(), 256, 8);

        writer.push_block(0, &[1111i16; 20]);
        generation.bump();
        writer.push_block(1, &[2222i16; 20]);
        generation.bump();
        writer.push_block(2, &[3333i16; 20]);

        let (out, filled) = pull(&mut source, 64);
        assert_eq!(filled, 20);
        assert_eq!(
            &out[..20],
            &vec![3333i16; 20][..],
            "after two bumps only generation 2 is alive; both older payloads must be \
             skipped in the same callback"
        );
        assert_eq!(source.skipped_frames(), 40);
    }

    #[test]
    fn barge_in_perceived_stop_is_within_20ms_by_arithmetic() {
        const DEVICE_RATE: u32 = 48_000;
        const CALLBACK_FRAMES: usize = 960;
        const QUEUED_RESPONSE_MS: usize = 2_000;
        const QUEUED_FRAMES: usize = DEVICE_RATE as usize * QUEUED_RESPONSE_MS / 1_000;
        const BLOCKS: usize = QUEUED_FRAMES / CALLBACK_FRAMES;

        let generation = Generation::new();
        let (mut writer, mut source) =
            tagged_playback(generation.clone(), QUEUED_FRAMES, BLOCKS + 1);

        let stale_payload = vec![1111i16; CALLBACK_FRAMES];
        for _ in 0..BLOCKS {
            assert_eq!(writer.push_block(0, &stale_payload), CALLBACK_FRAMES);
        }

        let (out, filled) = pull(&mut source, CALLBACK_FRAMES);
        assert_eq!(filled, CALLBACK_FRAMES);
        assert_eq!(
            &out[..],
            &stale_payload[..],
            "this is the device buffer whose fill had already begun when the bump \
             lands: the worst case the arithmetic charges"
        );

        generation.bump();
        writer.push_block(1, &vec![2222i16; CALLBACK_FRAMES]);

        let (out, filled) = pull(&mut source, CALLBACK_FRAMES);
        assert_eq!(filled, CALLBACK_FRAMES);
        assert!(
            !out.contains(&1111),
            "the first callback after the bump must emit zero stale frames"
        );
        assert_eq!(&out[..], &vec![2222i16; CALLBACK_FRAMES][..]);
        assert_eq!(
            source.skipped_frames() as usize,
            QUEUED_FRAMES - CALLBACK_FRAMES,
            "the entire remaining stale backlog was discarded inside that single \
             callback, so no later callback can leak any of it"
        );

        let callback_ms = CALLBACK_FRAMES * 1_000 / DEVICE_RATE as usize;
        assert_eq!(callback_ms, 20, "960 frames at 48 kHz is exactly 20 ms");
        let queued_ms = QUEUED_FRAMES * 1_000 / DEVICE_RATE as usize;
        assert_eq!(queued_ms, 2_000, "the backlog held 2 s of audio, not 20 ms");
        let worst_case_drained_ms = callback_ms;
        assert!(
            worst_case_drained_ms <= 20,
            "stale audio audible after a bump is bounded by the one in-flight device \
             buffer ({callback_ms} ms), never by the {queued_ms} ms that were queued"
        );
    }
}
