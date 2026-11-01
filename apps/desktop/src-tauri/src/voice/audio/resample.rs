//! Provenance (AGENTS.md §5a): rubato 4.0.0 synchronous FFT resampler.
//! Source: crate source read at ~/.cargo/registry/src/*/rubato-4.0.0/src/{lib.rs,synchro.rs}
//! and audioadapter-buffers-4.0.0/src/number_to_float.rs
//! Checked: 2026-08-18. API facts this module depends on:
//!   - `Fft::<f32>::new(rate_in, rate_out, chunk_size, channels, FixedSync::Input)`; the
//!     fixed side accepts exactly `input_frames_next()` frames per call and the other
//!     side varies, so the caller must buffer input to chunk boundaries.
//!   - `Resampler::process_into_buffer(&dyn Adapter<T>, &mut dyn AdapterMut<T>, Option<&Indexing>)`
//!     returns `(input_frames_consumed, output_frames_written)` and allocates nothing.
//!   - `InterleavedNumbers::<&[i16], f32>::new(buf, channels, frames)` (and `new_mut`)
//!     adapt an i16 PCM slice as a float buffer, so PCM16 never needs a manual
//!     conversion pass. Both return `Result<_, SizeError>`.
//!   - `Resampler::output_delay()` is the resampler's own group delay in output frames;
//!     for `Fft` it is `fft_size_out / 2`.
//!   - `rubato` re-exports `audioadapter` and `audioadapter_buffers` (lib.rs:6-7), so the
//!     adapter types are reached through `rubato::` rather than as a second direct
//!     dependency. That keeps one version pin instead of two that can drift apart.
//!   - With `FixedSync::Input`, `input_frames_next()` is a constant `chunk_size`, the
//!     produced count varies per call, and `output_frames_max()` is a true upper bound,
//!     so one scratch buffer allocated at construction serves every call.
//!
//! VOICE_AGENT §2/§6: both directions live on the tokio side of the rings — device rate
//! up to 16 kHz for `send_audio`, and provider 24 kHz down to the device rate because
//! many hosts refuse to open a 24 kHz output stream.
//!
//! The invariant these seams exist to keep is **real-time preservation**: a chunk of
//! audio that lasts `d` milliseconds going in must last `d` milliseconds coming out, and
//! the error must not accumulate across a long stream. R-T (TASKS.md) is the local
//! precedent for taking that seriously and for measuring it on the samples rather than
//! against a constant.

use rubato::audioadapter_buffers::number_to_float::InterleavedNumbers;
use rubato::{Fft, FixedSync, Resampler};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResampleSeamError {
    Construction(String),
    Process(String),
}

impl std::fmt::Display for ResampleSeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResampleSeamError::Construction(detail) => {
                write!(f, "the resampler could not be created ({detail})")
            }
            ResampleSeamError::Process(detail) => {
                write!(f, "the resampler failed on a chunk ({detail})")
            }
        }
    }
}

impl std::error::Error for ResampleSeamError {}

enum Engine {
    /// A device already running at the target rate needs no filtering, and opening an
    /// FFT resampler for ratio 1 would add both delay and cost for nothing.
    Passthrough,
    Fft(Box<Fft<f32>>),
}

/// One direction of mono PCM16 rate conversion, fed in arbitrary-sized pushes.
pub struct MonoResampler {
    engine: Engine,
    rate_in: u32,
    rate_out: u32,
    pending: Vec<i16>,
    scratch_out: Vec<i16>,
    frames_in: u64,
    frames_out: u64,
}

impl MonoResampler {
    pub fn new(
        rate_in: u32,
        rate_out: u32,
        chunk_frames: usize,
    ) -> Result<Self, ResampleSeamError> {
        if rate_in == 0 || rate_out == 0 {
            return Err(ResampleSeamError::Construction(format!(
                "sample rates must be positive, got {rate_in} -> {rate_out}"
            )));
        }
        let engine = if rate_in == rate_out {
            Engine::Passthrough
        } else {
            let fft = Fft::<f32>::new(
                rate_in as usize,
                rate_out as usize,
                chunk_frames,
                1,
                FixedSync::Input,
            )
            .map_err(|err| ResampleSeamError::Construction(err.to_string()))?;
            Engine::Fft(Box::new(fft))
        };
        let scratch_out = match &engine {
            Engine::Passthrough => Vec::new(),
            Engine::Fft(fft) => vec![0i16; fft.output_frames_max()],
        };
        Ok(Self {
            engine,
            rate_in,
            rate_out,
            pending: Vec::with_capacity(chunk_frames * 2),
            scratch_out,
            frames_in: 0,
            frames_out: 0,
        })
    }

