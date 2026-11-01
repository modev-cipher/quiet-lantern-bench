//! Harness normalization of the `teacher_anchor` tool call (VOICE_AGENT §11.1).
//!
//! The tutor signals presentation sync as an ordinary provider-neutral tool call
//! named [`TEACHER_ANCHOR_TOOL`]; no channel gets an anchor side-channel. This
//! module is the single normalization point: the session harness feeds every such
//! tool call through [`normalize_teacher_anchor`] and gets back the exact
//! [`ChannelEvent::TeacherAnchor`] to surface (or the reason not to) plus the
//! [`ToolResult`] ack to send back on the channel. VA2 owns this event + ack
//! surface only; what the UI does with the event ships with the Phase 5 UI task.
//!
//! Staleness mirrors the audio gate (VOICE_AGENT §11.1, §2 rule 2): an anchor
//! whose response generation was superseded by a barge-in bump is dropped — the
//! provider still receives an ack so the tool protocol never stalls, but no event
//! is emitted.
//!
//! Argument validation is strict per the VOICE_AGENT §5 policy (unknown fields
//! rejected, closed `kind` vocabulary, non-empty `id`). The §5 clause that the
//! `id` must exist in the open book is enforced where book data lives — the session
//! pump (harness) via `ExcerptSelector::has_anchor` — not here; this module validates shape, not existence.

use serde_json::Value;

use super::event::{AnchorKind, AnchorRef, ChannelEvent};
use super::types::ToolResult;
use crate::voice::audio::generation::Generation;

/// The provider-neutral tool name every adapter and fixture uses (VOICE_AGENT §5).
pub const TEACHER_ANCHOR_TOOL: &str = "teacher_anchor";

/// Ack error code for an anchor from a superseded response generation.
pub const STALE_GENERATION_CODE: &str = "stale_generation";

/// Ack error code for arguments that fail strict schema validation (§5 policy).
pub const OUT_OF_SCHEMA_CODE: &str = "out_of_schema";

/// Ack error code for an anchor kind that is real §11.1 vocabulary but
/// descoped from live sessions (today: `section`, until #205's upstream
/// fixes land). Distinct from [`OUT_OF_SCHEMA_CODE`] on purpose: the model
/// did nothing malformed and the message tells it what to do instead —
/// a loud refusal, never an `applied` ack over a preview that silently
/// rendered nothing (the #201 transcript-lie class, PR #206 verifier ruling).
pub const UNSUPPORTED_ANCHOR_KIND_CODE: &str = "unsupported_anchor_kind";

/// What the harness must do with one `teacher_anchor` tool call: at most one
/// event to surface, always exactly one ack to send.
#[derive(Debug, Clone, PartialEq)]
pub enum AnchorDecision {
    /// Live generation, valid args: surface the event, ack success.
    Emit {
        event: ChannelEvent,
        ack: ToolResult,
    },
    /// Superseded generation (VOICE_AGENT §11.1 staleness rule): no event.
    Stale { ack: ToolResult },
    /// No event, error ack: args failed strict validation (VOICE_AGENT §5,
    /// [`OUT_OF_SCHEMA_CODE`]) or named descoped vocabulary
    /// ([`UNSUPPORTED_ANCHOR_KIND_CODE`] — the routing is identical, the
    /// code tells the model which mistake it made).
    Invalid { ack: ToolResult },
}

impl AnchorDecision {
    pub fn ack(&self) -> &ToolResult {
        match self {
            Self::Emit { ack, .. } | Self::Stale { ack } | Self::Invalid { ack } => ack,
        }
    }
}

fn invalid(message: impl Into<String>) -> AnchorDecision {
    AnchorDecision::Invalid {
        ack: ToolResult::Err {
            code: OUT_OF_SCHEMA_CODE.to_string(),
            message: message.into(),
        },
    }
}

