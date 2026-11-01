//! The adaptive jitter buffer between the down-resampler and the playback ring
//! (VOICE_AGENT §2 data plane, §2 rule 4 latency budget).
//!
//! **This buffer has no clock of its own and never reads one.** Audio is its own clock:
//! the device consumes frames at exactly the rate it was opened at, so `n` frames pulled
//! *is* `n / rate` seconds of elapsed time. Every adaptation decision here is driven by
//! [`JitterBuffer::frames_pulled`], which is why the whole component is testable in CI
//! under simulated time with no devices, no sleeps and no wall-clock reads.
//!
//! Two independent bounds, for two different failures:
//!
//! * `target_ms` is the **cushion**: how much audio must be queued before playback
//!   starts, and therefore how much late arrival the stream absorbs before a gap is
//!   audible. The spec's figure is 150 ms. It adapts, but only inside
//!   `[min_target_ms, max_target_ms]` and only by `step_ms` per event, so a hostile
//!   network can lengthen the cushion but can never make it unbounded.
//! * `capacity_ms` is the **memory guard** the spec asks for ("size the jitter buffer in
//!   decoded milliseconds plus byte capacity"). A provider streams a long response far
//!   faster than real time, so this has to be sized in whole responses, not in cushions
//!   — see [`CAPACITY_MS`] for the sizing math. The guard is a safety backstop against a
//!   runaway provider, not a governor a normal reply is expected to hit; a caller that
//!   sees [`JitterBuffer::push`] refuse frames must surface it loudly (#209's bug class
//!   was the silent variant), which [`super::super::output`] does.
//!
//! Overflow drops the **newest** frames, the opposite of the mic ring's policy in
//! [`super::ring::MicRing`] and for the same structural reason as
//! [`super::ring::SpscProducer`]: the frames at the front of this queue are the ones the
//! listener is about to hear, so evicting them would put a skip in the middle of a word.
//! Refusing the tail of an over-long response is a bounded, counted loss instead.

use std::collections::VecDeque;

/// Default memory-guard capacity: **two minutes of audio**, sized to hold a whole long
/// reply, not to smooth link jitter (#209).
///
/// Gemini Live native-audio streams reply PCM several times faster than the device
/// drains it, so during a long reply nearly the entire remainder sits in this buffer.
/// The previous 2 000 ms figure was link-jitter scale: once the provider ran ~2 s ahead
/// the guard refused the tail of every delta and punched holes in every long reply
/// (171 s of audio eaten over one 24-minute live session). Capacity must therefore
/// bound *memory*, not *pace*.
///
/// The math: the buffer holds mono i16 frames at the device rate, so one second costs
/// `rate × 2` bytes — 96 KB/s at a 48 kHz device (the common case), 88.2 KB/s at
/// 44.1 kHz. 120 s is therefore ≈ 11.5 MB worst-case, a trivial footprint for a desktop
/// app and roughly 4× the longest reply observed live (a slow count-to-twenty runs
/// 25–35 s; teaching-length answers a minute or so). The figure scales with the device
/// rate — a 96 or 192 kHz output costs ~23/46 MB — but the deque's preallocation is
/// address space; pages commit only as backlog actually accumulates, and because the
/// guard caps the length at capacity, a push can never reallocate mid-reply. A reply
/// that outruns even this is provider misbehavior, and the guard still bounds it —
/// dropped, counted, and warned about by the output task, never silent.
pub const CAPACITY_MS: u32 = 120_000;

/// Sizing for one playback jitter buffer. All figures are milliseconds of audio at
/// `rate`, so the same numbers describe any device rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterConfig {
    pub rate: u32,
    pub target_ms: u32,
    pub min_target_ms: u32,
    pub max_target_ms: u32,
    pub step_ms: u32,
    pub capacity_ms: u32,
    pub decay_window_ms: u32,
    pub decay_hysteresis_ms: u32,
}

