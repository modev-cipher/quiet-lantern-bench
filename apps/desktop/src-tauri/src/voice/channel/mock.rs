//! Deterministic scripted channel for the conformance suite (VA2 unit 2).
//!
//! [`MockChannel`] is a full [`SessionChannel`] whose provider side is a test
//! handle, [`MockDriver`]: the suite injects events (transcript deltas, tool
//! calls, audio blocks, lifecycle signals including a mid-session end-of-session
//! announcement, error shapes) at exact points, and reads back everything the
//! channel received, in order. Nothing here spawns tasks, sleeps, or touches the
//! network — every run is bit-identical, which is what lets CI stand in for a
//! provider (VOICE_AGENT §10: CI gates are mock/fixture suites exclusively).
//!
//! Seedability: [`SeededPcm`] derives audio payloads from a fixture seed with an
//! in-house splitmix64 (no external RNG, so payloads never shift under a
//! dependency upgrade). Payload-pinned assertions in the conformance suite
//! compare these blocks sample-for-sample across the whole path.
//!
//! State model (the uniform contract the trait documents): sends before
//! `connect`, after `close`, or after `Expired` fail `InvalidState`; between the
//! injected end-of-session announcement (`Lifecycle(Renewing)`) and
//! [`MockDriver::complete_renewal`] (`Lifecycle(Reconnected)`) sends fail
//! `SessionExpired { retryable: true }` and reach the provider record not at
//! all. Each completed renewal advances an epoch counter; received audio is
//! recorded per epoch so exactly-once/no-replay is assertable.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::types::{
    ChannelCapabilities, ChannelError, ContextUpdate, ErrorKind, LifecycleEvent, SessionContext,
    ToolResult, VadMode,
};
use super::{ChannelEvent, SessionChannel};
use crate::voice::audio::generation::Generation;

const EVENT_CAPACITY: usize = 4096;

/// Deterministic PCM payload source: splitmix64 folded to i16 samples.
pub struct SeededPcm {
    state: u64,
}

impl SeededPcm {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// One block of `frames` deterministic samples.
    pub fn block(&mut self, frames: usize) -> Vec<i16> {
        (0..frames).map(|_| self.next_u64() as i16).collect()
    }
}

/// Everything the mock provider observed, in arrival order.
#[derive(Debug, Clone, PartialEq)]
pub enum MockOp {
    Connect(SessionContext),
    Close,
    Audio {
        epoch: u64,
        pcm: Vec<i16>,
    },
    Text {
        epoch: u64,
        text: String,
    },
    ToolResult {
        call_id: String,
        result: ToolResult,
    },
    ContextUpdate(ContextUpdate),
    /// A `finalize_input` (Gemini: `audioStreamEnd`) the channel accepted —
    /// the boundary-export pins assert its count and its ordering against
    /// the audio ops around it.
    AudioStreamEnd {
        epoch: u64,
    },
    /// `live_gen` snapshots the shared barge-in counter at the instant the
    /// cancel reached the provider — `Some` only when the channel was built
    /// with [`MockChannelBuilder::observe_generation`].
    Interrupt {
        live_gen: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    New,
    Connected,
    Renewing,
    Expired,
    Closed,
}

struct Shared {
    state: State,
    epoch: u64,
    ops: Vec<MockOp>,
    fail_next_send: Option<ChannelError>,
    tx: Option<mpsc::Sender<ChannelEvent>>,
    /// Call ids the scripted provider retracted (`toolCallCancellation`).
    retracted: Vec<String>,
}

impl Shared {
    fn emit(&mut self, event: ChannelEvent) {
        let tx = self
            .tx
            .as_ref()
            .expect("mock provider emitted into a channel that was never opened or already closed");
        tx.try_send(event)
            .expect("mock event capacity exceeded: a conformance scenario emitted > 4096 events");
    }

