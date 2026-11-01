//! MODEL PINS — the one place a live provider model id may be pinned.
//!
//! OWNER RULING (2026-08-29, F4 feedback round): model pins are a VISIBLE,
//! OWNER-TUNABLE surface. A pinned model id buried in a module constant went
//! stale unnoticed; that must never repeat. Every pin in this module carries:
//!
//! - `PINNED:`     the date the id was chosen
//! - `DOCS:`       the doc URLs the choice was verified against + check date
//! - `WHY:`        the capability facts that forced this exact id
//! - `REVIEW-BY:`  the date the pin goes stale. `grep -rn "REVIEW-BY:"` over
//!   this file is the staleness pulse; a pin past its REVIEW-BY must be
//!   re-verified against https://ai.google.dev/gemini-api/docs/models before
//!   any campaign builds on it.
//!
//! No other module may hard-code a live model id outside `#[cfg(test)]`
//! fixtures; new pins land HERE with the full convention block.

/// The tutor's Gemini Live model.
///
/// PINNED: 2026-09-24 (migrated from the dated 2.5 native-audio preview).
/// DOCS: https://ai.google.dev/gemini-api/docs/models/gemini-3.8-live
/// (migration guide, checked 2026-09-24),
/// https://ai.google.dev/gemini-api/docs/live-guide (checked 2026-09-24),
/// https://ai.google.dev/api/live (checked 2026-09-24).
/// WHY: `models/gemini-3.8-live` — stable ("New Stable", not preview),
/// native-audio, 131,072-token input window, free tier exists (input/
/// output free of charge; paid $0.75/$3.75 per 1M through 2026-12-31),
/// interleaved reasoning built in (`thinkingLevel`/`thinking_config` is
/// UNSUPPORTED and must be omitted), and — the fact that unblocks this
/// migration — `send_client_content` is documented "supported throughout
/// the entire session lifecycle with explicit roles" on 3.8, reversing
/// the 3.1-era restriction ("after the first model turn, use
/// `send_realtime_input` instead") that kept the tutor pinned to 2.5: the
/// tutor's four mid-session `clientContent` paths (typed sends,
/// chapter-boundary excerpt refresh, the JIT code-switch fragment, the
/// renewal recap) are all legal on this wire, and `turnComplete: true`
/// unconditionally interrupts generation — matching the runtime's
/// bump-on-SendText flush (R4-2). Caveats pinned in channel::gemini:
/// async function calling is the 3.8 DEFAULT (every tutor declaration
/// pins `behavior: BLOCKING`); `false` proactivity errors and is never
/// sent; `TURN_INCLUDES_ALL_INPUT` remains a valid enum.
/// REVIEW-BY: 2026-12-24
pub const TUTOR_LIVE_MODEL: &str = "models/gemini-3.8-live";

/// The Book agent's Gemini Live model (charter §6.1; agent_session.rs docs
/// carry the behavioral landmines around it).
///
/// PINNED: 2026-08-29 (re-verified; originally chosen 2026-08-29 in B-2a).
/// DOCS: https://ai.google.dev/gemini-api/docs/live-guide, checked
/// 2026-08-29.
/// WHY: the agent's tool plane REQUIRES async function calling — all eight
/// agent schemas declare `behavior: NON_BLOCKING` (B-2b) — and the live
/// guide states for `gemini-3.1-flash-live-preview`: "Not supported.
/// Function calling is sequential only." The 2.5 native-audio family
/// remains the only Live family with NON_BLOCKING; the agent CANNOT move
/// until a newer Live model documents it. VA3 rotation-ring note: any
/// future candidate must also re-verify the charter's other wire
/// requirements (scheduling parameter, mid-session `clientContent`).
/// UPDATE 2026-09-24: `gemini-3.8-live` documents NON_BLOCKING (now the
/// default) + function scheduling — the agent's blocker is RESOLVABLE, but
/// the agent channel keeps its exact 2.5 wire contract until its own
/// migration task lands (tutor-first, one channel per change; see
/// [`TUTOR_LIVE_MODEL`]).
/// REVIEW-BY: 2026-12-24
pub const AGENT_LIVE_MODEL: &str = "models/gemini-2.5-flash-native-audio-preview-12-2025";

