//! The control-plane event stream every channel emits (VOICE_AGENT §2, §3).
//!
//! One enum carries everything the session harness consumes: decoded response
//! audio, transcript deltas, tool calls, lifecycle signals, and errors. The enum is
//! `#[non_exhaustive]` because the surface is expected to grow (VOICE_AGENT §3
//! "field lists may grow"); consumers must tolerate variants they do not know.
//!
//! Generation tagging: response-scoped payloads (`AudioDelta`, `WordTimestamps`,
//! `TeacherAnchor`) carry the `gen` of the response that produced them. After a
//! barge-in bump (VOICE_AGENT §2 rule 2, `voice::audio::generation`), payloads
//! from a superseded generation are dropped at their respective gates and never
//! reach the speaker or the UI.

use super::types::{ChannelError, LifecycleEvent, Role};

/// One word with its timing offsets into the response audio stream
/// (VOICE_AGENT §3, §11 word-level follow-along).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedWord {
    pub w: String,
    pub start_ms: u32,
    pub end_ms: u32,
}

/// What a live tutor anchor points at (VOICE_AGENT §11.1). These are the only
/// anchor kinds; the arg validation in [`super::anchor`] rejects everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    Section,
    Figure,
    Equation,
    Table,
}

impl AnchorKind {
    /// Parses the provider-neutral `teacher_anchor` tool-call `kind` argument
    /// (VOICE_AGENT §5 table: kind ∈ {section, figure, equation, table}).
    pub fn from_tool_arg(kind: &str) -> Option<Self> {
        match kind {
            "section" => Some(Self::Section),
            "figure" => Some(Self::Figure),
            "equation" => Some(Self::Equation),
            "table" => Some(Self::Table),
            _ => None,
        }
    }

    pub fn as_tool_arg(&self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Figure => "figure",
            Self::Equation => "equation",
            Self::Table => "table",
        }
    }
}

/// A resolved live-tutor anchor (VOICE_AGENT §11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorRef {
    pub kind: AnchorKind,
    pub id: String,
}

/// Why an owed reply is taking longer than usual ([`ChannelEvent::Progress`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderProgress {
    /// The model was busy (503 / transport); `attempt` is the retry about
    /// to go out on the same model, 1-based.
    Retrying { attempt: u32 },
    /// The model stayed busy through its retry budget; the next model in
    /// the fallback chain answers (pinned ids, `models/` prefix stripped).
    FallbackModel { model: String },
}

