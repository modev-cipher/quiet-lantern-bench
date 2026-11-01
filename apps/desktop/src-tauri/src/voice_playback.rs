//! The production [`PlaybackControl`] adapter — the Tauri/webview bridge the
//! VA6 UI slice owes (`voice::tools::playback` module doc: "the VA6 UI slice
//! ships the one adapter that bridges these calls over Tauri events to the
//! store").
//!
//! The playing surface lives in the webview (`playbackStore.ts` /
//! `PlaybackEngine.tsx`); Rust owns *what* a tool does, the webview owns
//! *doing* it. This module is the seam's production half, in two parts:
//!
//! - **Inbound state** ([`PlaybackSurface`]): the webview reports its player
//!   snapshot — position, playing, speed — through the
//!   `voice_playback_report` command on every meaningful store change and on
//!   a heartbeat while a track is loaded, and clears it with
//!   `voice_playback_detach` when the track unloads. A snapshot older than
//!   [`SURFACE_TTL`] is treated as absent: a webview that died without
//!   detaching (reload, crash) must read as [`PlaybackUnavailable`], not as a
//!   frozen player the dispatcher keeps computing seeks against.
//! - **Outbound effects** ([`WebviewPlayback`]): every [`PlaybackControl`]
//!   method resolves to one [`PlaybackEffect`] delivered over the
//!   [`VOICE_PLAYBACK_EFFECT_EVENT`] Tauri event. Effects are **absolute
//!   state, never toggles** — pause means "be paused", seek carries the
//!   dispatcher-computed absolute target, speed carries the validated
//!   multiplier — so a duplicate delivery is a no-op by construction. The
//!   at-most-once *dispatch* guarantee lives upstream in the dispatcher's
//!   `answer_once` cache (harness recovery re-sends cached acks, it never
//!   re-executes); this adapter's job is to stay harmless if the same effect
//!   ever reaches the store twice anyway (webview re-attach mid-delivery).
//!
//! After a successful delivery the adapter folds the effect into its own
//! mirror of the snapshot (position extrapolated to now first, so a speed
//! change never re-scales already-elapsed time), which keeps the
//! dispatcher's follow-up arithmetic (`seek_relative` from `position_ms`)
//! honest between webview report ticks. A failed delivery updates nothing
//! and surfaces [`PlaybackUnavailable`] — the dispatcher already maps that
//! to a retryable tool error.
//!
//! Positions cross this seam in **milliseconds on the loaded item's
//! timeline** (the webview's `<audio>` clock, seconds×1000). Mapping the item
//! timeline onto the book timeline (chapter offsets) is the session
//! harness's job — the same place `SessionContext::playback_pos_ms` is
//! produced — not this adapter's.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::voice::tools::playback::{PlaybackControl, PlaybackUnavailable};

/// The Tauri event carrying [`PlaybackEffect`]s to the webview store bridge
/// (`voicePlaybackBridge.ts`).
pub const VOICE_PLAYBACK_EFFECT_EVENT: &str = "voice-playback-effect";

/// A report older than this is a dead surface. The webview heartbeats every
/// second while a track is loaded (playing or paused), so three missed beats
/// plus slack means the webview reloaded or crashed without detaching.
pub const SURFACE_TTL: Duration = Duration::from_secs(4);

/// One webview player snapshot, as last reported.
#[derive(Debug, Clone, Copy)]
struct Snapshot {
    pos_ms: u64,
    playing: bool,
    speed: f64,
    reported_at: Instant,
}

impl Snapshot {
    /// The position now: reports arrive on a cadence, so while playing the
    /// clock keeps running between them at the reported speed.
    fn extrapolated_ms(&self) -> u64 {
        if !self.playing {
            return self.pos_ms;
        }
        let elapsed_ms = self.reported_at.elapsed().as_millis() as f64 * self.speed.max(0.0);
        self.pos_ms.saturating_add(elapsed_ms as u64)
    }
}