impl JitterConfig {
    /// The spec's sizing: a 150 ms cushion (VOICE_AGENT §2 rule 4), free to grow to
    /// 400 ms on a bad link and to shrink to 60 ms on a good one, and [`CAPACITY_MS`]
    /// of capacity so the memory guard holds a whole long reply and only ever trips as
    /// a backstop against a runaway provider (#209 — the old 2 s figure truncated every
    /// long reply on the live wire).
    pub const fn for_rate(rate: u32) -> Self {
        Self {
            rate,
            target_ms: 150,
            min_target_ms: 60,
            max_target_ms: 400,
            step_ms: 30,
            capacity_ms: CAPACITY_MS,
            decay_window_ms: 5_000,
            decay_hysteresis_ms: 40,
        }
    }
}

/// Why a pull produced fewer frames than the device asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullOutcome {
    /// Playback is filling the cushion; the silence is expected, not a glitch.
    Priming,
    /// Every requested frame was delivered.
    Full,
    /// Playback had started and the queue ran dry. This is the audible failure.
    Underrun { missing: usize },
}

pub struct JitterBuffer {
    cfg: JitterConfig,
    buf: VecDeque<i16>,
    capacity_frames: usize,
    target_ms: u32,
    playing: bool,
    frames_pulled: u64,
    underruns: u64,
    underrun_frames: u64,
    primed_silence_frames: u64,
    dropped_newest: u64,
    target_raises: u64,
    target_lowers: u64,
    window_start_frame: u64,
    window_min_level: usize,
}

impl JitterBuffer {
    pub fn new(cfg: JitterConfig) -> Self {
        assert!(cfg.rate > 0, "jitter buffer rate must be positive");
        assert!(
            cfg.min_target_ms <= cfg.target_ms && cfg.target_ms <= cfg.max_target_ms,
            "the starting target must sit inside its own bounds"
        );
        assert!(
            cfg.capacity_ms >= cfg.max_target_ms,
            "capacity must be able to hold the largest cushion"
        );
        let capacity_frames = super::ms_to_frames(cfg.capacity_ms, cfg.rate);
        Self {
            buf: VecDeque::with_capacity(capacity_frames),
            capacity_frames,
            target_ms: cfg.target_ms,
            cfg,
            playing: false,
            frames_pulled: 0,
            underruns: 0,
            underrun_frames: 0,
            primed_silence_frames: 0,
            dropped_newest: 0,
            target_raises: 0,
            target_lowers: 0,
            window_start_frame: 0,
            window_min_level: usize::MAX,
        }
    }

    pub fn for_rate(rate: u32) -> Self {
        Self::new(JitterConfig::for_rate(rate))
    }

    /// Queues decoded frames. Returns how many were refused by the memory guard.
    pub fn push(&mut self, frames: &[i16]) -> usize {
        let room = self.capacity_frames - self.buf.len();
        let accepted = room.min(frames.len());
        self.buf.extend(frames[..accepted].iter().copied());
        let dropped = frames.len() - accepted;
        self.dropped_newest += dropped as u64;
        dropped
    }

    /// Fills `out` for the device callback, zero-filling whatever it cannot supply, and
    /// advances the buffer's clock by `out.len()` frames because that is the time the
    /// device just consumed.
    pub fn pull(&mut self, out: &mut [i16]) -> (usize, PullOutcome) {
        let wanted = out.len();

        if !self.playing && self.buf.len() >= self.target_frames() {
            self.playing = true;
        }

        let filled = if self.playing {
            let take = wanted.min(self.buf.len());
            for slot in out[..take].iter_mut() {
                *slot = self.buf.pop_front().expect("level was just measured");
            }
            take
        } else {
            0
        };
        out[filled..].fill(0);

        let outcome = if !self.playing {
            self.primed_silence_frames += wanted as u64;
            PullOutcome::Priming
        } else if filled < wanted {
            // Playback had started and ran dry: the listener hears a gap. Re-prime, and
            // lengthen the cushion so the next arrival pattern has more slack.
            self.underruns += 1;
            self.underrun_frames += (wanted - filled) as u64;
            self.playing = false;
            self.raise_target();
            PullOutcome::Underrun {
                missing: wanted - filled,
            }
        } else {
            PullOutcome::Full
        };

        self.frames_pulled += wanted as u64;
        self.observe_window();
        (filled, outcome)
    }

