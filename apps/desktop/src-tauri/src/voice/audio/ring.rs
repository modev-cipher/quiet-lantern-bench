//! Provenance (AGENTS.md §5a): rtrb 0.3.4 lock-free SPSC ring.
//! Source: crate source read at ~/.cargo/registry/src/*/rtrb-0.3.4/src/{lib.rs,chunks.rs}
//! Checked: 2026-08-18. API facts this module depends on:
//!   - `RingBuffer::new(capacity)` returns `(Producer<T>, Consumer<T>)` and the
//!     capacity is exact (a ring of 100 holds 100 items, no reserved slot).
//!   - `Producer::push_partial_slice(&[T]) -> (pushed, remainder)` and
//!     `Consumer::pop_partial_slice(&mut [T]) -> (filled, remainder)` are the
//!     allocation-free bulk paths (both require `T: Copy`).
//!   - `Consumer::read_chunk(n)` (chunks.rs:342) errs if fewer than `n` slots are
//!     readable, and `ReadChunk::commit_all()` (chunks.rs:651) releases the chunk
//!     without copying it out, which is the allocation-free discard path.
//!   - `Producer::push(value)` / `Consumer::pop()` (lib.rs:336, 572) are the
//!     single-item wait-free paths used for the generation block tags.
//!   - Only the `Producer` half may write and only the `Consumer` half may read.
//!     That split is why the drop-oldest buffer cannot be the rtrb ring; see below.
//!
//! Two buffers, two different overflow policies, for one reason:
//!
//! * [`SpscProducer`]/[`SpscConsumer`] is the realtime-thread seam. It must never
//!   block, lock or allocate (VOICE_AGENT §6), and rtrb's SPSC contract gives the
//!   producer no way to remove the oldest item — that memory belongs to the reader.
//!   So on overflow this seam drops the *newest* frames and counts them, which is
//!   observable as a glitch rather than silently absorbed.
//! * [`MicRing`] is the tokio-side ~30 s replay buffer of VOICE_AGENT §2 rule 3. It
//!   has a single owner, so it can evict the oldest frames, which is the policy the
//!   spec requires: the mic producer never blocks and old audio is what gets thrown
//!   away when the consumer is behind.
//!
//! Putting the drop-oldest buffer on the audio callback thread instead would mean
//! either locking (priority inversion) or a consumer-visible race, so the split is
//! forced by the spec's own threading rule rather than chosen for convenience.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::device::CaptureSink;

/// Result of a non-blocking bulk push at the realtime seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushOutcome {
    pub accepted: usize,
    pub dropped: usize,
}

/// Writing half of the realtime seam. Lives on the OS audio thread.
pub struct SpscProducer {
    inner: rtrb::Producer<i16>,
    dropped: Arc<AtomicU64>,
}

impl SpscProducer {
    /// Copies as many frames as fit and drops the rest, newest-first, counting them.
    /// Allocation-free and wait-free: safe to call from a device callback.
    pub fn push_frames(&mut self, frames: &[i16]) -> PushOutcome {
        let (pushed, remainder) = self.inner.push_partial_slice(frames);
        if !remainder.is_empty() {
            self.dropped
                .fetch_add(remainder.len() as u64, Ordering::Relaxed);
        }
        PushOutcome {
            accepted: pushed.len(),
            dropped: remainder.len(),
        }
    }

    pub fn free_frames(&self) -> usize {
        self.inner.slots()
    }

