//! The provider-shaped session channel (VA2, VOICE_AGENT §3).
//!
//! One trait, [`SessionChannel`], is the entire seam between the voice session
//! harness and any conversation endpoint. A conforming endpoint is just a
//! provider — a URL plus a credential: every adapter maps its wire protocol into
//! the provider-neutral types here, and nothing outside an adapter module may
//! know or branch on which provider is connected. The channel list is open
//! (VOICE_AGENT §3.1): any service implementing the versioned channel protocol
//! can register as an external provider with no special-cased code paths.
//!
//! How the trait meets the VA1 audio core (`voice::audio`):
//!
//! ```text
//! mic 16 kHz mono (UpResampler output) ──► SessionChannel::send_audio()
//! ChannelEvent::AudioDelta { pcm (24 kHz), gen } ──► TaggedPlaybackWriter::push_block(gen, pcm)
//!                                                       └─► DownResampler ─► speaker
//! barge-in: Generation::bump() ──then──► SessionChannel::interrupt()   (see [`barge_in`])
//! ```
//!
//! The two planes (VOICE_AGENT §2) map onto the trait as: `send_audio` and the
//! `AudioDelta` stream are the data plane; every other method/event is the
//! control plane, multiplexed by the adapter onto whatever its provider expects.
//!
//! Conformance: every channel adapter must pass the reusable suite in
//! `conformance` (test-gated, units 2–3 of this task), driven in CI against
//! the deterministic `mock::MockChannel` and against each adapter's recorded
//! protocol fixtures when those land (VOICE_AGENT §3, §10 — CI never touches
//! the network or real keys).

pub mod anchor;
pub mod cartesia;
pub mod composed;
pub mod deepgram;
pub mod deepgram_agent;
pub mod event;
pub mod gemini;
pub mod openai_compat;
pub mod text;
pub mod types;

#[cfg(test)]
pub mod conformance;
#[cfg(test)]
pub mod fixture;
#[cfg(test)]
pub mod mock;
#[cfg(test)]
pub mod roster;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::voice::audio::generation::Generation;

pub use anchor::{normalize_teacher_anchor, AnchorDecision, TEACHER_ANCHOR_TOOL};
pub use cartesia::CartesiaTtsSlot;
pub use composed::{
    CompatComposedChannel, CompatComposedConfig, LlmSlot, LlmSlotConfig, SttSlot, SttSlotConfig,
    TtsSlot, TtsSlotConfig,
};
pub use deepgram::{DeepgramChannel, DeepgramConfig, DeepgramSttSlot};
pub use deepgram_agent::{DeepgramAgentChannel, DeepgramAgentConfig, DeepgramAgentThinkConfig};
pub use event::{AnchorKind, AnchorRef, ChannelEvent, ProviderProgress, TimedWord};
pub use gemini::{GeminiLiveChannel, GeminiLiveConfig};
pub use openai_compat::{CompatFeature, OpenAiCompatLlmSlot, ProviderProfile};
pub use text::{TextChannel, TextChannelConfig};
pub use types::{
    resume_recap_framing, ChannelCapabilities, ChannelError, ContextUpdate, DelimitedExcerpt,
    ErrorKind, LifecycleEvent, Location, Modality, RecapOrigin, Role, SessionContext, ToolResult,
    UiLang, VadMode, RENEWAL_RECAP_FRAMING, RESUME_RECAP_FRAMING,
};

/// Version of the harness↔channel contract (VOICE_AGENT §3: the protocol between
/// harness and channels is versioned). Bump on any breaking change to the trait
/// or the event/type shapes; recorded fixtures pin the version they were cut for.
pub const CHANNEL_PROTOCOL_VERSION: u32 = 1;