    /// Barge-in (VOICE_AGENT §2 rule 2) drops queued assistant audio. The cushion the
    /// link earned is kept: the network did not change, only the turn did.
    pub fn flush(&mut self) -> usize {
        let dropped = self.buf.len();
        self.buf.clear();
        self.playing = false;
        dropped
    }

    fn target_frames(&self) -> usize {
        super::ms_to_frames(self.target_ms, self.cfg.rate)
    }

    fn raise_target(&mut self) {
        let raised = (self.target_ms + self.cfg.step_ms).min(self.cfg.max_target_ms);
        if raised != self.target_ms {
            self.target_ms = raised;
            self.target_raises += 1;
        }
    }

    fn lower_target(&mut self) {
        let lowered = self
            .target_ms
            .saturating_sub(self.cfg.step_ms)
            .max(self.cfg.min_target_ms);
        if lowered != self.target_ms {
            self.target_ms = lowered;
            self.target_lowers += 1;
        }
    }

    /// Decay half of the adaptation. If the queue never dropped near the cushion for a
    /// whole window of playback, the cushion is longer than the link needs and costs
    /// latency for nothing, so it comes down one step.
    fn observe_window(&mut self) {
        self.window_min_level = self.window_min_level.min(self.buf.len());

        let window_frames = super::ms_to_frames(self.cfg.decay_window_ms, self.cfg.rate) as u64;
        if self.frames_pulled - self.window_start_frame < window_frames {
            return;
        }

        let floor_ms = super::frames_to_ms(self.window_min_level, self.cfg.rate);
        if floor_ms > self.target_ms + self.cfg.decay_hysteresis_ms {
            self.lower_target();
        }
        self.window_start_frame = self.frames_pulled;
        self.window_min_level = usize::MAX;
    }

    pub fn config(&self) -> JitterConfig {
        self.cfg
    }