/// The Ask Tutor text channel's model (F11 §0 R1, §11.2).
///
/// PINNED: 2026-09-03.
/// DOCS: https://ai.google.dev/gemini-api/docs/models,
/// https://ai.google.dev/gemini-api/docs/pricing, https://ai.google.dev/api/generate-content
/// — read 2026-09-02 (spec §11.2 matrix) and 2026-09-03 (REST reference).
/// WHY: the documented stable Flash text model: text-only output, function
/// calling supported, free of charge for Standard requests, 1,048,576-token
/// context; the Live API is unavailable for it, which is exactly the point —
/// Ask billing is text billing (ruling R1). `gemini-2.5-flash` is the
/// lower-cost alternative; `GEMINI_TEXT_MODEL` overrides for the smoke rig.
/// RE-VERIFIED 2026-10-09 against https://ai.google.dev/gemini-api/docs/models
/// (updated 2026-10-06: listed Stable) and
/// https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash (updated
/// 2026-09-02: text output only, function calling supported, Live API not
/// supported, 1,048,576-token input).
/// REVIEW-BY: 2026-12-31
pub const TUTOR_TEXT_MODEL: &str = "models/gemini-3.8-flash";

/// The Ask Tutor's fallback chain (owner ruling 2026-10-09, after field
/// 503s on [`TUTOR_TEXT_MODEL`]): when the primary stays overloaded through
/// its retry budget, the turn is answered by the next model here, in order.
///
/// PINNED: 2026-10-09.
/// DOCS: https://ai.google.dev/gemini-api/docs/models (updated 2026-10-06,
/// all three listed Stable), and each model page, read 2026-10-09:
/// .../models/gemini-3.7-flash (updated 2026-08-13), .../gemini-3.6-flash
/// (updated 2026-07-30), .../gemini-3.5-flash (updated 2026-07-21).
/// WHY: the stable Flash line below the primary — every one text output,
/// function calling supported, Live API not supported, 1,048,576-token
/// input — so a fallback answers the same request shape with the same
/// tools. Thought signatures from another model are resent as received:
/// "When switching models within a session, you should still resend the
/// previous model's thought blocks … The backend manages compatibility"
/// (https://ai.google.dev/gemini-api/docs/thinking, updated 2026-09-25).
/// REVIEW-BY: 2026-12-31
pub const TUTOR_TEXT_FALLBACK_MODELS: [&str; 3] = [
    "models/gemini-3.7-flash",
    "models/gemini-3.6-flash",
    "models/gemini-3.5-flash",
];

/// The live sessions' thinking budget — OWNER RULING (2026-08-29, ratified):
/// thinking effort is LOW by default everywhere, and for the LIVE VOICE
/// LOOP it is minimal/OFF — deliberation is audible silence in a
/// conversation.
///
/// PINNED: 2026-08-29.
/// DOCS: https://ai.google.dev/gemini-api/docs/live-guide, checked
/// 2026-08-29 — the 2.5 Live family "Uses `thinkingBudget` to set the
/// number of thinking tokens. Dynamic thinking is enabled by default." and
/// "Set `thinkingBudget` to `0` to disable."
/// WHY: without this pin the 2.5-family model deliberates by default and
/// the user hears dead air. UPDATE 2026-09-24 (migration guide, checked
/// 2026-09-24): `gemini-3.8-live` does NOT take a thinking config at all —
/// interleaved reasoning is native and `thinking_config` must be OMITTED —
/// so the adapter sends this block ONLY on [`LiveFamily::Legacy25`] (the
/// agent channel today); the 3.8 tutor channel sends no thinking config.
/// REVIEW-BY: 2026-12-24
pub const LIVE_THINKING_BUDGET: u32 = 0;

/// Which Live wire family a model id belongs to. The adapter branches the
/// setup frame on this — the two families take DIFFERENT contracts:
///
/// - **Gemini38** (`gemini-3.8-live`, checked 2026-09-24): NO thinking
///   config (unsupported, interleaved reasoning native), async function
///   calling is the DEFAULT (tutor declarations pin `behavior: BLOCKING`),
///   `contextWindowCompression` is sent (3.8-era field), `proactivity` is
///   never sent (permanently enabled; `false` errors).
/// - **Legacy25** (the dated 2.5 native-audio preview): `thinkingBudget: 0`
///   (LIVE_THINKING_BUDGET), no `contextWindowCompression` field, explicit
///   `behavior` only where a schema declares it (agent: NON_BLOCKING).
///
/// A future model pin MUST re-verify both branches against its live docs
/// and update this mapping — this is the single place the setup contract
/// forks, by owner-tunable design (model_pins is the visible surface).
pub enum LiveFamily {
    Gemini38,
    Legacy25,
}