/// A full-duplex voice session with one provider (VOICE_AGENT §3).
///
/// Audio contract: `send_audio` takes 16 kHz mono PCM16 — the output of the VA1
/// up-resampler seam (`voice::audio::PROVIDER_INPUT_RATE`) — and `AudioDelta`
/// events carry 24 kHz mono PCM16 into the VA1 playback seam
/// (`voice::audio::PROVIDER_OUTPUT_RATE`). Adapters own their wire
/// encoding/framing in both directions (base64-JSON vs binary frames is an
/// adapter detail the harness never sees — VOICE_AGENT §1).
///
/// State contract: methods other than `connect` fail with
/// `ErrorKind::InvalidState` before `connect` succeeds and after `close`; while
/// the channel is renewing (between `Lifecycle(Renewing)` and
/// `Lifecycle(Reconnected)`) sends fail with a `retryable: true` error and the
/// harness owns buffer-or-discard policy (VOICE_AGENT §2 rule 3, §9).
///
/// The trait is object-safe: the harness holds `Box<dyn SessionChannel>` chosen
/// at session start from the provider registry.
#[async_trait]
pub trait SessionChannel: Send {
    /// Opens the session. Emits `Lifecycle(Connected)` on the event stream once
    /// the provider accepted the session setup.
    async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError>;

    /// Ends the session cleanly. Idempotent: closing a closed channel is a no-op.
    async fn close(&mut self) -> Result<(), ChannelError>;

    /// Streams one chunk of 16 kHz mono PCM16 mic audio upstream.
    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError>;

    /// Asks the provider to finalize whatever input audio it still holds
    /// (Gemini: `realtimeInput.audioStreamEnd`, the documented fast-
    /// finalization prompt for a transcription session's open segment).
    /// Default no-op: most channels have no such frame, and finalization is
    /// only ever best-effort — callers follow it by draining events, never
    /// by trusting the ack.
    async fn finalize_input(&mut self) -> Result<(), ChannelError> {
        Ok(())
    }

    /// Sends one still image (the CURRENT PAGE's rendered pixels — F7 owner
    /// ruling 2026-08-30) into the session context. Default: refused — a
    /// channel must opt in, and callers surface the refusal honestly rather
    /// than pretend the model saw anything.
    async fn send_image(&mut self, _image: &[u8], _mime: &str) -> Result<(), ChannelError> {
        Err(ChannelError::new(
            ErrorKind::Protocol,
            false,
            "this channel cannot receive images".to_string(),
        ))
    }

    /// Text input path: no-mic fallback, accessibility, debugging (VOICE_AGENT §3).
    async fn send_text(&mut self, text: &str) -> Result<(), ChannelError>;

    /// Answers a `ChannelEvent::ToolCall` after local execution (VOICE_AGENT §5).
    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError>;

    /// Chapter-boundary instruction refresh / playback-position update
    /// (VOICE_AGENT §4: never a blind mid-turn re-prompt).
    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError>;

    /// The network half of barge-in: asks the provider to cancel the in-flight
    /// response. The local half — the generation bump that makes the stop feel
    /// instant — must already have happened; use [`barge_in`] to get the order
    /// right (VOICE_AGENT §2 rule 2).
    async fn interrupt(&mut self) -> Result<(), ChannelError>;

    /// Hands over the owned event receiver. Yields `Some` exactly once, at
    /// session start; every later call returns `None`. An owned receiver (not a
    /// borrowed stream) is what lets the control plane consume events while
    /// `send_audio`/`send_tool_result`/`interrupt` keep using the channel, and
    /// keeps the trait object-safe (VOICE_AGENT §3). The receiver survives
    /// renewal-recycle: one receiver spans the whole session.
    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>>;

    /// What this session can do (VOICE_AGENT §3). Stable for the lifetime of the
    /// session; the harness degrades from these flags, never from provider
    /// identity.
    fn capabilities(&self) -> ChannelCapabilities;

    /// Whether the provider retracted this tool call before it was answered
    /// (a Live `toolCallCancellation`). The harness checks it before
    /// EXECUTING a queued call: a retracted `jump_to_page` must not move the
    /// reader (review 2026-10-09, L7). Wires without retraction never do.
    fn tool_call_retracted(&self, _call_id: &str) -> bool {
        false
    }
}