/// The shared inbound half: managed Tauri state the report/detach commands
/// write and [`WebviewPlayback`] reads. `None` between tracks — no open book
/// is the ordinary [`PlaybackUnavailable`] case, not an error.
#[derive(Default)]
pub struct PlaybackSurface {
    snapshot: Mutex<Option<Snapshot>>,
    pending_seek: Mutex<Option<PendingSeek>>,
    seek_epochs: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct PendingSeek {
    epoch: u64,
    issued_at: Instant,
}

impl PlaybackSurface {
    pub fn report(&self, pos_ms: u64, playing: bool, speed: f64, seek_epoch: Option<u64>) {
        let mut pending = self.pending_seek.lock().unwrap();
        let mut snapshot = self.snapshot.lock().unwrap();
        if let Some(seek) = *pending {
            let applied = seek_epoch.is_some_and(|epoch| epoch >= seek.epoch);
            if !applied && seek.issued_at.elapsed() <= SURFACE_TTL {
                if let Some(current) = snapshot.as_mut() {
                    current.pos_ms = current.extrapolated_ms();
                    current.reported_at = Instant::now();
                    return;
                }
            }
            *pending = None;
        }
        *snapshot = Some(Snapshot {
            pos_ms,
            playing,
            speed,
            reported_at: Instant::now(),
        });
    }

    pub fn detach(&self) {
        *self.pending_seek.lock().unwrap() = None;
        *self.snapshot.lock().unwrap() = None;
    }

    fn begin_seek(&self) -> u64 {
        let epoch = self
            .seek_epochs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        *self.pending_seek.lock().unwrap() = Some(PendingSeek {
            epoch,
            issued_at: Instant::now(),
        });
        epoch
    }

    fn abandon_seek(&self, epoch: u64) {
        let mut pending = self.pending_seek.lock().unwrap();
        if pending.is_some_and(|seek| seek.epoch == epoch) {
            *pending = None;
        }
    }

    /// The live snapshot, or `Err` when none was ever reported, the track
    /// detached, or the last report aged out ([`SURFACE_TTL`]).
    fn live(&self) -> Result<Snapshot, PlaybackUnavailable> {
        self.snapshot
            .lock()
            .unwrap()
            .filter(|snapshot| snapshot.reported_at.elapsed() <= SURFACE_TTL)
            .ok_or(PlaybackUnavailable)
    }

    fn store(&self, snapshot: Snapshot) {
        *self.snapshot.lock().unwrap() = Some(snapshot);
    }
}

#[cfg(test)]
impl PlaybackSurface {
    fn backdate_seek(&self, by: Duration) {
        if let Some(seek) = self.pending_seek.lock().unwrap().as_mut() {
            seek.issued_at = seek
                .issued_at
                .checked_sub(by)
                .expect("test backdate underflows the monotonic clock");
        }
    }

    /// Test hook: age the last report by `by`, as if the webview had gone
    /// silent for that long (heartbeats stopped without a detach).
    fn backdate(&self, by: Duration) {
        let mut guard = self.snapshot.lock().unwrap();
        if let Some(snapshot) = guard.as_mut() {
            snapshot.reported_at = snapshot
                .reported_at
                .checked_sub(by)
                .expect("test backdate underflows the monotonic clock");
        }
    }
}

/// One playback effect on the webview player — absolute state, never a
/// toggle, so a duplicate delivery cannot flip anything (module doc).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlaybackEffect {
    Pause,
    Resume,
    /// Absolute target on the item timeline, already clamped by the
    /// dispatcher (`clamp_to_timeline`).
    Seek {
        pos_ms: u64,
        epoch: u64,
    },
    /// Multiplier already validated to the registry bounds by the
    /// dispatcher.
    Speed {
        speed: f64,
    },
}