pub fn live_family(model: &str) -> LiveFamily {
    if model.contains("gemini-3.8") {
        LiveFamily::Gemini38
    } else {
        LiveFamily::Legacy25
    }
}

/// The tutor's dedicated transcription stream model (owner ruling
/// 2026-08-29: WIRED — the dialog model converses, this one writes the
/// bubbles; accurate transcripts ruled worth the doubled audio egress).
///
/// PINNED: 2026-08-29.
/// DOCS: https://ai.google.dev/gemini-api/docs/live-api/live-transcribe +
/// https://ai.google.dev/gemini-api/docs/models, checked 2026-08-29.
/// WHY: a dedicated real-time STT model, Live API `BidiGenerateContent`
/// with `responseModalities: ["TEXT"]`, documented to handle "multilingual
/// conversations and code-switching" with `customVocabulary` up to 1,000
/// terms — exactly the owner's Egyptian-Arabic-plus-English sessions.
/// `AudioTranscriptionConfig` offers no field to delegate a dialog
/// session's transcription to it, so it runs as a SECOND concurrent
/// session fed by the same mic fan-out (same key, same endpoint, same
/// per-(book, endpoint) consent grant). Sessions cap at 10 minutes of
/// continuous streaming — on stream death the display falls back to the
/// dialog model's own transcription, honestly noticed.
/// RE-VERIFIED 2026-10-09 against https://ai.google.dev/gemini-api/docs/models
/// (updated 2026-10-06: `gemini-3.5-transcribe` Stable, `-live` its live
/// endpoint) and .../live-api/live-transcribe (updated 2026-08-26: 10-minute
/// streaming cap, `customVocabulary` up to 1,000 terms, best ≤ 100, VERBATIM
/// default) — the facts above still hold.
/// REVIEW-BY: 2026-12-31
pub const TUTOR_TRANSCRIBE_MODEL: &str = "models/gemini-3.5-transcribe-live";

/// OWNER-TUNABLE toggle for the transcription stream above — kept a build
/// constant until the owner asks for a runtime setting.
///
/// PINNED: 2026-08-29.
/// DOCS: see [`TUTOR_TRANSCRIBE_MODEL`].
/// WHY: the owner ruled the stream ON with an explicit off-switch reserved
/// ("owner-tunable constant for the stream on/off so it can become a
/// toggle later"). `false` restores the single-session behavior: bubbles
/// from the dialog model's own input transcription.
/// REVIEW-BY: 2026-12-31
pub const TUTOR_TRANSCRIBE_STREAM: bool = true;

#[cfg(test)]
mod tests {
    /// The staleness convention is machine-checkable: every `pub const` pin
    /// in this file has a `REVIEW-BY: YYYY-MM-DD` line the pulse can grep,
    /// and the dates parse. (A date-vs-today assertion would be a CI time
    /// bomb; the PULSE owns comparing against today.)
    #[test]
    fn every_pin_carries_a_parseable_review_by_date() {
        let source = include_str!("model_pins.rs");
        let pins = source
            .lines()
            .filter(|line| line.starts_with("pub const"))
            .count();
        let dates: Vec<&str> = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("/// REVIEW-BY: "))
            .collect();
        assert_eq!(dates.len(), pins, "every pin carries a REVIEW-BY line");
        for date in dates {
            let parts: Vec<u32> = date
                .trim()
                .splitn(3, '-')
                .map(|part| part.parse().expect("REVIEW-BY parts are numeric"))
                .collect();
            assert_eq!(parts.len(), 3, "REVIEW-BY is YYYY-MM-DD: {date}");
            assert!((2026..2100).contains(&parts[0]), "sane year: {date}");
            assert!((1..=12).contains(&parts[1]), "sane month: {date}");
            assert!((1..=31).contains(&parts[2]), "sane day: {date}");
        }
    }
}