    fn send_gate(&mut self, what: &str) -> Result<(), ChannelError> {
        if let Some(err) = self.fail_next_send.take() {
            return Err(err);
        }
        match self.state {
            State::Connected => Ok(()),
            State::Renewing => Err(ChannelError::new(
                ErrorKind::SessionExpired,
                true,
                format!("{what} while the session is renewing"),
            )),
            State::New | State::Expired | State::Closed => Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("{what} on a channel in state {:?}", self.state),
            )),
        }
    }
}

/// The provider side of a [`MockChannel`]: injects events, reads the record.
#[derive(Clone)]
pub struct MockDriver {
    shared: Arc<Mutex<Shared>>,
}

impl MockDriver {
    /// Injects one raw event. Panics if the channel is not open — a scenario
    /// bug, not a runtime condition.
    pub fn emit(&self, event: ChannelEvent) {
        self.shared.lock().unwrap().emit(event);
    }

    pub fn emit_audio(&self, gen: u64, pcm: Vec<i16>) {
        self.emit(ChannelEvent::AudioDelta { pcm, gen });
    }

    pub fn emit_error(&self, error: ChannelError) {
        self.emit(ChannelEvent::Error(error));
    }

    /// The provider announces the session is ending (the uniform surface for a
    /// provider session cap — VOICE_AGENT §3, §9): emits `Lifecycle(Renewing)`
    /// and rejects sends until [`Self::complete_renewal`].
    pub fn announce_session_end(&self) {
        let mut shared = self.shared.lock().unwrap();
        assert_eq!(
            shared.state,
            State::Connected,
            "a session-end announcement needs a connected session"
        );
        shared.state = State::Renewing;
        shared.emit(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
            time_left: None,
        }));
    }

    /// The adapter's recycle succeeded: emits `Lifecycle(Reconnected)` on the
    /// same receiver and opens the next epoch.
    pub fn complete_renewal(&self) {
        self.complete_renewal_with(false);
    }

    /// The resumption shape (owner-approved 2026-09-24): a recycle whose
    /// redial RESUMED the server's session state — the harness must skip
    /// the renewal recap re-inject (the server already holds its history).
    pub fn complete_renewal_resumed(&self) {
        self.complete_renewal_with(true);
    }

    fn complete_renewal_with(&self, resumed: bool) {
        let mut shared = self.shared.lock().unwrap();
        assert_eq!(
            shared.state,
            State::Renewing,
            "complete_renewal without a pending renewal"
        );
        shared.state = State::Connected;
        shared.epoch += 1;
        shared.emit(ChannelEvent::Lifecycle(LifecycleEvent::Reconnected {
            resumed,
        }));
    }

    /// The session ended without recovery: emits `Lifecycle(Expired)`; every
    /// later send fails `InvalidState`.
    pub fn expire(&self) {
        let mut shared = self.shared.lock().unwrap();
        shared.state = State::Expired;
        shared.emit(ChannelEvent::Lifecycle(LifecycleEvent::Expired));
    }

    /// The provider retracts a call it issued (a Live
    /// `toolCallCancellation`): the channel reports it retracted.
    pub fn retract_tool_call(&self, call_id: &str) {
        self.shared
            .lock()
            .unwrap()
            .retracted
            .push(call_id.to_string());
    }

    /// Arms a one-shot failure returned by the next send-side call.
    pub fn fail_next_send(&self, error: ChannelError) {
        self.shared.lock().unwrap().fail_next_send = Some(error);
    }

    /// Everything the provider received so far, in order.
    pub fn ops(&self) -> Vec<MockOp> {
        self.shared.lock().unwrap().ops.clone()
    }

    /// Received audio payloads grouped by renewal epoch: `result[e]` holds the
    /// blocks accepted while the session was in epoch `e`, in order.
    pub fn audio_by_epoch(&self) -> Vec<Vec<Vec<i16>>> {
        let shared = self.shared.lock().unwrap();
        let mut by_epoch: Vec<Vec<Vec<i16>>> = vec![Vec::new(); shared.epoch as usize + 1];
        for op in &shared.ops {
            if let MockOp::Audio { epoch, pcm } = op {
                by_epoch[*epoch as usize].push(pcm.clone());
            }
        }
        by_epoch
    }

    pub fn tool_results(&self) -> Vec<(String, ToolResult)> {
        self.shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter_map(|op| match op {
                MockOp::ToolResult { call_id, result } => Some((call_id.clone(), result.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn interrupts(&self) -> Vec<Option<u64>> {
        self.shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter_map(|op| match op {
                MockOp::Interrupt { live_gen } => Some(*live_gen),
                _ => None,
            })
            .collect()
    }

    pub fn current_epoch(&self) -> u64 {
        self.shared.lock().unwrap().epoch
    }

    /// How many `finalize_input` calls the provider accepted so far.
    pub fn audio_stream_ends(&self) -> usize {
        self.shared
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|op| matches!(op, MockOp::AudioStreamEnd { .. }))
            .count()
    }
}

pub struct MockChannelBuilder {
    capabilities: ChannelCapabilities,
    observe: Option<Generation>,
    refuse_connect: Option<ChannelError>,
}

impl MockChannelBuilder {
    pub fn capabilities(mut self, capabilities: ChannelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Snapshot this shared counter into every recorded interrupt, so ordering
    /// between the local generation bump and the provider cancel is assertable.
    pub fn observe_generation(mut self, generation: Generation) -> Self {
        self.observe = Some(generation);
        self
    }

    /// Script a refused dial: `connect` fails with `error` and the channel
    /// stays `New` (the start-path phantom-row pin).
    pub fn refuse_connect(mut self, error: ChannelError) -> Self {
        self.refuse_connect = Some(error);
        self
    }

    pub fn build(self) -> (MockChannel, MockDriver) {
        let (tx, rx) = mpsc::channel(EVENT_CAPACITY);
        let shared = Arc::new(Mutex::new(Shared {
            state: State::New,
            epoch: 0,
            ops: Vec::new(),
            fail_next_send: None,
            tx: Some(tx),
            retracted: Vec::new(),
        }));
        (
            MockChannel {
                shared: Arc::clone(&shared),
                capabilities: self.capabilities,
                observe: self.observe,
                refuse_connect: self.refuse_connect,
                events: Some(rx),
            },
            MockDriver { shared },
        )
    }
}

/// The deterministic in-memory [`SessionChannel`] the conformance suite runs
/// against in CI.
pub struct MockChannel {
    shared: Arc<Mutex<Shared>>,
    capabilities: ChannelCapabilities,
    observe: Option<Generation>,
    refuse_connect: Option<ChannelError>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
}

impl MockChannel {
    pub fn builder() -> MockChannelBuilder {
        MockChannelBuilder {
            capabilities: ChannelCapabilities {
                vad_mode: VadMode::Server,
                word_timestamps: false,
                // The mock driver scripts the full renewal contract
                // (announce_session_end / complete_renewal / expire).
                session_renewal: true,
                text_only: false,
                stt_only: false,
            },
            observe: None,
            refuse_connect: None,
        }
    }
}

#[async_trait]
impl SessionChannel for MockChannel {
    async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError> {
        if let Some(error) = self.refuse_connect.take() {
            return Err(error);
        }
        let mut shared = self.shared.lock().unwrap();
        if shared.state != State::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a channel in state {:?}", shared.state),
            ));
        }
        shared.ops.push(MockOp::Connect(ctx));
        shared.state = State::Connected;
        shared.emit(ChannelEvent::Lifecycle(LifecycleEvent::Connected));
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        if shared.state == State::Closed {
            return Ok(());
        }
        shared.ops.push(MockOp::Close);
        shared.state = State::Closed;
        shared.tx = None;
        Ok(())
    }

    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        shared.send_gate("send_audio")?;
        let epoch = shared.epoch;
        shared.ops.push(MockOp::Audio {
            epoch,
            pcm: pcm_16k_mono.to_vec(),
        });
        Ok(())
    }

    async fn finalize_input(&mut self) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        shared.send_gate("finalize_input")?;
        let epoch = shared.epoch;
        shared.ops.push(MockOp::AudioStreamEnd { epoch });
        Ok(())
    }

    async fn send_text(&mut self, text: &str) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        shared.send_gate("send_text")?;
        let epoch = shared.epoch;
        shared.ops.push(MockOp::Text {
            epoch,
            text: text.to_string(),
        });
        Ok(())
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        shared.send_gate("send_tool_result")?;
        shared.ops.push(MockOp::ToolResult {
            call_id: call_id.to_string(),
            result,
        });
        Ok(())
    }

    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        shared.send_gate("update_context")?;
        shared.ops.push(MockOp::ContextUpdate(update));
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), ChannelError> {
        let mut shared = self.shared.lock().unwrap();
        shared.send_gate("interrupt")?;
        let live_gen = self.observe.as_ref().map(Generation::current);
        shared.ops.push(MockOp::Interrupt { live_gen });
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }

    fn tool_call_retracted(&self, call_id: &str) -> bool {
        self.shared
            .lock()
            .unwrap()
            .retracted
            .iter()
            .any(|id| id == call_id)
    }

    fn capabilities(&self) -> ChannelCapabilities {
        self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::channel::types::{DelimitedExcerpt, Role};

    fn ctx() -> SessionContext {
        SessionContext {
            book_id: Some("book-1".to_string()),
            session_id: "session-1".to_string(),
            system_prompt: "tutor".to_string(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<s>excerpt</s>".to_string(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: Vec::new(),
        }
    }

    #[test]
    fn seeded_pcm_is_deterministic_and_seed_sensitive() {
        let a1 = SeededPcm::new(7).block(32);
        let a2 = SeededPcm::new(7).block(32);
        let b = SeededPcm::new(8).block(32);
        assert_eq!(a1, a2, "same seed, same payload, every run");
        assert_ne!(a1, b, "different seed, different payload");
        assert_eq!(a1.len(), 32);
    }

    #[tokio::test]
    async fn sends_are_recorded_in_order_with_their_epoch() {
        let (mut channel, driver) = MockChannel::builder().build();
        channel.connect(ctx()).await.unwrap();
        channel.send_audio(&[1, 2, 3]).await.unwrap();
        channel.send_text("hello").await.unwrap();
        channel
            .send_tool_result("call-1", ToolResult::Ok(serde_json::json!({})))
            .await
            .unwrap();

        let ops = driver.ops();
        assert!(matches!(ops[0], MockOp::Connect(_)));
        assert_eq!(
            ops[1],
            MockOp::Audio {
                epoch: 0,
                pcm: vec![1, 2, 3]
            }
        );
        assert_eq!(
            ops[2],
            MockOp::Text {
                epoch: 0,
                text: "hello".to_string()
            }
        );
        assert!(matches!(ops[3], MockOp::ToolResult { .. }));
    }

    #[tokio::test]
    async fn lifecycle_and_scripted_events_arrive_on_the_receiver_in_emission_order() {
        let (mut channel, driver) = MockChannel::builder().build();
        let mut events = channel
            .take_events()
            .expect("first take yields the receiver");
        channel.connect(ctx()).await.unwrap();
        driver.emit(ChannelEvent::TranscriptDelta {
            role: Role::Assistant,
            text: "hi".to_string(),
        });
        driver.emit_audio(0, vec![9, 9]);

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "hi".to_string()
            })
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![9, 9],
                gen: 0
            })
        );
    }

    #[tokio::test]
    async fn take_events_yields_exactly_once() {
        let (mut channel, _driver) = MockChannel::builder().build();
        assert!(channel.take_events().is_some());
        assert!(channel.take_events().is_none());
    }

    #[tokio::test]
    async fn sends_fail_invalid_state_before_connect_and_after_close() {
        let (mut channel, _driver) = MockChannel::builder().build();
        let before = channel.send_audio(&[1]).await.unwrap_err();
        assert_eq!(before.kind, ErrorKind::InvalidState);
        assert!(!before.retryable);

        channel.connect(ctx()).await.unwrap();
        channel.close().await.unwrap();
        channel.close().await.unwrap();
        let after = channel.send_text("x").await.unwrap_err();
        assert_eq!(after.kind, ErrorKind::InvalidState);
    }

    #[tokio::test]
    async fn close_ends_the_event_stream() {
        let (mut channel, _driver) = MockChannel::builder().build();
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        channel.close().await.unwrap();
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(events.recv().await, None, "sender dropped at close");
    }

    #[tokio::test]
    async fn renewal_rejects_sends_retryably_and_epochs_separate_the_audio() {
        let (mut channel, driver) = MockChannel::builder().build();
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        channel.send_audio(&[10, 10]).await.unwrap();

        driver.announce_session_end();
        let rejected = channel.send_audio(&[20, 20]).await.unwrap_err();
        assert_eq!(rejected.kind, ErrorKind::SessionExpired);
        assert!(rejected.retryable, "renewal is a wait-and-retry condition");

        driver.complete_renewal();
        channel.send_audio(&[30, 30]).await.unwrap();

        let by_epoch = driver.audio_by_epoch();
        assert_eq!(by_epoch.len(), 2);
        assert_eq!(by_epoch[0], vec![vec![10, 10]]);
        assert_eq!(
            by_epoch[1],
            vec![vec![30, 30]],
            "the renewal-rejected payload must appear in no epoch"
        );

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                time_left: None
            }))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Reconnected {
                resumed: false,
            }))
        );
    }

    #[tokio::test]
    async fn expiry_is_terminal_for_sends() {
        let (mut channel, driver) = MockChannel::builder().build();
        channel.connect(ctx()).await.unwrap();
        driver.expire();
        let err = channel.send_audio(&[1]).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidState);
        assert!(!err.retryable);
    }

    #[tokio::test]
    async fn interrupt_snapshots_the_observed_generation() {
        let generation = Generation::new();
        let (mut channel, driver) = MockChannel::builder()
            .observe_generation(generation.clone())
            .build();
        channel.connect(ctx()).await.unwrap();
        generation.bump();
        channel.interrupt().await.unwrap();
        assert_eq!(driver.interrupts(), vec![Some(1)]);
    }

    #[tokio::test]
    async fn an_armed_send_failure_fires_once_and_reaches_no_record() {
        let (mut channel, driver) = MockChannel::builder().build();
        channel.connect(ctx()).await.unwrap();
        driver.fail_next_send(ChannelError::new(ErrorKind::RateLimited, true, "429"));

        let err = channel.send_audio(&[5, 5]).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::RateLimited);
        channel.send_audio(&[6, 6]).await.unwrap();

        let audio = driver.audio_by_epoch();
        assert_eq!(
            audio[0],
            vec![vec![6, 6]],
            "the failed send must not be half-recorded"
        );
    }

    #[tokio::test]
    async fn scripted_error_events_pass_through_shape_intact() {
        let (mut channel, driver) = MockChannel::builder().build();
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let shape = ChannelError::new(ErrorKind::Provider, true, "upstream 503");
        driver.emit_error(shape.clone());

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(events.recv().await, Some(ChannelEvent::Error(shape)));
    }

    #[tokio::test]
    async fn a_second_connect_is_refused() {
        let (mut channel, _driver) = MockChannel::builder().build();
        channel.connect(ctx()).await.unwrap();
        let err = channel.connect(ctx()).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidState);
    }
}