/// Where effects go. Production is a `tauri::AppHandle` emitting
/// [`VOICE_PLAYBACK_EFFECT_EVENT`]; rigs substitute their own sink. Returns
/// `false` when the effect demonstrably could not be delivered.
///
/// `Send` because the sink rides `PlaybackControl` (itself `Send`: the
/// session runtime's actor task owns the port across await points).
pub trait EffectSink: Send {
    fn deliver(&self, effect: &PlaybackEffect) -> bool;
}

impl EffectSink for tauri::AppHandle {
    fn deliver(&self, effect: &PlaybackEffect) -> bool {
        tauri::Emitter::emit(self, VOICE_PLAYBACK_EFFECT_EVENT, effect).is_ok()
    }
}

/// The production playing-surface port: [`PlaybackControl`] over the webview
/// bridge. Constructed per session by the harness with the app's managed
/// [`PlaybackSurface`] and the `AppHandle` as its sink.
pub struct WebviewPlayback<S: EffectSink> {
    surface: Arc<PlaybackSurface>,
    sink: S,
}

impl<S: EffectSink> WebviewPlayback<S> {
    pub fn new(surface: Arc<PlaybackSurface>, sink: S) -> Self {
        Self { surface, sink }
    }

    /// The shared shape of every mutation: require a live surface, deliver
    /// the absolute effect, and only then fold it into the mirror (position
    /// extrapolated to now first). A failed delivery changes nothing.
    fn apply(
        &mut self,
        effect: PlaybackEffect,
        fold: impl FnOnce(&mut Snapshot),
    ) -> Result<(), PlaybackUnavailable> {
        let snapshot = self.surface.live()?;
        if !self.sink.deliver(&effect) {
            return Err(PlaybackUnavailable);
        }
        let mut folded = Snapshot {
            pos_ms: snapshot.extrapolated_ms(),
            reported_at: Instant::now(),
            ..snapshot
        };
        fold(&mut folded);
        self.surface.store(folded);
        Ok(())
    }
}

impl<S: EffectSink> PlaybackControl for WebviewPlayback<S> {
    fn position_ms(&self) -> Result<u64, PlaybackUnavailable> {
        Ok(self.surface.live()?.extrapolated_ms())
    }

    fn pause(&mut self) -> Result<(), PlaybackUnavailable> {
        self.apply(PlaybackEffect::Pause, |snapshot| snapshot.playing = false)
    }

    fn resume(&mut self) -> Result<(), PlaybackUnavailable> {
        self.apply(PlaybackEffect::Resume, |snapshot| snapshot.playing = true)
    }

    fn seek_to_ms(&mut self, pos_ms: u64) -> Result<(), PlaybackUnavailable> {
        let epoch = self.surface.begin_seek();
        let applied = self.apply(PlaybackEffect::Seek { pos_ms, epoch }, |snapshot| {
            snapshot.pos_ms = pos_ms;
        });
        if applied.is_err() {
            self.surface.abandon_seek(epoch);
        }
        applied
    }

    fn set_speed(&mut self, speed: f64) -> Result<(), PlaybackUnavailable> {
        self.apply(PlaybackEffect::Speed { speed }, |snapshot| {
            snapshot.speed = speed;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A rig sink: records every delivered effect, and can be armed to
    /// refuse the next delivery (webview gone mid-emit).
    #[derive(Default)]
    struct RecordingSink {
        delivered: Mutex<Vec<PlaybackEffect>>,
        fail_next: AtomicBool,
    }

    impl EffectSink for &RecordingSink {
        fn deliver(&self, effect: &PlaybackEffect) -> bool {
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return false;
            }
            self.delivered.lock().unwrap().push(effect.clone());
            true
        }
    }

    fn rig() -> (Arc<PlaybackSurface>, RecordingSink) {
        (
            Arc::new(PlaybackSurface::default()),
            RecordingSink::default(),
        )
    }

    #[test]
    fn without_a_report_every_method_is_playback_unavailable_and_nothing_is_delivered() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        assert_eq!(playback.position_ms(), Err(PlaybackUnavailable));
        assert_eq!(playback.pause(), Err(PlaybackUnavailable));
        assert_eq!(playback.resume(), Err(PlaybackUnavailable));
        assert_eq!(playback.seek_to_ms(1_000), Err(PlaybackUnavailable));
        assert_eq!(playback.set_speed(1.5), Err(PlaybackUnavailable));
        assert!(
            sink.delivered.lock().unwrap().is_empty(),
            "no live surface, no effect: the webview must never receive \
             commands for a player that does not exist"
        );
    }

    #[test]
    fn a_report_older_than_the_ttl_reads_as_a_dead_surface_and_a_fresh_report_recovers_it() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);
        assert_eq!(playback.position_ms(), Ok(10_000));