/// Barge-in, in the spec-mandated order (VOICE_AGENT §2 rule 2): bump the shared
/// generation counter first — from this instant the playback gate
/// (`voice::audio::generation`) refuses and discards the superseded response —
/// then send the provider's async cancel, which only stops future deltas.
/// Returns the new live generation, the id the next response's payloads must
/// carry. The audio-side cleanup between the two actions (jitter `flush`,
/// down-resampler `reset`) is owned by the session harness, which holds those
/// seams; adapters and this helper never touch them.
pub async fn barge_in<C: SessionChannel + ?Sized>(
    generation: &Generation,
    channel: &mut C,
) -> Result<u64, ChannelError> {
    let live = generation.bump();
    channel.interrupt().await?;
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn object_safety_holds(_: &mut dyn SessionChannel) {}

    struct OrderProbe {
        generation: Generation,
        interrupt_saw_gen: Arc<Mutex<Option<u64>>>,
        events: Option<mpsc::Receiver<ChannelEvent>>,
    }

    #[async_trait]
    impl SessionChannel for OrderProbe {
        async fn connect(&mut self, _ctx: SessionContext) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn close(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn send_audio(&mut self, _pcm: &[i16]) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn send_text(&mut self, _text: &str) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn send_tool_result(
            &mut self,
            _call_id: &str,
            _result: ToolResult,
        ) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn update_context(&mut self, _update: ContextUpdate) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn interrupt(&mut self) -> Result<(), ChannelError> {
            *self.interrupt_saw_gen.lock().unwrap() = Some(self.generation.current());
            Ok(())
        }
        fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
            self.events.take()
        }
        fn capabilities(&self) -> ChannelCapabilities {
            ChannelCapabilities {
                vad_mode: VadMode::Server,
                word_timestamps: false,
                session_renewal: false,
                text_only: false,
                stt_only: false,
            }
        }
    }

    #[tokio::test]
    async fn barge_in_bumps_the_generation_before_the_provider_cancel_goes_out() {
        let generation = Generation::new();
        let saw = Arc::new(Mutex::new(None));
        let mut probe = OrderProbe {
            generation: generation.clone(),
            interrupt_saw_gen: Arc::clone(&saw),
            events: None,
        };
        object_safety_holds(&mut probe);

        let live = barge_in(&generation, &mut probe).await.unwrap();

        assert_eq!(live, 1, "barge_in returns the new live generation");
        assert_eq!(generation.current(), 1);
        assert_eq!(
            *saw.lock().unwrap(),
            Some(1),
            "the provider cancel must observe the already-bumped generation: the \
             local stop precedes the network round-trip"
        );
    }

    #[tokio::test]
    async fn barge_in_through_a_trait_object_still_bumps_even_if_the_cancel_fails() {
        struct FailingCancel;
        #[async_trait]
        impl SessionChannel for FailingCancel {
            async fn connect(&mut self, _ctx: SessionContext) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn close(&mut self) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn send_audio(&mut self, _pcm: &[i16]) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn send_text(&mut self, _text: &str) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn send_tool_result(
                &mut self,
                _call_id: &str,
                _result: ToolResult,
            ) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn update_context(&mut self, _update: ContextUpdate) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn interrupt(&mut self) -> Result<(), ChannelError> {
                Err(ChannelError::new(ErrorKind::Transport, true, "ws drop"))
            }
            fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
                None
            }
            fn capabilities(&self) -> ChannelCapabilities {
                ChannelCapabilities {
                    vad_mode: VadMode::Server,
                    word_timestamps: false,
                    session_renewal: false,
                    text_only: false,
                    stt_only: false,
                }
            }
        }

        let generation = Generation::new();
        let mut boxed: Box<dyn SessionChannel> = Box::new(FailingCancel);
        let result = barge_in(&generation, boxed.as_mut()).await;

        assert!(result.is_err(), "the transport failure is surfaced");
        assert_eq!(
            generation.current(),
            1,
            "the local stop already happened: a failed provider cancel must never \
             resurrect the superseded response"
        );
    }
}