/// Everything a channel can tell the session harness (VOICE_AGENT §3).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelEvent {
    /// Response audio decoded to 24 kHz mono PCM16 by the channel adapter
    /// (VOICE_AGENT §3.1 output rate; `voice::audio::PROVIDER_OUTPUT_RATE`).
    /// `gen` is the response generation id the barge-in gate keys on
    /// (`voice::audio::generation::TaggedPlaybackWriter::push_block`).
    AudioDelta { pcm: Vec<i16>, gen: u64 },
    /// Incremental transcript text in arrival order (VOICE_AGENT §3, §11).
    TranscriptDelta { role: Role, text: String },
    /// Word-aligned timing, only on channels whose capabilities advertise
    /// `word_timestamps`; everything else degrades to sentence-level (§11).
    WordTimestamps { words: Vec<TimedWord>, gen: u64 },
    /// A transcription-mode stream is HEARING the current utterance: emitted
    /// once per finalization cycle, on the first interim hypothesis
    /// (`serverContent.interimInputTranscription` — "emitted continuously
    /// while the user is actively speaking",
    /// ai.google.dev/gemini-api/docs/live-api/live-transcribe, checked
    /// 2026-08-30). Kritias round 4: `cycle` is the adapter's MONOTONIC
    /// finalization-cycle index — the wire is one serial WebSocket read
    /// loop, so cycle N's activity always precedes cycle N's final, which
    /// precedes cycle N+1's activity, and the events channel preserves that
    /// order. The adapter guarantees every user final is preceded by its
    /// cycle's activity (a final whose cycle produced no interim gets the
    /// announcement immediately before it), so the fan-out ledger settles
    /// finals by CYCLE IDENTITY, never by arrival order or age. Only
    /// transcribe-mode adapters emit it.
    TranscriptActivity { cycle: u64 },
    /// The channel's authoritative endpointing committed the user's utterance
    /// (VOICE_AGENT §3.2 turn-detection ownership: only the VAD owner signals
    /// turns, so only `VadMode::Server` channels emit this). Exactly one per
    /// utterance, however many wire signals the provider fires for the same
    /// boundary — the adapter owns that dedupe (Deepgram `speech_final` vs
    /// `UtteranceEnd`: the `endpoint-race-dedupe` fixture, see
    /// `deepgram::read_loop`). A wire with no utterance-end vocabulary maps
    /// the obligation to its own commit signal — Gemini Live emits this at
    /// each fresh response-turn open (issue #201, see `gemini::read_loop`) —
    /// so *when* the boundary lands relative to response content is
    /// per-adapter; consumers pinning content order must tolerate it
    /// in-between. The harness tracks the last committed end-of-utterance as
    /// the mic-ring replay watermark (VOICE_AGENT §3), and the event pump
    /// re-arms its `response_gen` snapshot on it (§11.1 staleness).
    UtteranceEnd,
    /// The model invoked a local tool (VOICE_AGENT §5). The harness validates,
    /// executes locally, and answers with `SessionChannel::send_tool_result`.
    ToolCall {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// The assistant's streamed reply is complete: no more deltas follow and
    /// no tool call is pending (a turn that ends owing tool answers emits
    /// this only after the answered continuation finishes). Composition-
    /// internal — the vocabulary growth the openai_compat/cartesia headers
    /// deferred to the VA5 TTS-wiring stride (cc-1e w2): an `LlmSlot` emits
    /// it on its slot stream as the composed loop's TTS flush trigger
    /// ([`super::composed::TtsSlot::speak`] takes one complete turn's text),
    /// and the loop consumes it — it never rides a harness-facing channel
    /// stream, mirroring how gemini.rs consumes `turnComplete` internally.
    TurnComplete,
    /// Live-tutor presentation sync (VOICE_AGENT §11.1). Never emitted by
    /// adapters directly: the harness normalizes the provider-neutral
    /// `teacher_anchor` tool call into this event ([`super::anchor`]), so it is
    /// identical across providers. `gen` mirrors `AudioDelta`: anchors from a
    /// superseded response are dropped, never rendered.
    TeacherAnchor { anchor: AnchorRef, gen: u64 },
    /// Session-lifetime signal (VOICE_AGENT §3, §9), surfaced uniformly.
    Lifecycle(LifecycleEvent),
    /// A classified failure (VOICE_AGENT §9 degradation matrix).
    Error(ChannelError),
    /// The provider is still working on an owed reply, and the user should
    /// know why it is slow (field session 2026-10-09: a minute of silence
    /// read as a dead tutor). Display-only: nothing is journaled and no
    /// turn state changes. Only the text channel emits it today.
    Progress(ProviderProgress),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_kind_round_trips_through_the_tool_arg_vocabulary() {
        for kind in [
            AnchorKind::Section,
            AnchorKind::Figure,
            AnchorKind::Equation,
            AnchorKind::Table,
        ] {
            assert_eq!(AnchorKind::from_tool_arg(kind.as_tool_arg()), Some(kind));
        }
    }

    #[test]
    fn anchor_kind_rejects_everything_outside_the_spec_vocabulary() {
        for bad in ["", "Section", "FIGURE", "paragraph", "page", "chapter"] {
            assert_eq!(
                AnchorKind::from_tool_arg(bad),
                None,
                "kind vocabulary is closed: {bad:?} must not parse"
            );
        }
    }
}