/// Normalizes one `teacher_anchor` tool call into the event/ack pair.
///
/// `response_gen` is the generation of the response the tool call belongs to —
/// the same id the response's `AudioDelta`s carry — and `live` is the shared
/// barge-in counter (`voice::audio::generation::Generation`). Validation runs
/// before the staleness check so a malformed call is reported as malformed even
/// when it is also stale.
pub fn normalize_teacher_anchor(
    args: &Value,
    response_gen: u64,
    live: &Generation,
) -> AnchorDecision {
    let Some(fields) = args.as_object() else {
        return invalid("teacher_anchor args must be a JSON object");
    };
    if let Some(unknown) = fields.keys().find(|k| *k != "kind" && *k != "id") {
        return invalid(format!("unknown teacher_anchor field: {unknown}"));
    }
    let Some(kind_str) = fields.get("kind").and_then(Value::as_str) else {
        return invalid("teacher_anchor requires a string `kind`");
    };
    let Some(kind) = AnchorKind::from_tool_arg(kind_str) else {
        return invalid(format!(
            "teacher_anchor kind must be one of section|figure|equation|table, got {kind_str:?}"
        ));
    };
    let Some(id) = fields.get("id").and_then(Value::as_str) else {
        return invalid("teacher_anchor requires a string `id`");
    };
    if id.is_empty() {
        return invalid("teacher_anchor `id` must be non-empty");
    }

    if response_gen != live.current() {
        return AnchorDecision::Stale {
            ack: ToolResult::Err {
                code: STALE_GENERATION_CODE.to_string(),
                message: format!(
                    "anchor from superseded generation {response_gen} (live: {})",
                    live.current()
                ),
            },
        };
    }

    // Section anchors are descoped from live sessions until #205's upstream
    // gaps land (PR #206 verifier ruling): the UI resolves section ids
    // against render-local heading segments and would silently render
    // nothing while this ack claimed `applied`. Refused AFTER the staleness
    // check on purpose — a superseded call is dropped as stale regardless of
    // kind, so the distinct code fires exactly where the lie would have: on
    // a call that would otherwise emit. Policy, not shape: the tool schema
    // no longer offers `section`, so a live model reaching this arm ignored
    // its vocabulary. Budget-free by construction: the anchor path never
    // touches the dispatcher's Tier-1 rate window (H3's refusals-cost-
    // nothing rule holds trivially). Presentation anchors are budget-free
    // and exempt per VOICE_AGENT §5/§8.1 and TC-VTOOLS-03.
    if matches!(kind, AnchorKind::Section) {
        tracing::warn!(
            "teacher_anchor refused: section anchors are descoped (#205), id {id:?} not applied"
        );
        return AnchorDecision::Invalid {
            ack: ToolResult::Err {
                code: UNSUPPORTED_ANCHOR_KIND_CODE.to_string(),
                message: "section anchors are not supported in this session; \
                          anchor a figure, equation, or table id from your \
                          context instead"
                    .to_string(),
            },
        };
    }

    AnchorDecision::Emit {
        event: ChannelEvent::TeacherAnchor {
            anchor: AnchorRef {
                kind,
                id: id.to_string(),
            },
            gen: response_gen,
        },
        ack: ToolResult::Ok(serde_json::json!({ "applied": true })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expect_invalid(decision: AnchorDecision) -> String {
        match decision {
            AnchorDecision::Invalid {
                ack: ToolResult::Err { code, message },
            } => {
                assert_eq!(code, OUT_OF_SCHEMA_CODE);
                message
            }
            other => panic!("expected Invalid with an error ack, got {other:?}"),
        }
    }

    #[test]
    fn a_live_valid_anchor_emits_the_event_and_a_success_ack() {
        let live = Generation::new();
        let decision =
            normalize_teacher_anchor(&json!({"kind": "figure", "id": "fig-3"}), 0, &live);
        match decision {
            AnchorDecision::Emit { event, ack } => {
                assert_eq!(
                    event,
                    ChannelEvent::TeacherAnchor {
                        anchor: AnchorRef {
                            kind: AnchorKind::Figure,
                            id: "fig-3".to_string(),
                        },
                        gen: 0,
                    }
                );
                assert_eq!(ack, ToolResult::Ok(json!({ "applied": true })));
            }
            other => panic!("expected Emit, got {other:?}"),
        }
    }

    #[test]
    fn every_supported_kind_normalizes() {
        let live = Generation::new();
        for (arg, kind) in [
            ("figure", AnchorKind::Figure),
            ("equation", AnchorKind::Equation),
            ("table", AnchorKind::Table),
        ] {
            match normalize_teacher_anchor(&json!({"kind": arg, "id": "x-1"}), 0, &live) {
                AnchorDecision::Emit {
                    event: ChannelEvent::TeacherAnchor { anchor, .. },
                    ..
                } => assert_eq!(anchor.kind, kind),
                other => panic!("kind {arg} must emit, got {other:?}"),
            }
        }
    }

    /// The #205 descope (PR #206 verifier ruling): a live, well-formed
    /// section anchor must refuse loudly with the distinct code — never an
    /// `applied` ack over a preview that rendered nothing — and must not
    /// surface an event.
    #[test]
    fn a_live_section_anchor_refuses_with_unsupported_anchor_kind() {
        let live = Generation::new();
        let decision =
            normalize_teacher_anchor(&json!({"kind": "section", "id": "sec-2"}), 0, &live);
        match decision {
            AnchorDecision::Invalid {
                ack: ToolResult::Err { code, message },
            } => {
                assert_eq!(code, UNSUPPORTED_ANCHOR_KIND_CODE);
                assert!(
                    message.contains("figure, equation, or table"),
                    "the refusal must tell the model what to do instead: {message}"
                );
            }
            other => panic!("expected the loud refusal, got {other:?}"),
        }
    }

    /// Order pin: staleness still wins over the descope — a superseded
    /// section anchor answers `stale_generation`, byte-compatible with every
    /// pre-descope fixture (see also
    /// [`a_superseded_generation_is_dropped_but_still_acked`], which runs on
    /// section args for exactly this reason).
    #[test]
    fn a_stale_section_anchor_is_stale_not_unsupported() {
        let live = Generation::new();
        live.bump();
        let decision =
            normalize_teacher_anchor(&json!({"kind": "section", "id": "sec-2"}), 0, &live);
        assert!(
            matches!(
                decision,
                AnchorDecision::Stale {
                    ack: ToolResult::Err { ref code, .. }
                } if code == STALE_GENERATION_CODE
            ),
            "staleness must be judged before the descope, got {decision:?}"
        );
    }

    #[test]
    fn a_superseded_generation_is_dropped_but_still_acked() {
        let live = Generation::new();
        live.bump();
        let decision =
            normalize_teacher_anchor(&json!({"kind": "section", "id": "sec-2"}), 0, &live);
        match decision {
            AnchorDecision::Stale {
                ack: ToolResult::Err { code, .. },
            } => assert_eq!(code, STALE_GENERATION_CODE),
            other => panic!("expected Stale with an error ack, got {other:?}"),
        }
    }

    #[test]
    fn a_future_generation_is_not_live_either() {
        let live = Generation::new();
        let decision = normalize_teacher_anchor(&json!({"kind": "table", "id": "tbl-1"}), 5, &live);
        assert!(
            matches!(decision, AnchorDecision::Stale { .. }),
            "only the live generation may surface, mirroring the audio gate"
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let live = Generation::new();
        let message = expect_invalid(normalize_teacher_anchor(
            &json!({"kind": "figure", "id": "fig-1", "page": 4}),
            0,
            &live,
        ));
        assert!(message.contains("page"));
    }

    #[test]
    fn out_of_vocabulary_kind_is_rejected() {
        let live = Generation::new();
        expect_invalid(normalize_teacher_anchor(
            &json!({"kind": "paragraph", "id": "p-9"}),
            0,
            &live,
        ));
    }

    #[test]
    fn missing_or_non_string_or_empty_fields_are_rejected() {
        let live = Generation::new();
        for bad in [
            json!({"id": "fig-1"}),
            json!({"kind": "figure"}),
            json!({"kind": 3, "id": "fig-1"}),
            json!({"kind": "figure", "id": 7}),
            json!({"kind": "figure", "id": ""}),
            json!("not an object"),
            json!(null),
        ] {
            expect_invalid(normalize_teacher_anchor(&bad, 0, &live));
        }
    }

    /// Full order pin for the #205 descope: shape validation runs before the
    /// descope refusal, so a *malformed* section call is reported as malformed
    /// ([`OUT_OF_SCHEMA_CODE`]) — the distinct [`UNSUPPORTED_ANCHOR_KIND_CODE`]
    /// fires only on a call that would otherwise have emitted. Together with
    /// [`a_stale_section_anchor_is_stale_not_unsupported`] this pins the
    /// descope check as the LAST gate: validation, then staleness, then
    /// descope.
    #[test]
    fn a_malformed_section_call_is_out_of_schema_not_unsupported() {
        let live = Generation::new();
        for bad in [
            json!({"kind": "section", "id": ""}),
            json!({"kind": "section", "id": 7}),
            json!({"kind": "section"}),
        ] {
            expect_invalid(normalize_teacher_anchor(&bad, 0, &live));
        }
    }

    #[test]
    fn a_malformed_call_reports_out_of_schema_even_when_it_is_also_stale() {
        let live = Generation::new();
        live.bump();
        expect_invalid(normalize_teacher_anchor(
            &json!({"kind": "nope", "id": "x"}),
            0,
            &live,
        ));
    }
}