        // Three missed heartbeats plus slack: the webview reloaded or died
        // without detaching. It must read as PlaybackUnavailable, not as a
        // frozen player the dispatcher keeps computing seeks against.
        surface.backdate(SURFACE_TTL + Duration::from_secs(1));
        assert_eq!(playback.position_ms(), Err(PlaybackUnavailable));
        assert_eq!(playback.pause(), Err(PlaybackUnavailable));
        assert!(sink.delivered.lock().unwrap().is_empty());

        // The next snapshot revives the surface (webview reloaded and the
        // bridge reported again): no restart ceremony required.
        surface.report(42_000, false, 1.0, None);
        assert_eq!(playback.position_ms(), Ok(42_000));
        assert_eq!(playback.pause(), Ok(()));
        assert_eq!(*sink.delivered.lock().unwrap(), vec![PlaybackEffect::Pause]);
    }

    #[test]
    fn a_report_within_the_ttl_stays_live() {
        let (surface, sink) = rig();
        let playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);
        surface.backdate(SURFACE_TTL - Duration::from_secs(1));
        assert_eq!(playback.position_ms(), Ok(10_000));
    }

    #[test]
    fn a_detached_surface_is_unavailable() {
        let (surface, sink) = rig();
        let playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, true, 1.0, None);
        surface.detach();
        assert_eq!(playback.position_ms(), Err(PlaybackUnavailable));
    }

    #[test]
    fn a_failed_delivery_mutates_nothing() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);

        sink.fail_next.store(true, Ordering::SeqCst);
        assert_eq!(playback.seek_to_ms(50_000), Err(PlaybackUnavailable));
        assert!(sink.delivered.lock().unwrap().is_empty());
        assert_eq!(
            playback.position_ms(),
            Ok(10_000),
            "the mirror must not fold an effect the webview never received"
        );
    }

    #[test]
    fn while_paused_the_position_does_not_extrapolate() {
        let (surface, sink) = rig();
        let playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 2.0, None);
        surface.backdate(Duration::from_secs(1));
        assert_eq!(playback.position_ms(), Ok(10_000));
    }

    #[test]
    fn while_playing_the_position_extrapolates_at_the_reported_speed() {
        let (surface, sink) = rig();
        let playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, true, 2.0, None);
        surface.backdate(Duration::from_secs(1));
        let position = playback.position_ms().unwrap();
        // 1 s of wall clock at 2.0x is 2 000 ms of item timeline; the upper
        // bound only allows test-execution slack.
        assert!(
            (12_000..12_800).contains(&position),
            "1s at 2.0x must extrapolate to ~12_000, got {position}"
        );
    }

    #[test]
    fn a_speed_change_folds_elapsed_time_at_the_old_speed_first() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, true, 2.0, None);
        surface.backdate(Duration::from_secs(1));

        playback.set_speed(1.0).unwrap();
        let position = playback.position_ms().unwrap();
        // The elapsed second ran at 2.0x (≈ +2 000 ms) BEFORE the new speed
        // took over; re-scaling already-elapsed time at 1.0x would read
        // ≈ 11_000 instead.
        assert!(
            (12_000..12_800).contains(&position),
            "fold-first: elapsed time keeps its original speed, got {position}"
        );
        assert_eq!(
            *sink.delivered.lock().unwrap(),
            vec![PlaybackEffect::Speed { speed: 1.0 }]
        );
    }

    #[test]
    fn effects_are_absolute_state_and_fold_into_the_mirror() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, true, 1.0, None);

        playback.seek_to_ms(30_000).unwrap();
        let after_seek = playback.position_ms().unwrap();
        assert!(
            (30_000..30_500).contains(&after_seek),
            "the mirror follows the absolute seek target, got {after_seek}"
        );

        playback.pause().unwrap();
        surface.backdate(Duration::from_secs(1));
        let after_pause = playback.position_ms().unwrap();
        assert!(
            (30_000..30_500).contains(&after_pause),
            "a folded pause freezes the mirror clock, got {after_pause}"
        );

        playback.resume().unwrap();
        assert_eq!(
            *sink.delivered.lock().unwrap(),
            vec![
                PlaybackEffect::Seek {
                    pos_ms: 30_000,
                    epoch: 1,
                },
                PlaybackEffect::Pause,
                PlaybackEffect::Resume,
            ],
            "each mutation delivered exactly one absolute effect, in order"
        );
    }

    #[test]
    fn a_report_that_has_seen_the_seek_is_one_the_mirror_yields_to() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);
        playback.seek_to_ms(30_000).unwrap();
        surface.report(29_500, false, 1.0, Some(1));
        assert_eq!(playback.position_ms(), Ok(29_500));
    }

    #[test]
    fn a_report_sent_before_the_seek_landed_never_undoes_it() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);
        playback.seek_to_ms(30_000).unwrap();
        surface.report(10_000, false, 1.0, None);
        assert_eq!(playback.position_ms(), Ok(30_000));
        surface.report(10_000, false, 1.0, Some(0));
        assert_eq!(playback.position_ms(), Ok(30_000));
    }

    #[test]
    fn an_unacknowledged_seek_stops_holding_reports_after_the_surface_ttl() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);
        playback.seek_to_ms(30_000).unwrap();
        surface.backdate_seek(SURFACE_TTL + Duration::from_millis(1));
        surface.report(12_000, false, 1.0, None);
        assert_eq!(playback.position_ms(), Ok(12_000));
    }

    #[test]
    fn a_held_report_still_proves_the_surface_alive() {
        let (surface, sink) = rig();
        let mut playback = WebviewPlayback::new(Arc::clone(&surface), &sink);
        surface.report(10_000, false, 1.0, None);
        playback.seek_to_ms(30_000).unwrap();
        surface.backdate(SURFACE_TTL - Duration::from_millis(100));
        surface.report(10_000, false, 1.0, None);
        surface.backdate(Duration::from_millis(200));
        assert_eq!(playback.position_ms(), Ok(30_000));
    }

    /// The wire form the webview bridge parses (`voicePlaybackBridge.ts`'s
    /// `PlaybackEffect` type): snake_case `op` tags, `pos_ms` / `speed`
    /// payload fields. This is the cross-boundary contract.
    #[test]
    fn effect_serialization_matches_the_webview_contract() {
        let cases = [
            (PlaybackEffect::Pause, r#"{"op":"pause"}"#),
            (PlaybackEffect::Resume, r#"{"op":"resume"}"#),
            (
                PlaybackEffect::Seek {
                    pos_ms: 30_000,
                    epoch: 1,
                },
                r#"{"op":"seek","pos_ms":30000,"epoch":1}"#,
            ),
            (
                PlaybackEffect::Speed { speed: 1.5 },
                r#"{"op":"speed","speed":1.5}"#,
            ),
        ];
        for (effect, wire) in cases {
            assert_eq!(serde_json::to_string(&effect).unwrap(), wire);
        }
    }
}