    pub fn capacity_frames(&self) -> usize {
        self.inner.buffer().capacity()
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl CaptureSink for SpscProducer {
    fn on_capture(&mut self, frames: &[i16]) {
        self.push_frames(frames);
    }
}

/// Reading half of the realtime seam. Lives on the tokio side for capture and on
/// the OS audio thread for playback.
pub struct SpscConsumer {
    inner: rtrb::Consumer<i16>,
    dropped: Arc<AtomicU64>,
}

impl SpscConsumer {
    /// Fills `out` with as many frames as are available, in order. Returns the count.
    pub fn pop_frames(&mut self, out: &mut [i16]) -> usize {
        let (filled, _remainder) = self.inner.pop_partial_slice(out);
        filled.len()
    }

    pub fn available_frames(&self) -> usize {
        self.inner.slots()
    }

    /// Releases up to `n` frames without copying them anywhere: the barge-in skip
    /// (VOICE_AGENT §2 rule 2) discards stale audio from the playback callback, which
    /// must stay allocation-free. Returns how many frames were actually released.
    pub fn discard_frames(&mut self, n: usize) -> usize {
        let take = n.min(self.inner.slots());
        if take == 0 {
            return 0;
        }
        match self.inner.read_chunk(take) {
            Ok(chunk) => {
                chunk.commit_all();
                take
            }
            Err(_) => 0,
        }
    }

    pub fn capacity_frames(&self) -> usize {
        self.inner.buffer().capacity()
    }

    /// Frames the producer had to throw away because this side was behind.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Builds one realtime seam of `capacity_frames` mono PCM16 frames.
pub fn spsc_pcm(capacity_frames: usize) -> (SpscProducer, SpscConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::<i16>::new(capacity_frames);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        SpscProducer {
            inner: producer,
            dropped: Arc::clone(&dropped),
        },
        SpscConsumer {
            inner: consumer,
            dropped,
        },
    )
}

/// Bounded drop-oldest mic buffer (VOICE_AGENT §2 rule 3): the producer never
/// blocks, capacity is a wall-clock window, and the replay source after a transient
/// socket drop is this same buffer.
///
/// Frame indices are absolute and monotonic for the lifetime of the buffer so a
/// caller can align a committed-turn watermark against them (VOICE_AGENT §3
/// mic-ring replay boundary). Eviction advances the indices; it never renumbers.
pub struct MicRing {
    buf: VecDeque<i16>,
    capacity_frames: usize,
    first_frame_index: u64,
    total_pushed: u64,
    dropped_oldest: u64,
}

impl MicRing {
    pub fn with_capacity_frames(capacity_frames: usize) -> Self {
        assert!(capacity_frames > 0, "mic ring capacity must be positive");
        Self {
            buf: VecDeque::with_capacity(capacity_frames),
            capacity_frames,
            first_frame_index: 0,
            total_pushed: 0,
            dropped_oldest: 0,
        }
    }

    /// Sizes the window in seconds at `rate`, which is how the spec states it.
    pub fn for_seconds(rate: u32, seconds: u32) -> Self {
        Self::with_capacity_frames(rate as usize * seconds as usize)
    }

    /// Appends frames, evicting the oldest to stay within capacity. Returns how
    /// many frames were evicted by this push.
    ///
    /// A push larger than the whole window keeps the newest `capacity_frames` of it:
    /// the frames it could not store are the oldest ones, so they are counted as
    /// evictions like any other overflow.
    pub fn push_frames(&mut self, frames: &[i16]) -> usize {
        let tail = if frames.len() > self.capacity_frames {
            &frames[frames.len() - self.capacity_frames..]
        } else {
            frames
        };
        let skipped = frames.len() - tail.len();
        let overflow = (self.buf.len() + tail.len()).saturating_sub(self.capacity_frames);

        self.buf.drain(..overflow);
        self.buf.extend(tail.iter().copied());

        let evicted = skipped + overflow;
        self.total_pushed += frames.len() as u64;
        self.dropped_oldest += evicted as u64;
        self.first_frame_index += evicted as u64;
        evicted
    }

    pub fn len_frames(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    /// Frames evicted by the capacity policy over the buffer's lifetime. A
    /// [`Self::discard`] is a policy decision, not an overflow, and is not counted here.
    pub fn dropped_oldest_frames(&self) -> u64 {
        self.dropped_oldest
    }

    /// Absolute index of the oldest frame still buffered.
    pub fn first_frame_index(&self) -> u64 {
        self.first_frame_index
    }

    /// Absolute index the next pushed frame will get.
    pub fn next_frame_index(&self) -> u64 {
        self.total_pushed
    }

    pub fn to_vec(&self) -> Vec<i16> {
        self.buf.iter().copied().collect()
    }

    /// Drops everything up to (not including) `frame_index`, the acknowledged
    /// end-of-utterance watermark. Audio the provider already accepted must not be
    /// replayed (VOICE_AGENT §3). Returns the number of frames released.
    pub fn drop_through(&mut self, frame_index: u64) -> usize {
        if frame_index <= self.first_frame_index {
            return 0;
        }
        let wanted = (frame_index - self.first_frame_index) as usize;
        let released = wanted.min(self.buf.len());
        self.buf.drain(..released);
        self.first_frame_index += released as u64;
        released
    }

    /// Session-level renewal (expiry / GoAway) discards the stale buffer rather than
    /// replaying it (VOICE_AGENT §2 rule 3, §9). Indices stay monotonic.
    pub fn discard(&mut self) {
        self.buf.clear();
        self.first_frame_index = self.total_pushed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(from: i16, count: usize) -> Vec<i16> {
        (0..count).map(|i| from + i as i16).collect()
    }

    #[test]
    fn mic_ring_below_capacity_keeps_every_frame_in_order() {
        let mut ring = MicRing::with_capacity_frames(8);
        assert_eq!(ring.push_frames(&ramp(1, 3)), 0);
        assert_eq!(ring.push_frames(&ramp(4, 2)), 0);

        assert_eq!(ring.to_vec(), vec![1, 2, 3, 4, 5]);
        assert_eq!(ring.len_frames(), 5);
        assert_eq!(ring.dropped_oldest_frames(), 0);
        assert_eq!(ring.first_frame_index(), 0);
        assert_eq!(ring.next_frame_index(), 5);
    }

    #[test]
    fn mic_ring_overflow_drops_the_oldest_frames_not_the_newest() {
        let mut ring = MicRing::with_capacity_frames(4);
        ring.push_frames(&ramp(1, 4));

        let evicted = ring.push_frames(&ramp(5, 2));

        assert_eq!(evicted, 2);
        assert_eq!(
            ring.to_vec(),
            vec![3, 4, 5, 6],
            "the two oldest frames must be the ones gone, and the newest two must be present"
        );
        assert_eq!(ring.dropped_oldest_frames(), 2);
        assert_eq!(ring.first_frame_index(), 2);
        assert_eq!(ring.next_frame_index(), 6);
    }

    #[test]
    fn mic_ring_eviction_counts_are_exact_over_many_pushes() {
        let mut ring = MicRing::with_capacity_frames(10);
        let mut total_evicted = 0usize;
        for chunk in 0..7 {
            total_evicted += ring.push_frames(&ramp(chunk * 4 + 1, 4));
        }

        assert_eq!(ring.len_frames(), 10);
        assert_eq!(total_evicted, 28 - 10);
        assert_eq!(ring.dropped_oldest_frames(), 18);
        assert_eq!(ring.to_vec(), ramp(19, 10));
        assert_eq!(ring.first_frame_index(), 18);
        assert_eq!(ring.next_frame_index(), 28);
    }

    #[test]
    fn mic_ring_push_larger_than_the_window_keeps_only_the_newest_capacity_frames() {
        let mut ring = MicRing::with_capacity_frames(4);
        ring.push_frames(&ramp(1, 2));

        let evicted = ring.push_frames(&ramp(3, 6));

        assert_eq!(evicted, 4);
        assert_eq!(ring.to_vec(), vec![5, 6, 7, 8]);
        assert_eq!(ring.dropped_oldest_frames(), 4);
        assert_eq!(ring.first_frame_index(), 4);
        assert_eq!(ring.next_frame_index(), 8);
    }

    #[test]
    fn mic_ring_frame_indices_stay_aligned_with_the_buffered_content() {
        let mut ring = MicRing::with_capacity_frames(5);
        for chunk in 0..6 {
            ring.push_frames(&ramp(chunk * 3 + 1, 3));
            assert_eq!(
                ring.first_frame_index() + ring.len_frames() as u64,
                ring.next_frame_index(),
                "first index plus length must always name the next frame"
            );
        }
        ring.drop_through(ring.first_frame_index() + 2);
        assert_eq!(
            ring.first_frame_index() + ring.len_frames() as u64,
            ring.next_frame_index()
        );
        ring.discard();
        assert_eq!(
            ring.first_frame_index() + ring.len_frames() as u64,
            ring.next_frame_index()
        );
    }

    #[test]
    fn mic_ring_drop_through_releases_acknowledged_audio_only() {
        let mut ring = MicRing::with_capacity_frames(8);
        ring.push_frames(&ramp(1, 6));

        let released = ring.drop_through(4);

        assert_eq!(released, 4);
        assert_eq!(ring.to_vec(), vec![5, 6]);
        assert_eq!(ring.first_frame_index(), 4);
        assert_eq!(
            ring.drop_through(2),
            0,
            "a stale watermark releases nothing"
        );
        assert_eq!(ring.to_vec(), vec![5, 6]);
        assert_eq!(
            ring.drop_through(99),
            2,
            "a watermark past the end drains what is there without panicking"
        );
        assert!(ring.is_empty());
    }

    #[test]
    fn mic_ring_discard_is_not_counted_as_an_overflow() {
        let mut ring = MicRing::with_capacity_frames(4);
        ring.push_frames(&ramp(1, 6));
        let overflow_before = ring.dropped_oldest_frames();

        ring.discard();

        assert!(ring.is_empty());
        assert_eq!(
            ring.dropped_oldest_frames(),
            overflow_before,
            "session renewal discards by policy; counting it as glitch overflow would \
             hide real capacity loss"
        );
        assert_eq!(ring.first_frame_index(), 6);
        assert_eq!(ring.next_frame_index(), 6);
        ring.push_frames(&ramp(7, 2));
        assert_eq!(ring.to_vec(), vec![7, 8]);
        assert_eq!(ring.first_frame_index(), 6);
    }

    #[test]
    fn mic_ring_window_is_sized_in_seconds_of_audio() {
        let ring = MicRing::for_seconds(16_000, 30);
        assert_eq!(ring.capacity_frames(), 480_000);
    }

    #[test]
    fn spsc_seam_round_trips_frames_in_order() {
        let (mut producer, mut consumer) = spsc_pcm(8);
        assert_eq!(producer.capacity_frames(), 8);
        assert_eq!(consumer.capacity_frames(), 8);

        let outcome = producer.push_frames(&ramp(1, 5));
        assert_eq!(
            outcome,
            PushOutcome {
                accepted: 5,
                dropped: 0
            }
        );
        assert_eq!(consumer.available_frames(), 5);

        let mut out = [0i16; 8];
        assert_eq!(consumer.pop_frames(&mut out), 5);
        assert_eq!(&out[..5], &[1, 2, 3, 4, 5]);
        assert_eq!(consumer.available_frames(), 0);
        assert_eq!(consumer.dropped_frames(), 0);
    }

    #[test]
    fn spsc_seam_overflow_drops_the_newest_frames_and_counts_them_exactly() {
        let (mut producer, mut consumer) = spsc_pcm(4);

        let outcome = producer.push_frames(&ramp(1, 6));

        assert_eq!(
            outcome,
            PushOutcome {
                accepted: 4,
                dropped: 2
            }
        );
        assert_eq!(producer.dropped_frames(), 2);
        assert_eq!(
            consumer.dropped_frames(),
            2,
            "the tokio side must be able to see the glitch the callback could not avoid"
        );

        let mut out = [0i16; 8];
        assert_eq!(consumer.pop_frames(&mut out), 4);
        assert_eq!(
            &out[..4],
            &[1, 2, 3, 4],
            "rtrb's SPSC contract forbids the producer from evicting the reader's frames, \
             so the realtime seam drops the newest"
        );
    }

    #[test]
    fn spsc_seam_and_mic_ring_disagree_on_purpose() {
        let (mut producer, mut consumer) = spsc_pcm(4);
        let mut ring = MicRing::with_capacity_frames(4);

        producer.push_frames(&ramp(1, 6));
        ring.push_frames(&ramp(1, 6));

        let mut seam = [0i16; 4];
        consumer.pop_frames(&mut seam);
        assert_eq!(seam, [1, 2, 3, 4]);
        assert_eq!(ring.to_vec(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn capture_sink_feeds_the_seam_without_reporting_upward() {
        let (producer, mut consumer) = spsc_pcm(4);
        let mut sink: Box<dyn CaptureSink> = Box::new(producer);

        sink.on_capture(&ramp(1, 3));
        sink.on_capture(&ramp(4, 3));

        let mut out = [0i16; 4];
        assert_eq!(consumer.pop_frames(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(consumer.dropped_frames(), 2);
    }
}