    pub fn target_ms(&self) -> u32 {
        self.target_ms
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn level_frames(&self) -> usize {
        self.buf.len()
    }

    pub fn level_ms(&self) -> u32 {
        super::frames_to_ms(self.buf.len(), self.cfg.rate)
    }

    /// PCM16 is two bytes per mono frame, which is the byte half of the spec's sizing.
    pub fn level_bytes(&self) -> usize {
        self.buf.len() * 2
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    pub fn capacity_bytes(&self) -> usize {
        self.capacity_frames * 2
    }

    /// The buffer's clock: frames the device has consumed.
    pub fn frames_pulled(&self) -> u64 {
        self.frames_pulled
    }

    /// The same clock in milliseconds. No wall-clock reading is involved.
    pub fn elapsed_ms(&self) -> u64 {
        self.frames_pulled * 1000 / self.cfg.rate as u64
    }

    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    pub fn underrun_frames(&self) -> u64 {
        self.underrun_frames
    }

    pub fn primed_silence_frames(&self) -> u64 {
        self.primed_silence_frames
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_newest
    }

    pub fn target_raises(&self) -> u64 {
        self.target_raises
    }

    pub fn target_lowers(&self) -> u64 {
        self.target_lowers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_target_never_exceeds_the_documented_maximum_under_sustained_late_frames() {
        let mut jitter = JitterBuffer::for_rate(1_000);
        let slab = vec![7i16; 400];
        let mut out = vec![0i16; 500];

        for _ in 0..30 {
            jitter.push(&slab);
            let (_, outcome) = jitter.pull(&mut out);
            assert!(matches!(outcome, PullOutcome::Underrun { .. }));
            assert!(jitter.target_ms() <= 400);
        }

        assert_eq!(jitter.target_ms(), 400);
        assert!(jitter.target_raises() > 0);
    }

    /// #209's live failure, deviceless: the provider streams a whole long reply
    /// several times faster than the device drains it. At the default capacity every
    /// frame must come out the other side — the memory guard is a backstop, and a
    /// backstop that fires on a plausible reply is the bug.
    #[test]
    fn a_4x_realtime_120s_reply_drains_completely_with_zero_drops() {
        let mut jitter = JitterBuffer::for_rate(48_000);
        // Deliberately NOT derived from CAPACITY_MS: the ruling's acceptance
        // property is that a 120 s reply survives, so the fixture must not
        // shrink if someone shrinks the constant.
        let reply_frames = super::super::ms_to_frames(120_000, 48_000);
        let tick_frames = 480; // 10 ms at 48 kHz, the output task's pull cadence
        let slab = vec![7i16; tick_frames * 4]; // provider runs 4× realtime
        let mut out = vec![0i16; tick_frames];

        let mut pushed = 0;
        let mut heard = 0;
        while pushed < reply_frames {
            let take = slab.len().min(reply_frames - pushed);
            let refused = jitter.push(&slab[..take]);
            assert_eq!(refused, 0, "the guard fired {pushed} frames into the reply");
            pushed += take;
            heard += jitter.pull(&mut out).0;
        }
        while jitter.level_frames() > 0 {
            heard += jitter.pull(&mut out).0;
        }

        assert_eq!(
            jitter.dropped_frames(),
            0,
            "no frame of the reply was eaten"
        );
        assert_eq!(heard, reply_frames, "every frame reached the device side");
    }

    /// Pins [`CAPACITY_MS`] itself: the default guard refuses exactly what exceeds
    /// 120 s when nothing drains, so the backstop is still a hard memory bound.
    #[test]
    fn the_default_guard_still_bounds_memory_at_the_documented_capacity() {
        let mut jitter = JitterBuffer::for_rate(48_000);
        // Retuning the backstop is a design change: update the sizing math in
        // CAPACITY_MS's doc together with this pin.
        assert_eq!(CAPACITY_MS, 120_000);
        let capacity = super::super::ms_to_frames(CAPACITY_MS, 48_000);
        let over = 48_000; // one second past the cap

        let payload = vec![3i16; capacity + over];
        let refused = jitter.push(&payload);

        assert_eq!(refused, over);
        assert_eq!(jitter.dropped_frames(), over as u64);
        assert_eq!(jitter.level_frames(), capacity);
    }

    /// #210 interaction, measured not tuned: the raised capacity leaves the adaptive
    /// target's raise path untouched (the planned turn-end underrun still raises one
    /// step), and by ending mid-reply truncation it lengthens the continuous playback
    /// the decay window can land in — during a deep-backlog reply the floor sits far
    /// above the target, so the target walks down to its minimum, then the turn
    /// boundary raises it one step. The decay mechanism itself is unchanged (a
    /// 2 s-pinned buffer also decayed when a window aligned — live saw 6 lowers over
    /// 25 turns of alignment luck); what changed is opportunity: replies now play out
    /// full-length. The upward ratchet #210 documents is unchanged in direction and
    /// untouched in mechanism; short replies behave exactly as before.
    #[test]
    fn a_long_buffered_reply_lets_the_target_decay_and_the_turn_end_still_raises() {
        let mut jitter = JitterBuffer::for_rate(48_000);
        let tick_frames = 480;
        let reply_frames = super::super::ms_to_frames(120_000, 48_000);
        let slab = vec![7i16; tick_frames * 4];
        let mut out = vec![0i16; tick_frames];

        let mut pushed = 0;
        while pushed < reply_frames {
            let take = slab.len().min(reply_frames - pushed);
            jitter.push(&slab[..take]);
            pushed += take;
            jitter.pull(&mut out);
        }
        while jitter.level_frames() > 0 {
            jitter.pull(&mut out);
        }
        // 120 s at 48 kHz is 5 760 000 frames — an exact multiple of the 480-frame
        // pull, so the drain empties on a pull boundary and records no underrun of
        // its own; change either figure and the asserts below need rethinking.
        //
        // The deep backlog kept the window floor above target + hysteresis, so the
        // 150 ms start decayed to the 60 ms minimum (three 30 ms steps) mid-reply.
        assert_eq!(jitter.target_ms(), 60);
        assert_eq!(jitter.target_lowers(), 3);
        assert_eq!(
            jitter.underruns(),
            0,
            "a buffered reply never runs dry mid-play"
        );

        // The planned turn-boundary underrun (#210's raise driver) is unchanged.
        let (_, outcome) = jitter.pull(&mut out);
        assert!(matches!(outcome, PullOutcome::Underrun { .. }));
        assert_eq!(jitter.target_ms(), 90);
        assert_eq!(jitter.target_raises(), 1);
    }

    #[test]
    fn overflow_drops_the_newest_frames_and_keeps_the_oldest() {
        let mut jitter = JitterBuffer::new(JitterConfig {
            rate: 1_000,
            target_ms: 10,
            min_target_ms: 10,
            max_target_ms: 20,
            step_ms: 5,
            capacity_ms: 20,
            decay_window_ms: 100,
            decay_hysteresis_ms: 5,
        });
        let payload: Vec<i16> = (1..=30).collect();

        let refused = jitter.push(&payload);

        assert_eq!(refused, 10);
        assert_eq!(jitter.dropped_frames(), 10);

        let mut out = vec![0i16; 20];
        let (filled, outcome) = jitter.pull(&mut out);

        assert_eq!(filled, 20);
        assert_eq!(outcome, PullOutcome::Full);
        assert_eq!(out, (1..=20).collect::<Vec<i16>>());
    }

    #[test]
    fn target_decays_one_step_per_quiet_window_and_hysteresis_blocks_the_borderline_floor() {
        let mut jitter = JitterBuffer::new(JitterConfig {
            rate: 1_000,
            target_ms: 100,
            min_target_ms: 50,
            max_target_ms: 200,
            step_ms: 25,
            capacity_ms: 2_000,
            decay_window_ms: 200,
            decay_hysteresis_ms: 40,
        });

        jitter.push(&[1i16; 100]);
        let mut burst_out = vec![0i16; 150];
        let (_, outcome) = jitter.pull(&mut burst_out);
        assert!(matches!(outcome, PullOutcome::Underrun { .. }));
        assert_eq!(jitter.target_ms(), 125);

        jitter.push(&[1i16; 600]);
        let mut out = vec![0i16; 50];
        jitter.pull(&mut out);
        assert_eq!(jitter.frames_pulled(), 200);
        assert_eq!(jitter.target_ms(), 125);
        assert_eq!(jitter.target_lowers(), 0);

        for pulls_in_window in 1..=4u64 {
            jitter.push(&[1i16; 50]);
            jitter.pull(&mut out);
            if pulls_in_window < 4 {
                assert_eq!(jitter.target_ms(), 125);
            }
        }
        assert_eq!(jitter.frames_pulled(), 400);
        assert_eq!(jitter.target_ms(), 100);
        assert_eq!(jitter.target_lowers(), 1);

        for _ in 0..4 {
            jitter.push(&[1i16; 50]);
            jitter.pull(&mut out);
        }
        assert_eq!(jitter.frames_pulled(), 600);
        assert_eq!(jitter.target_ms(), 75);
        assert_eq!(jitter.target_lowers(), 2);

        let mut borderline_out = vec![0i16; 435];
        let (filled, outcome) = jitter.pull(&mut borderline_out);
        assert_eq!(filled, 435);
        assert_eq!(outcome, PullOutcome::Full);
        assert_eq!(jitter.level_ms(), 115);
        assert_eq!(jitter.target_ms(), 75);
        assert_eq!(jitter.target_lowers(), 2);
    }

    #[test]
    fn priming_silence_is_counted_as_expected_while_underrun_is_the_counted_audible_failure() {
        let mut jitter = JitterBuffer::new(JitterConfig {
            rate: 1_000,
            target_ms: 100,
            min_target_ms: 50,
            max_target_ms: 200,
            step_ms: 25,
            capacity_ms: 500,
            decay_window_ms: 10_000,
            decay_hysteresis_ms: 40,
        });
        let mut out = vec![9i16; 40];

        let (filled, outcome) = jitter.pull(&mut out);
        assert_eq!(filled, 0);
        assert_eq!(outcome, PullOutcome::Priming);
        assert!(out.iter().all(|&s| s == 0));
        assert_eq!(jitter.primed_silence_frames(), 40);
        assert_eq!(jitter.underruns(), 0);
        assert_eq!(jitter.target_ms(), 100);
        assert!(!jitter.is_playing());

        jitter.push(&[3i16; 60]);
        let (filled, outcome) = jitter.pull(&mut out);
        assert_eq!(filled, 0);
        assert_eq!(outcome, PullOutcome::Priming);
        assert_eq!(jitter.level_frames(), 60);
        assert_eq!(jitter.primed_silence_frames(), 80);
        assert_eq!(jitter.underruns(), 0);

        jitter.push(&[3i16; 40]);
        let mut small_out = vec![0i16; 30];
        let (filled, outcome) = jitter.pull(&mut small_out);
        assert_eq!(filled, 30);
        assert_eq!(outcome, PullOutcome::Full);
        assert!(jitter.is_playing());

        let mut dry_out = vec![0i16; 100];
        let (filled, outcome) = jitter.pull(&mut dry_out);
        assert_eq!(filled, 70);
        assert_eq!(outcome, PullOutcome::Underrun { missing: 30 });
        assert_eq!(jitter.underruns(), 1);
        assert_eq!(jitter.underrun_frames(), 30);
        assert_eq!(jitter.primed_silence_frames(), 80);
        assert_eq!(jitter.target_ms(), 125);
        assert!(!jitter.is_playing());

        let mut after_out = vec![0i16; 20];
        let (filled, outcome) = jitter.pull(&mut after_out);
        assert_eq!(filled, 0);
        assert_eq!(outcome, PullOutcome::Priming);
        assert_eq!(jitter.primed_silence_frames(), 100);
        assert_eq!(jitter.underruns(), 1);
    }

    #[test]
    fn flush_drops_queued_audio_but_keeps_the_earned_cushion_and_reprimes_against_it() {
        let mut jitter = JitterBuffer::new(JitterConfig {
            rate: 1_000,
            target_ms: 100,
            min_target_ms: 50,
            max_target_ms: 200,
            step_ms: 25,
            capacity_ms: 500,
            decay_window_ms: 10_000,
            decay_hysteresis_ms: 40,
        });

        jitter.push(&[5i16; 100]);
        let mut burst_out = vec![0i16; 150];
        let (_, outcome) = jitter.pull(&mut burst_out);
        assert!(matches!(outcome, PullOutcome::Underrun { .. }));
        assert_eq!(jitter.target_ms(), 125);

        jitter.push(&[5i16; 300]);
        let mut out = vec![0i16; 50];
        let (filled, outcome) = jitter.pull(&mut out);
        assert_eq!(filled, 50);
        assert_eq!(outcome, PullOutcome::Full);
        assert!(jitter.is_playing());

        let flushed = jitter.flush();
        assert_eq!(flushed, 250);
        assert_eq!(jitter.level_frames(), 0);
        assert!(!jitter.is_playing());
        assert_eq!(jitter.target_ms(), 125);
        assert_eq!(jitter.dropped_frames(), 0);
        assert_eq!(jitter.underruns(), 1);

        jitter.push(&[5i16; 100]);
        let (filled, outcome) = jitter.pull(&mut out);
        assert_eq!(filled, 0);
        assert_eq!(outcome, PullOutcome::Priming);
        assert_eq!(jitter.level_frames(), 100);

        jitter.push(&[5i16; 25]);
        let (filled, outcome) = jitter.pull(&mut out);
        assert_eq!(filled, 50);
        assert_eq!(outcome, PullOutcome::Full);
        assert!(jitter.is_playing());
        assert_eq!(jitter.underruns(), 1);
    }
}