    /// Mic path: device rate up to the rate every channel is fed at.
    pub fn to_provider_rate(
        device_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, ResampleSeamError> {
        Self::new(device_rate, super::PROVIDER_INPUT_RATE, chunk_frames)
    }

    /// Playback path: provider response rate down to whatever the host opened.
    pub fn to_device_rate(
        device_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, ResampleSeamError> {
        Self::new(super::PROVIDER_OUTPUT_RATE, device_rate, chunk_frames)
    }

    pub fn rate_in(&self) -> u32 {
        self.rate_in
    }

    pub fn rate_out(&self) -> u32 {
        self.rate_out
    }

    /// Group delay in output frames, so the jitter buffer can charge it to the budget.
    pub fn output_delay_frames(&self) -> usize {
        match &self.engine {
            Engine::Passthrough => 0,
            Engine::Fft(fft) => fft.output_delay(),
        }
    }

    /// Input frames buffered because they do not yet complete a chunk.
    pub fn pending_frames(&self) -> usize {
        self.pending.len()
    }

    pub fn frames_in(&self) -> u64 {
        self.frames_in
    }

    pub fn frames_out(&self) -> u64 {
        self.frames_out
    }

    /// Appends converted frames to `out` and returns how many were appended. Consumes
    /// whole chunks only; a partial tail stays buffered for the next push.
    pub fn push(&mut self, frames: &[i16], out: &mut Vec<i16>) -> Result<usize, ResampleSeamError> {
        self.frames_in += frames.len() as u64;
        let before = out.len();

        match &mut self.engine {
            Engine::Passthrough => out.extend_from_slice(frames),
            Engine::Fft(fft) => {
                self.pending.extend_from_slice(frames);
                loop {
                    let needed = fft.input_frames_next();
                    if self.pending.len() < needed {
                        break;
                    }
                    let input =
                        InterleavedNumbers::<&[i16], f32>::new(&self.pending[..needed], 1, needed)
                            .map_err(|err| ResampleSeamError::Process(err.to_string()))?;
                    let capacity = self.scratch_out.len();
                    let mut output = InterleavedNumbers::<&mut [i16], f32>::new_mut(
                        &mut self.scratch_out[..capacity],
                        1,
                        capacity,
                    )
                    .map_err(|err| ResampleSeamError::Process(err.to_string()))?;
                    let (consumed, produced) =
                        fft.process_into_buffer(&input, &mut output, None)
                            .map_err(|err| ResampleSeamError::Process(err.to_string()))?;
                    out.extend_from_slice(&self.scratch_out[..produced]);
                    self.pending.drain(..consumed);
                }
            }
        }

        let appended = out.len() - before;
        self.frames_out += appended as u64;
        Ok(appended)
    }
}

impl MonoResampler {
    /// Drops buffered input and the filter's internal overlap state, and restarts the
    /// length arithmetic. Barge-in (VOICE_AGENT §2 rule 2) discards queued assistant
    /// audio, and the frames still inside the down-resampler are part of that queue:
    /// without this they would be mixed into the next response's first chunk.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.frames_in = 0;
        self.frames_out = 0;
        if let Engine::Fft(fft) = &mut self.engine {
            fft.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20 ms chunk at each rate this seam is expected to meet, stated as frames so the
    /// tests below never read a production constant to build their own expectation.
    const CHUNK_48K: usize = 960;
    const CHUNK_44K1: usize = 882;
    const CHUNK_24K: usize = 480;
    const CHUNK_16K: usize = 320;

    fn sine(rate: u32, hz: f64, amplitude: i16, frames: usize) -> Vec<i16> {
        (0..frames)
            .map(|n| {
                let phase = 2.0 * std::f64::consts::PI * hz * (n as f64) / (rate as f64);
                (phase.sin() * amplitude as f64).round() as i16
            })
            .collect()
    }

    /// Counts sign changes, ignoring exact zeros. A tone of `f` Hz lasting `d` seconds
    /// crosses zero `2 * f * d` times, so this measures the frequency of the samples
    /// themselves rather than trusting a frame count (the R-T lesson: a resampler can
    /// emit the right number of frames and still be wrong about their content).
    fn zero_crossings(samples: &[i16]) -> usize {
        let mut crossings = 0;
        let mut last = 0i16;
        for &s in samples {
            if s == 0 {
                continue;
            }
            if last != 0 && (s > 0) != (last > 0) {
                crossings += 1;
            }
            last = s;
        }
        crossings
    }

    fn peak(samples: &[i16]) -> i16 {
        samples
            .iter()
            .map(|s| s.saturating_abs())
            .max()
            .unwrap_or(0)
    }

    // ----- length arithmetic, up direction (mic: device rate -> 16 kHz) -----

    /// The 48 kHz case is the only one of the four that is not frame-exact per chunk,
    /// so its arithmetic is spelled out here and pinned.
    ///
    /// rubato 4.0 `Fft::new(48000, 16000, 960, 1, FixedSync::Input)` derives, from the
    /// crate source read in this module's header:
    ///   sub_chunks    = max(960 / 256, 1)                = 3
    ///   gcd           = gcd(48000, 16000)                = 16000
    ///   min_chunk_in  = 48000 / 16000                    = 3
    ///   fft_chunks    = ceil((960 / 3) / 3) = ceil(106.6) = 107
    ///   fft_size_in   = 107 * 3                          = 321
    ///   fft_size_out  = 107 * 1                          = 107
    /// Each call consumes the fixed 960 frames and emits whole 107-frame sub-chunks:
    /// floor((960 + saved) / 321) of them, carrying the remainder forward. `saved`
    /// walks 0 -> 318 -> 315 -> ... -> 3 -> 0, so the pattern is one 214-frame call
    /// followed by 106 calls of 321, and it closes exactly after 107 calls:
    ///   input  107 * 960 = 102720 frames = 2140 ms at 48 kHz
    ///   output 214 + 106 * 321 = 34240 frames = 2140 ms at 16 kHz
    /// 102720 / 34240 = 3 exactly. That is real-time preservation with zero residue,
    /// and it is the property the seam exists to keep.
    #[test]
    fn up_seam_from_48k_closes_its_length_arithmetic_exactly_every_cycle() {
        const CYCLE_PUSHES: usize = 107;
        const CYCLE_IN_FRAMES: usize = 102_720;
        const CYCLE_OUT_FRAMES: usize = 34_240;
        const FFT_OUT: usize = 107;

        let mut seam = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        let chunk = vec![0i16; CHUNK_48K];
        let mut out = Vec::new();

        let mut produced = Vec::new();
        for _ in 0..CYCLE_PUSHES {
            let n = seam.push(&chunk, &mut out).expect("push");
            produced.push(n);
        }

        // Every call emits a whole number of filter sub-chunks, never a partial one.
        for (index, n) in produced.iter().enumerate() {
            assert_eq!(
                n % FFT_OUT,
                0,
                "call {index} emitted {n} frames, which is not a whole multiple of {FFT_OUT}"
            );
        }
        assert_eq!(
            produced[0], 214,
            "the first call has no carried remainder yet"
        );
        assert_eq!(
            produced[1..]
                .iter()
                .collect::<std::collections::BTreeSet<_>>(),
            [321usize].iter().collect::<std::collections::BTreeSet<_>>(),
            "after the first call every call in the cycle emits 321 frames"
        );

        assert_eq!(seam.frames_in(), CYCLE_IN_FRAMES as u64);
        assert_eq!(seam.frames_out(), CYCLE_OUT_FRAMES as u64);
        assert_eq!(out.len(), CYCLE_OUT_FRAMES);

        // The real-time claim, as a cross-multiplication so no float rounding hides in it.
        assert_eq!(
            seam.frames_out() * 48_000,
            seam.frames_in() * 16_000,
            "{} in / {} out is not exactly 3:1",
            seam.frames_in(),
            seam.frames_out()
        );
        assert_eq!(
            CYCLE_IN_FRAMES * 1000 / 48_000,
            2140,
            "input duration in ms"
        );
        assert_eq!(
            CYCLE_OUT_FRAMES * 1000 / 16_000,
            2140,
            "output duration in ms"
        );
    }

    /// The invariant that matters over a long call is that the shortfall stays bounded
    /// instead of accumulating. Three cycles: the deficit never exceeds one filter
    /// sub-chunk and is exactly zero at every cycle boundary.
    #[test]
    fn up_seam_from_48k_never_accumulates_drift_across_cycles() {
        const CYCLE_PUSHES: usize = 107;
        const CYCLES: usize = 3;
        const MAX_DEFICIT_FRAMES: usize = 106;
        const MAX_DEFICIT_MS: usize = 6; // 106 frames / 16 kHz = 6.625 ms, truncated

        let mut seam = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        let chunk = vec![0i16; CHUNK_48K];
        let mut out = Vec::new();

        let mut worst_deficit = 0usize;
        for push in 1..=CYCLE_PUSHES * CYCLES {
            seam.push(&chunk, &mut out).expect("push");
            // 3 input frames per output frame, so the ideal output is in/3.
            let ideal = push * CHUNK_48K / 3;
            let deficit = ideal - out.len();
            worst_deficit = worst_deficit.max(deficit);
            if push % CYCLE_PUSHES == 0 {
                assert_eq!(
                    deficit, 0,
                    "cycle boundary at push {push} must land exactly on the ideal output"
                );
            }
        }

        assert_eq!(
            worst_deficit, MAX_DEFICIT_FRAMES,
            "the mid-cycle shortfall is one sub-chunk minus one frame and does not grow"
        );
        assert!(
            worst_deficit * 1000 / 16_000 <= MAX_DEFICIT_MS,
            "shortfall {worst_deficit} frames exceeds the {MAX_DEFICIT_MS} ms budget"
        );
    }

    /// The two tests above push exactly one filter chunk at a time, which a device
    /// callback never does. Under ragged pushes the conservation identity is what holds:
    /// every pushed frame is either already converted, still in our queue, or still in
    /// the filter's carry, and nothing else. Converted frames always come in whole
    /// 321-frame groups producing 107 output frames each, so
    ///
    ///   frames_in - pending - 3 * out_frames = the filter's carry, in [0, 321)
    ///
    /// That residue is bounded by a constant no matter how long the stream runs, which is
    /// the no-drift claim in the form that survives arbitrary chunk sizes. A seam that
    /// dropped its carried tail would show this residue growing without limit.
    #[test]
    fn up_seam_from_48k_loses_no_frame_under_ragged_pushes() {
        const FFT_IN: usize = 321;
        const RAGGED: [usize; 6] = [137, 960, 500, 1_921, 64, 703];
        const ROUNDS: usize = 400;

        let mut seam = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        let mut out = Vec::new();
        let mut worst_residue = 0usize;

        for round in 0..ROUNDS {
            for size in RAGGED {
                seam.push(&vec![0i16; size], &mut out).expect("push");

                let converted = out.len() * 3;
                let accounted = converted + seam.pending_frames();
                assert!(
                    accounted <= seam.frames_in() as usize,
                    "round {round}: {accounted} frames accounted for exceeds the \
                     {} pushed",
                    seam.frames_in()
                );
                let residue = seam.frames_in() as usize - accounted;
                assert!(
                    residue < FFT_IN,
                    "round {round}: the filter carry reached {residue} frames, past the \
                     {FFT_IN}-frame bound -- frames are being lost or invented"
                );
                worst_residue = worst_residue.max(residue);
            }
        }

        // Sanity that the run was long enough for a drift to have shown itself.
        let pushed: usize = RAGGED.iter().sum::<usize>() * ROUNDS;
        assert_eq!(seam.frames_in(), pushed as u64);
        assert!(
            pushed / 48_000 >= 35,
            "the ragged run covered only {} s of audio",
            pushed / 48_000
        );
        assert!(
            worst_residue > 0,
            "the filter never carried anything, so this pin proved nothing"
        );
    }

    /// 44.1 kHz is the other rate hosts actually hand out, and there the arithmetic is
    /// frame-exact per chunk: gcd is 100, min_chunk_in 441, fft_chunks
    /// ceil((882/3)/441) = 1, so fft_size_in 441 and fft_size_out 160, and a fixed
    /// 882-frame call emits exactly 2 * 160 = 320 frames with nothing carried.
    #[test]
    fn up_seam_from_44k1_is_frame_exact_on_every_chunk() {
        const PUSHES: usize = 50;

        let mut seam = MonoResampler::new(44_100, 16_000, CHUNK_44K1).expect("construct");
        let chunk = vec![0i16; CHUNK_44K1];
        let mut out = Vec::new();

        for push in 1..=PUSHES {
            let n = seam.push(&chunk, &mut out).expect("push");
            assert_eq!(n, CHUNK_16K, "push {push} converted 20 ms to {n} frames");
            assert_eq!(
                seam.pending_frames(),
                0,
                "nothing is carried at push {push}"
            );
            assert_eq!(out.len(), push * CHUNK_16K);
        }

        assert_eq!(seam.frames_out() * 44_100, seam.frames_in() * 16_000);
        assert_eq!(CHUNK_44K1 * 1000 / 44_100, 20, "input chunk is 20 ms");
        assert_eq!(CHUNK_16K * 1000 / 16_000, 20, "output chunk is 20 ms");
    }

    // ----- length arithmetic, down direction (playback: 24 kHz -> device rate) -----

    /// 24 kHz -> 48 kHz: gcd 24000, min_chunk_in 1, fft_size_in 480, fft_size_out 960.
    /// A fixed 480-frame call emits exactly 960 frames, so 20 ms in is 20 ms out.
    #[test]
    fn down_seam_to_48k_is_frame_exact_on_every_chunk() {
        const PUSHES: usize = 50;
        const OUT_PER_CHUNK: usize = 960;

        let mut seam = MonoResampler::new(24_000, 48_000, CHUNK_24K).expect("construct");
        let chunk = vec![0i16; CHUNK_24K];
        let mut out = Vec::new();

        for push in 1..=PUSHES {
            let n = seam.push(&chunk, &mut out).expect("push");
            assert_eq!(
                n, OUT_PER_CHUNK,
                "push {push} converted 20 ms to {n} frames"
            );
            assert_eq!(seam.pending_frames(), 0);
        }

        assert_eq!(out.len(), PUSHES * OUT_PER_CHUNK);
        assert_eq!(seam.frames_out() * 24_000, seam.frames_in() * 48_000);
        assert_eq!(CHUNK_24K * 1000 / 24_000, 20, "input chunk is 20 ms");
        assert_eq!(OUT_PER_CHUNK * 1000 / 48_000, 20, "output chunk is 20 ms");
    }

    /// 24 kHz -> 44.1 kHz is the awkward ratio (147/80) and it is still frame-exact:
    /// gcd 300, min_chunk_in 80, fft_chunks ceil(480/80) = 6, fft_size_in 480,
    /// fft_size_out 6 * 147 = 882. So 480 frames in emits 882 frames out, and
    /// 480/24000 = 882/44100 = 20 ms.
    #[test]
    fn down_seam_to_44k1_is_frame_exact_on_every_chunk() {
        const PUSHES: usize = 50;
        const OUT_PER_CHUNK: usize = 882;

        let mut seam = MonoResampler::new(24_000, 44_100, CHUNK_24K).expect("construct");
        let chunk = vec![0i16; CHUNK_24K];
        let mut out = Vec::new();

        for push in 1..=PUSHES {
            let n = seam.push(&chunk, &mut out).expect("push");
            assert_eq!(
                n, OUT_PER_CHUNK,
                "push {push} converted 20 ms to {n} frames"
            );
        }

        assert_eq!(out.len(), PUSHES * OUT_PER_CHUNK);
        assert_eq!(seam.frames_out() * 24_000, seam.frames_in() * 44_100);
        assert_eq!(CHUNK_24K * 1000 / 24_000, 20, "input chunk is 20 ms");
        assert_eq!(OUT_PER_CHUNK * 1000 / 44_100, 20, "output chunk is 20 ms");
    }

    // ----- content: the frames must still be the same sound -----

    /// A frame count can be right while the audio is wrong. A 1 kHz tone pushed through
    /// the up seam must still be a 1 kHz tone at the new rate, measured on the samples.
    #[test]
    fn up_seam_preserves_the_frequency_of_a_tone() {
        const TONE_HZ: f64 = 1000.0;
        const AMPLITUDE: i16 = 12_000;
        const PUSHES: usize = 50;
        const SKIP_FRAMES: usize = 160; // past the 53-frame group delay and its settling
        const MEASURE_FRAMES: usize = 15_000;
        // 15000 frames at 16 kHz is 937.5 ms; a 1 kHz tone crosses zero 2 * 1000 * 0.9375.
        const EXPECTED_CROSSINGS: usize = 1875;
        const CROSSING_TOLERANCE: usize = 4;

        let mut seam = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        let input = sine(48_000, TONE_HZ, AMPLITUDE, CHUNK_48K * PUSHES);
        let mut out = Vec::new();
        for chunk in input.chunks(CHUNK_48K) {
            seam.push(chunk, &mut out).expect("push");
        }

        assert!(
            out.len() >= SKIP_FRAMES + MEASURE_FRAMES,
            "only {} output frames to measure",
            out.len()
        );
        let measured = &out[SKIP_FRAMES..SKIP_FRAMES + MEASURE_FRAMES];
        let crossings = zero_crossings(measured);
        assert!(
            crossings.abs_diff(EXPECTED_CROSSINGS) <= CROSSING_TOLERANCE,
            "measured {crossings} zero crossings, expected {EXPECTED_CROSSINGS} \
             +/- {CROSSING_TOLERANCE}: the tone changed pitch"
        );

        // Guards the crossing count against passing on silence or a wrecked gain.
        let measured_peak = peak(measured);
        assert!(
            (9_000..=13_000).contains(&measured_peak),
            "peak amplitude {measured_peak} left the band around the {AMPLITUDE} input"
        );
    }

    /// The same claim in the other direction, where the seam is interpolating up.
    #[test]
    fn down_seam_preserves_the_frequency_of_a_tone() {
        const TONE_HZ: f64 = 1000.0;
        const AMPLITUDE: i16 = 12_000;
        const PUSHES: usize = 50;
        const SKIP_FRAMES: usize = 1_000; // past the 480-frame group delay
        const MEASURE_FRAMES: usize = 38_400;
        // 38400 frames at 48 kHz is 800 ms; a 1 kHz tone crosses zero 2 * 1000 * 0.8.
        const EXPECTED_CROSSINGS: usize = 1600;
        const CROSSING_TOLERANCE: usize = 4;

        let mut seam = MonoResampler::new(24_000, 48_000, CHUNK_24K).expect("construct");
        let input = sine(24_000, TONE_HZ, AMPLITUDE, CHUNK_24K * PUSHES);
        let mut out = Vec::new();
        for chunk in input.chunks(CHUNK_24K) {
            seam.push(chunk, &mut out).expect("push");
        }

        let measured = &out[SKIP_FRAMES..SKIP_FRAMES + MEASURE_FRAMES];
        let crossings = zero_crossings(measured);
        assert!(
            crossings.abs_diff(EXPECTED_CROSSINGS) <= CROSSING_TOLERANCE,
            "measured {crossings} zero crossings, expected {EXPECTED_CROSSINGS} \
             +/- {CROSSING_TOLERANCE}: the tone changed pitch"
        );
        let measured_peak = peak(measured);
        assert!(
            (9_000..=13_000).contains(&measured_peak),
            "peak amplitude {measured_peak} left the band around the {AMPLITUDE} input"
        );
    }

    // ----- chunking, passthrough, delay, refusals -----

    /// Pushes do not have to arrive on chunk boundaries. The seam consumes whole filter
    /// chunks and carries the tail, so nothing is dropped and nothing is invented:
    /// 500 frames buffer with no output, the next 500 complete one 960-frame chunk
    /// (emitting the first cycle's 214 frames) and carry 1000 - 960 = 40 frames.
    #[test]
    fn partial_pushes_are_carried_to_the_next_chunk_boundary() {
        let mut seam = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        let mut out = Vec::new();

        let first = seam.push(&vec![0i16; 500], &mut out).expect("push");
        assert_eq!(first, 0, "500 frames cannot complete a 960-frame chunk");
        assert_eq!(seam.pending_frames(), 500);

        let second = seam.push(&vec![0i16; 500], &mut out).expect("push");
        assert_eq!(
            second, 214,
            "the completed chunk emits the first cycle's output"
        );
        assert_eq!(seam.pending_frames(), 40, "1000 - 960 frames are carried");

        assert_eq!(
            seam.frames_in(),
            1000,
            "every pushed frame is accounted for"
        );
        assert_eq!(seam.frames_out(), 214);
    }

    /// A device already at the target rate must not be filtered: no resampler, no delay,
    /// and the samples come out bit-identical.
    #[test]
    fn matched_rates_pass_samples_through_untouched() {
        let mut seam = MonoResampler::new(16_000, 16_000, CHUNK_16K).expect("construct");
        assert_eq!(
            seam.output_delay_frames(),
            0,
            "passthrough adds no group delay"
        );

        let input = sine(16_000, 1000.0, 12_000, CHUNK_16K * 3);
        let mut out = Vec::new();
        for chunk in input.chunks(CHUNK_16K) {
            let n = seam.push(chunk, &mut out).expect("push");
            assert_eq!(n, CHUNK_16K);
        }

        assert_eq!(out, input, "passthrough altered the samples");
        assert_eq!(seam.pending_frames(), 0);
        assert_eq!(seam.frames_in(), seam.frames_out());
    }

    /// The jitter buffer charges the filter's group delay to the latency budget, so the
    /// seam has to report it. For `Fft` it is `fft_size_out / 2`.
    #[test]
    fn group_delay_is_reported_as_half_the_output_fft_size() {
        let up = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        assert_eq!(up.output_delay_frames(), 53, "107 / 2, truncated");
        assert_eq!(up.rate_in(), 48_000);
        assert_eq!(up.rate_out(), 16_000);

        let down = MonoResampler::new(24_000, 48_000, CHUNK_24K).expect("construct");
        assert_eq!(down.output_delay_frames(), 480, "960 / 2");
        // 480 frames at 48 kHz is 10 ms, which is inside the 150 ms jitter allowance.
        assert_eq!(down.output_delay_frames() * 1000 / 48_000, 10);
    }

    /// The named constructors must aim at the rates the spec fixes, not at whatever the
    /// caller happens to pass, so a wiring mistake in unit 5 cannot go unnoticed.
    #[test]
    fn the_named_constructors_target_the_provider_rates() {
        let up = MonoResampler::to_provider_rate(48_000, CHUNK_48K).expect("construct");
        assert_eq!(up.rate_in(), 48_000);
        assert_eq!(up.rate_out(), 16_000, "mic audio is sent at 16 kHz");

        let down = MonoResampler::to_device_rate(44_100, CHUNK_24K).expect("construct");
        assert_eq!(down.rate_in(), 24_000, "provider audio arrives at 24 kHz");
        assert_eq!(down.rate_out(), 44_100);
    }

    /// Barge-in drops queued assistant audio; the frames inside the filter are part of
    /// that queue. After a reset the seam behaves like a fresh one.
    #[test]
    fn reset_clears_carried_frames_and_restarts_the_arithmetic() {
        let mut seam = MonoResampler::new(48_000, 16_000, CHUNK_48K).expect("construct");
        let mut out = Vec::new();

        // The filter's own carry has to be non-zero before the reset, or this test would
        // pass on a reset that never touches the filter. One full chunk leaves 960 - 642
        // = 318 frames saved inside it, which is why the *second* chunk emits 321 rather
        // than 214 -- that difference is the observable this pin rests on.
        let first = seam.push(&vec![0i16; CHUNK_48K], &mut out).expect("push");
        assert_eq!(first, 214, "the first chunk of a fresh seam");
        let second = seam.push(&vec![0i16; CHUNK_48K], &mut out).expect("push");
        assert_eq!(second, 321, "the filter is now carrying 318 input frames");

        // And our own queue has to be non-empty too, so the reset is proven on both.
        seam.push(&vec![0i16; 500], &mut out).expect("push");
        assert_eq!(seam.pending_frames(), 500);
        assert_eq!(seam.frames_in(), (CHUNK_48K * 2 + 500) as u64);

        seam.reset();
        assert_eq!(seam.pending_frames(), 0, "carried input survived the reset");
        assert_eq!(seam.frames_in(), 0);
        assert_eq!(seam.frames_out(), 0);

        // A fresh seam's first chunk emits 214; if the filter kept its 318 saved frames
        // this would emit 321 instead.
        out.clear();
        let after = seam.push(&vec![0i16; CHUNK_48K], &mut out).expect("push");
        assert_eq!(after, 214, "the filter carry was not restarted");
    }

    #[test]
    fn a_zero_sample_rate_is_refused_rather_than_dividing_by_it() {
        let Err(err) = MonoResampler::new(0, 16_000, CHUNK_48K) else {
            panic!("a zero input rate must be refused");
        };
        assert!(matches!(err, ResampleSeamError::Construction(_)));
        assert!(err.to_string().contains("0 -> 16000"), "{err}");

        let Err(err) = MonoResampler::new(48_000, 0, CHUNK_48K) else {
            panic!("a zero output rate must be refused");
        };
        assert!(matches!(err, ResampleSeamError::Construction(_)));
    }
}
