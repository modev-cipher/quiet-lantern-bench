//! The session-harness event pump (task VA6 in docs/TASKS.md; VOICE_AGENT §3,
//! §5, §11.1).
//!
//! One loop owns the receiver handed over by
//! [`SessionChannel::take_events`] and turns every [`ChannelEvent`] into the
//! reaction the spec owes for it. This slice wires the tool plane:
//!
//! - `ToolCall` named `teacher_anchor` **on a session that offers it** routes
//!   to the anchor path — [`normalize_teacher_anchor`] decides emit/stale/
//!   invalid and the pump surfaces the `TeacherAnchor` event — *before* the
//!   dispatcher ever sees the call, so [`CODE_UNROUTED_TOOL`] cannot occur on
//!   a correctly wired session (it remains the dispatcher's backstop for a
//!   mis-wired one). On a session that never offered the tool the call falls
//!   through to the dispatcher on purpose: it must be answered `unknown_tool`,
//!   byte-identical to a hallucinated name (pin1 — the answer must not leak
//!   that the gated tool exists).
//! - Every other `ToolCall` goes through the H1 dispatch/send shape
//!   ([`answer_tool_call`]) under the real clock: dispatch at most once per
//!   `call_id`, cache the answer, send; a failed send keeps the answer for
//!   [`resend_pending_answers`] after the harness recovers the channel.
//! - Everything else is forwarded verbatim to the caller's sink — the audio
//!   writer, transcript view, and lifecycle policy own those events and land
//!   in their own slices.
//!
//! **Response-generation attribution** (VOICE_AGENT §11.1 staleness; §2 rule
//! 2): `ChannelEvent::ToolCall` carries no `gen`, so the pump attributes each
//! anchor to the response committed at the last turn boundary — it snapshots
//! the live [`Generation`] at every `UtteranceEnd`, exactly the composed
//! loop's `reply_gen` discipline (`channel::composed`: commit-time snapshot,
//! held until the next boundary). A barge-in bump between that snapshot and
//! the anchor's arrival makes the anchor stale: acked, never surfaced. The
//! same holds across the pump's own queue — events buffered behind a bump are
//! still judged against the generation their response was committed under.

use std::time::Instant;

use tokio::sync::mpsc;

use super::audio::generation::Generation;
use super::channel::anchor::{normalize_teacher_anchor, AnchorDecision, TEACHER_ANCHOR_TOOL};
use super::channel::{ChannelError, ChannelEvent, SessionChannel};
use super::excerpt::selector::ExcerptSelector;
use super::tools::dispatcher::{answer_once, answer_tool_call, Dispatcher};
use super::tools::playback::PlaybackControl;

#[cfg(doc)]
use super::tools::dispatcher::{resend_pending_answers, CODE_UNROUTED_TOOL};

pub mod behavioral;
mod pins;
mod smoke;

/// The event pump for one voice session. Owns only what no other seam owns:
/// the response-generation attribution state. The channel, dispatcher,
/// playback port, and excerpt selector stay with the session harness and are
/// borrowed per call, mirroring the dispatcher's own ownership shape.
pub struct EventPump {
    pub require_visible_anchor: bool,
    /// One deferred citation, set when the Agent dispatch returned Deferred:
    /// (provider call id, the response generation it was issued under). The
    /// session runtime drains it into its pending slot; the surface ack (or
    /// the deadline) answers it, generation-gated at settlement.
    #[cfg(feature = "book-agent")]
    pub deferred_highlight: Option<(String, u64)>,
    /// Set by the runtime before each pumped event: a citation is already
    /// pending, so a second highlight_passage is refused BEFORE any
    /// presentation emission (A10, round-3 item 4) — the registered surface
    /// citation is never replaced by a call the provider is told failed.
    #[cfg(feature = "book-agent")]
    pub citation_pending: bool,
    generation: Generation,
    /// The generation the in-flight response was committed under — the value
    /// [`normalize_teacher_anchor`] compares against the live counter. Starts
    /// at the session's opening generation, re-snapshotted at every
    /// `UtteranceEnd` (the turn boundary that commits the next response).
    response_gen: u64,
    /// F7 sight port: `Some` when the session may answer `look_at_page`
    /// (tutor sessions with a rendered book). `None` refuses honestly.
    pub vision: Option<Box<dyn crate::voice::tools::vision::VisionPort>>,
    pub sight_refused: bool,
}

impl EventPump {
    pub fn response_generation(&self) -> u64 {
        self.response_gen
    }
    /// `generation` is the session's shared barge-in counter — the same
    /// handle the audio gate and [`super::channel::barge_in`] use.
    pub fn new(generation: Generation) -> Self {
        let response_gen = generation.current();
        Self {
            require_visible_anchor: false,
            #[cfg(feature = "book-agent")]
            deferred_highlight: None,
            #[cfg(feature = "book-agent")]
            citation_pending: false,
            generation,
            response_gen,
            vision: None,
            sight_refused: false,
        }
    }

    /// Consume events until the stream ends (the channel closed and dropped
    /// its sender): the session's steady-state loop.
    ///
    /// Errors are send failures only — every tool outcome, refusal, and
    /// anchor decision is an *answer*, not an error. On `Err` the computed
    /// answer is already cached in the dispatcher (H1), and the receiver is
    /// borrowed, not consumed: after the harness recovers the channel
    /// (renewal wait, [`resend_pending_answers`]) it re-enters `run` with the
    /// same receiver and no event or ack is lost or doubled.
    pub async fn run(
        &mut self,
        events: &mut mpsc::Receiver<ChannelEvent>,
        channel: &mut dyn SessionChannel,
        dispatcher: &mut Dispatcher,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
        forward: &mut (dyn FnMut(ChannelEvent) + Send),
    ) -> Result<(), ChannelError> {
        while let Some(event) = events.recv().await {
            self.handle(event, channel, dispatcher, playback, excerpts, forward)
                .await?;
        }
        Ok(())
    }

    /// React to one event — `run`'s body, public so a step-driven harness (or
    /// a pin suite) can interleave barge-in bumps between events
    /// deterministically.
    pub async fn handle(
        &mut self,
        event: ChannelEvent,
        channel: &mut dyn SessionChannel,
        dispatcher: &mut Dispatcher,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
        forward: &mut (dyn FnMut(ChannelEvent) + Send),
    ) -> Result<(), ChannelError> {
        match event {
            // The turn boundary commits the next response under the now-live
            // generation (the composed loop's `reply_gen` rule). Forwarded
            // too: the harness tracks its mic-ring replay watermark off it
            // (VOICE_AGENT §3).
            ChannelEvent::UtteranceEnd => {
                self.response_gen = self.generation.current();
                forward(ChannelEvent::UtteranceEnd);
            }
            ChannelEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                let anchor_offered = dispatcher.registry().tier(TEACHER_ANCHOR_TOOL).is_some();
                let sight_offered = dispatcher
                    .registry()
                    .tier(crate::voice::tools::registry::LOOK_AT_PAGE)
                    .is_some();
                if name == crate::voice::tools::registry::LOOK_AT_PAGE && sight_offered {
                    // F7 sight (owner ruling 2026-08-30, tutor tier:
                    // on-demand only — the USER's question triggered this
                    // call). The page still goes over the wire FIRST, the
                    // ack second, so the model's next words already have
                    // the pixels. Every failure is an honest refusal — the
                    // model must never be told it saw a page it did not.
                    // At-most-once side effect (the teacher_anchor
                    // discipline): a retried call re-sends only its cached
                    // ack — the still itself never goes twice.
                    let fresh = dispatcher.pending_answer(&call_id).is_none();
                    let sent: Result<i64, String> = match self.vision.as_mut() {
                        Some(_) if !fresh => Err(String::new()),
                        Some(port) => {
                            match playback
                                .position_ms()
                                .map_err(|_| "playback position unavailable".to_string())
                                .and_then(|pos_ms| {
                                    dispatcher
                                        .shown_page()
                                        .or_else(|| excerpts.page_at(pos_ms))
                                        .ok_or_else(|| {
                                            "no page at the current position".to_string()
                                        })
                                }) {
                                Ok(page_no) => match port.frame_for_page(page_no).await {
                                    Ok(frame) => channel
                                        .send_image(&frame.bytes, frame.mime)
                                        .await
                                        .map(|()| frame.page_no)
                                        .map_err(|error| error.message),
                                    Err(reason) => Err(reason),
                                },
                                Err(reason) => Err(reason),
                            }
                        }
                        None => Err("page images are not available in this session".to_string()),
                    };
                    if fresh && sent.is_err() {
                        self.sight_refused = true;
                    }
                    answer_once(dispatcher, channel, &call_id, |dispatcher| {
                        let (ok, code) = match &sent {
                            Ok(_) => (true, None),
                            Err(reason) => (false, Some(reason.clone())),
                        };
                        dispatcher.journal_record(
                            crate::voice::tools::dispatcher::ToolOutcomeRecord {
                                tool: crate::voice::tools::registry::LOOK_AT_PAGE.to_string(),
                                call_id: call_id.to_string(),
                                ok,
                                code,
                                pos_ms: None,
                            },
                        );
                        match &sent {
                            Ok(page_no) => crate::voice::channel::types::ToolResult::Ok(
                                serde_json::json!({ "sent": true, "page": page_no }),
                            ),
                            Err(reason) => crate::voice::channel::types::ToolResult::Err {
                                code: "page_image_unavailable".to_string(),
                                message: reason.clone(),
                            },
                        }
                    })
                    .await?;
                } else if name == TEACHER_ANCHOR_TOOL && anchor_offered {
                    // The anchor path (§11.1), under the same at-most-once
                    // shape as dispatch: normalize once, surface the event at
                    // most once, ack always — a retry after a failed ack send
                    // re-sends the cached ack and cannot re-emit the event or
                    // flip the decision (re-normalizing after a later bump
                    // would judge the same call stale on the second look).
                    let response_gen = self.response_gen;
                    let generation = &self.generation;
                    let require_visible = self.require_visible_anchor;
                    answer_once(dispatcher, channel, &call_id, |dispatcher| {
                        match normalize_teacher_anchor(&args, response_gen, generation) {
                            AnchorDecision::Emit { event, ack } => {
                                let anchor_ref = match &event {
                                    ChannelEvent::TeacherAnchor { anchor, .. } => anchor,
                                    _ => unreachable!(),
                                };
                                if !excerpts.has_anchor(&anchor_ref.kind, &anchor_ref.id) {
                                    let code = "unknown_anchor".to_string();
                                    let message = format!(
                                        "{} id {:?} not found in the open book",
                                        anchor_ref.kind.as_tool_arg(),
                                        anchor_ref.id
                                    );
                                    dispatcher.record_anchor_refusal(&code, &message);
                                    dispatcher.journal_record(
                                        crate::voice::tools::dispatcher::ToolOutcomeRecord {
                                            tool: TEACHER_ANCHOR_TOOL.to_string(),
                                            call_id: call_id.to_string(),
                                            ok: false,
                                            code: Some(code.clone()),
                                            pos_ms: None,
                                        },
                                    );
                                    crate::voice::channel::types::ToolResult::Err { code, message }
                                } else if require_visible && dispatcher.shown_page() != excerpts.anchor_page(&anchor_ref.kind, &anchor_ref.id) {
                                    let message = "This emphasis target is not on the page confirmed by the reader. Navigate to its document page first and wait for an applied result.";
                                    dispatcher.record_anchor_refusal("anchor_page_not_displayed", message);
                                    dispatcher.journal_record(crate::voice::tools::dispatcher::ToolOutcomeRecord {
                                        tool: TEACHER_ANCHOR_TOOL.into(), call_id: call_id.clone(), ok: false,
                                        code: Some("anchor_page_not_displayed".into()), pos_ms: None,
                                    });
                                    crate::voice::channel::types::ToolResult::Err { code: "anchor_page_not_displayed".into(), message: message.into() }
                                } else {
                                    forward(event);
                                    dispatcher.journal_record(
                                        crate::voice::tools::dispatcher::ToolOutcomeRecord {
                                            tool: TEACHER_ANCHOR_TOOL.to_string(),
                                            call_id: call_id.to_string(),
                                            ok: true,
                                            code: None,
                                            pos_ms: None,
                                        },
                                    );
                                    if require_visible {
                                        crate::voice::channel::types::ToolResult::Ok(serde_json::json!({
                                            "requested": true,
                                            "status": "Visual emphasis was requested on the already displayed page. This does not confirm a rendered spotlight or a page change."
                                        }))
                                    } else { ack }
                                }
                            }
                            // A refused anchor is recorded like a refused
                            // jump (dossier F2): the session runtime drains
                            // it and surfaces the failure to the user —
                            // never an ack the model alone hears.
                            AnchorDecision::Stale { ack } | AnchorDecision::Invalid { ack } => {
                                if let crate::voice::channel::types::ToolResult::Err {
                                    code,
                                    message,
                                } = &ack
                                {
                                    dispatcher.record_anchor_refusal(code, message);
                                    dispatcher.journal_record(
                                        crate::voice::tools::dispatcher::ToolOutcomeRecord {
                                            tool: TEACHER_ANCHOR_TOOL.to_string(),
                                            call_id: call_id.to_string(),
                                            ok: false,
                                            code: Some(code.clone()),
                                            pos_ms: None,
                                        },
                                    );
                                }
                                ack
                            }
                        }
                    })
                    .await?;
                } else {
                    // Agent sessions run the full authority envelope here
                    // (charter §2.2/§2.3/§4.4.3, Hermes finale items 4-5):
                    // pre-dispatch generation gate, sentinel wrap of every
                    // book-derived result on the SEND path, post-dispatch
                    // generation gate (a result computed across a barge-in
                    // never enters the new turn), and deferred citations.
                    // Tutor sessions (no search_book offered) are untouched.
                    #[cfg(feature = "book-agent")]
                    {
                        use crate::voice::tools::registry as tools;
                        let agent_session =
                            dispatcher.registry().tier(tools::SEARCH_BOOK).is_some();
                        if agent_session {
                            if let Some(stale_ack) = crate::agent_session::gate_stale_agent_call(
                                &name,
                                self.response_gen,
                                &self.generation,
                            ) {
                                answer_once(dispatcher, channel, &call_id, |_| stale_ack).await?;
                                return Ok(());
                            }
                            if name == tools::HIGHLIGHT_PASSAGE && self.citation_pending {
                                // A10 preflight: refused before the surface
                                // hears anything — no emission, no replace.
                                answer_once(dispatcher, channel, &call_id, |_| {
                                    crate::voice::channel::types::ToolResult::Err {
                                        code: "highlight_not_applied".to_string(),
                                        message: "another citation is already pending".to_string(),
                                    }
                                })
                                .await?;
                                return Ok(());
                            }
                            if dispatcher.pending_answer(&call_id).is_some() {
                                answer_once(dispatcher, channel, &call_id, |_| unreachable!())
                                    .await?;
                                return Ok(());
                            }
                            let outcome = dispatcher.dispatch_with_call_id(
                                &name,
                                &args,
                                &call_id,
                                Instant::now(),
                                playback,
                                excerpts,
                            );
                            if matches!(outcome, crate::voice::tools::ToolOutcome::Deferred) {
                                // The surface answers after render; the
                                // runtime owns the deadline. Nothing is sent.
                                self.deferred_highlight = Some((call_id, self.response_gen));
                                return Ok(());
                            }
                            let response_gen = self.response_gen;
                            let generation = &self.generation;
                            answer_once(dispatcher, channel, &call_id, |_| {
                                // Post-dispatch gate: a barge-in that landed
                                // while the tool ran makes this result stale
                                // — it must not enter the new turn (§2.2
                                // step 2), even though the local effect (a
                                // seek that already happened) stands.
                                if generation.current() != response_gen {
                                    return crate::voice::channel::types::ToolResult::Err {
                                        code: crate::agent_session::STALE_GENERATION_CODE
                                            .to_string(),
                                        message: "superseded while executing; no result                                                   was delivered"
                                            .to_string(),
                                    };
                                }
                                let result = outcome.into_tool_result();
                                // Every book-derived payload crosses to the
                                // provider sentinel-wrapped (§4.4.3): the ONE
                                // session sentinel, at the one send boundary.
                                match (&*name, result) {
                                    (
                                        tools::SEARCH_BOOK | tools::OPEN_PASSAGE,
                                        crate::voice::channel::types::ToolResult::Ok(payload),
                                    ) => crate::agent_session::wrap_agent_tool_payload(
                                        excerpts, &payload,
                                    ),
                                    (_, result) => result,
                                }
                            })
                            .await?;
                            return Ok(());
                        }
                    }
                    answer_tool_call(
                        dispatcher,
                        channel,
                        &call_id,
                        &name,
                        &args,
                        Instant::now(),
                        playback,
                        excerpts,
                    )
                    .await?;
                }
            }
            // Audio, transcripts, word timing, anchors already normalized,
            // lifecycle, errors: owned by their own seams, forwarded verbatim.
            other => forward(other),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::voice::channel::anchor::{
        OUT_OF_SCHEMA_CODE, STALE_GENERATION_CODE, UNSUPPORTED_ANCHOR_KIND_CODE,
    };
    use crate::voice::channel::mock::{MockChannel, MockDriver};
    use crate::voice::channel::types::{
        DelimitedExcerpt, ErrorKind, LifecycleEvent, Role, SessionContext, ToolResult,
    };
    use crate::voice::channel::{AnchorKind, AnchorRef};
    use crate::voice::excerpt::source::SessionSnapshot;
    use crate::voice::excerpt::window::WindowPolicy;
    use crate::voice::tools::dispatcher::{
        resend_pending_answers, CODE_RATE_LIMITED, CODE_UNKNOWN_TOOL, CODE_UNROUTED_TOOL,
        TIER_ONE_MAX_CALLS,
    };
    use crate::voice::tools::playback::InMemoryPlayback;
    use crate::voice::tools::registry::{self, ToolRegistry};

    fn selector() -> ExcerptSelector {
        ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 1000, "pump book segment zero"),
                    (1, 1000, 2000, "pump book segment one"),
                    (2, 2000, 3000, "pump book segment two"),
                ],
                vec![(0, 2000), (2000, 3000)],
            )
            .with_test_figures_with_id(vec![
                ("fig-1", 1, "figure", "Figure 1 diagram"),
                ("fig-2", 1, "figure", "Figure 2 chart"),
                ("fig-3", 1, "figure", "Figure 3 chart"),
                ("tbl-1", 2, "table", "Table 1 data"),
                ("eq-2", 2, "equation", "Equation 2 formula"),
            ]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        )
    }

    async fn connected_mock() -> (MockChannel, MockDriver) {
        let (mut channel, driver) = MockChannel::builder().build();
        channel
            .connect(SessionContext {
                book_id: Some("pump-book".to_string()),
                session_id: "pump-session".to_string(),
                system_prompt: "tutor".to_string(),
                excerpt: DelimitedExcerpt {
                    delimited_text: "<s>x</s>".to_string(),
                },
                rolling_summary: None,
                playback_pos_ms: 0,
                tool_schemas: ToolRegistry::new(true).schemas(),
            })
            .await
            .expect("mock connects");
        (channel, driver)
    }

    struct Rig {
        pump: EventPump,
        generation: Generation,
        channel: MockChannel,
        driver: MockDriver,
        dispatcher: Dispatcher,
        playback: InMemoryPlayback,
        excerpts: ExcerptSelector,
        forwarded: Vec<ChannelEvent>,
    }

    async fn rig(anchor_offered: bool) -> Rig {
        let generation = Generation::new();
        let (channel, driver) = connected_mock().await;
        Rig {
            pump: EventPump::new(generation.clone()),
            generation,
            channel,
            driver,
            dispatcher: Dispatcher::new(ToolRegistry::new(anchor_offered)),
            playback: InMemoryPlayback::at(1500),
            excerpts: selector(),
            forwarded: Vec::new(),
        }
    }

    impl Rig {
        async fn handle(&mut self, event: ChannelEvent) -> Result<(), ChannelError> {
            self.pump
                .handle(
                    event,
                    &mut self.channel,
                    &mut self.dispatcher,
                    &mut self.playback,
                    &mut self.excerpts,
                    &mut |event| self.forwarded.push(event),
                )
                .await
        }
    }

    fn tool_call(call_id: &str, name: &str, args: serde_json::Value) -> ChannelEvent {
        ChannelEvent::ToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            args,
        }
    }

    #[tokio::test]
    async fn run_answers_tool_calls_and_forwards_everything_else_in_order() {
        let mut rig = rig(true).await;
        let (tx, mut events) = mpsc::channel(16);
        let script = vec![
            ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "pause it".to_string(),
            },
            ChannelEvent::UtteranceEnd,
            tool_call("call-1", registry::PAUSE_PLAYBACK, json!({})),
            ChannelEvent::AudioDelta {
                pcm: vec![1, 2, 3],
                gen: 0,
            },
            ChannelEvent::Lifecycle(LifecycleEvent::Expired),
        ];
        for event in script {
            tx.try_send(event).expect("scripted event fits");
        }
        drop(tx); // the stream ends: run returns

        rig.pump
            .run(
                &mut events,
                &mut rig.channel,
                &mut rig.dispatcher,
                &mut rig.playback,
                &mut rig.excerpts,
                &mut |event| rig.forwarded.push(event),
            )
            .await
            .expect("no send fails in this script");

        assert!(rig.playback.paused, "the tool call executed");
        assert_eq!(
            rig.driver.tool_results(),
            vec![(
                "call-1".to_string(),
                ToolResult::Ok(json!({ "paused": true }))
            )],
            "exactly one ack, and the ToolCall itself is consumed, not forwarded"
        );
        assert_eq!(
            rig.forwarded,
            vec![
                ChannelEvent::TranscriptDelta {
                    role: Role::User,
                    text: "pause it".to_string(),
                },
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: vec![1, 2, 3],
                    gen: 0,
                },
                ChannelEvent::Lifecycle(LifecycleEvent::Expired),
            ],
            "everything that is not a ToolCall passes through verbatim, in order"
        );
    }

    /// F7 honesty law: without a sight port the tool refuses loudly — the
    /// model is never told it saw a page it did not (the happy path is
    /// live-smoke territory: MockChannel deliberately refuses images too,
    /// exercising the same refusal shape end to end).
    #[tokio::test]
    async fn look_at_page_refuses_honestly_without_sight() {
        let mut rig = rig(true).await;
        rig.handle(tool_call(
            "call-look",
            crate::voice::tools::registry::LOOK_AT_PAGE,
            json!({}),
        ))
        .await
        .expect("acked");
        let results = rig.driver.tool_results();
        assert_eq!(results.len(), 1);
        match &results[0] {
            (id, ToolResult::Err { code, message }) => {
                assert_eq!(id, "call-look");
                assert_eq!(code, "page_image_unavailable");
                assert!(
                    message.contains("not available"),
                    "the refusal names the truth: {message}"
                );
            }
            other => panic!("expected an honest refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refused_look_is_flagged_and_other_tools_are_not() {
        let mut rig = rig(true).await;
        rig.handle(tool_call(
            "call-jump",
            crate::voice::tools::registry::JUMP_TO_PAGE,
            json!({ "page": 1 }),
        ))
        .await
        .expect("acked");
        assert!(!rig.pump.sight_refused);

        rig.handle(tool_call(
            "call-look",
            crate::voice::tools::registry::LOOK_AT_PAGE,
            json!({}),
        ))
        .await
        .expect("acked");
        assert!(rig.pump.sight_refused);
    }

    #[tokio::test]
    async fn emphasis_cannot_claim_or_steer_to_an_unconfirmed_page() {
        let mut rig = rig(true).await;
        rig.pump.require_visible_anchor = true;
        rig.dispatcher.set_shown_page(Some(2));
        rig.handle(tool_call(
            "hidden-anchor",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "figure", "id": "fig-1"}),
        ))
        .await
        .unwrap();
        assert!(
            matches!(&rig.driver.tool_results()[0].1, ToolResult::Err { code, .. } if code == "anchor_page_not_displayed")
        );
        assert!(rig.forwarded.is_empty());
        rig.dispatcher.set_shown_page(Some(1));
        rig.handle(tool_call(
            "visible-anchor",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "figure", "id": "fig-1"}),
        ))
        .await
        .unwrap();
        assert!(
            matches!(&rig.driver.tool_results()[1].1, ToolResult::Ok(payload) if payload["requested"] == true && payload.get("applied").is_none())
        );
        assert_eq!(rig.forwarded.len(), 1);
    }

    /// F6 carried pin — the standalone 13-tool matrix: every core tool
    /// answered through the real pump in a standalone session shape (mode
    /// Standalone, EMPTY snapshot, no playing surface, no sight port, a live
    /// profile sink — the F6 runtime wiring). Book tools refuse with their
    /// typed no-book codes, surface-touching audio tools answer
    /// `playback_unavailable`, `look_at_page` refuses `page_image_unavailable`
    /// honestly, and the book-independent `update_learning_profile` succeeds.
    /// Guards the sabotage of collapsing standalone refusals into one generic
    /// code (navigation honesty and the model's retry wording key on them) or
    /// silently "succeeding" a tool whose surface is absent.
    #[tokio::test]
    async fn standalone_all_thirteen_tools_answer_their_documented_outcomes() {
        struct AcceptingProfile;
        impl crate::voice::tools::learning::LearningProfileSink for AcceptingProfile {
            fn update(
                &mut self,
                _key: &str,
                _value: &str,
            ) -> Result<(), crate::voice::tools::learning::ProfileRefused> {
                Ok(())
            }
        }
        use crate::voice::tools::dispatcher::{
            DispatchMode, CODE_NO_CONTEXT, CODE_PLAYBACK_UNAVAILABLE, CODE_UNKNOWN_CHAPTER,
            CODE_UNKNOWN_PAGE,
        };
        let mut rig = rig(true).await;
        rig.dispatcher.set_mode(DispatchMode::Standalone);
        rig.dispatcher.set_profile_sink(Box::new(AcceptingProfile));
        rig.playback = InMemoryPlayback::at(0);
        rig.playback.available = false;
        rig.excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(vec![], vec![]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );

        enum Expect {
            Ok,
            Err(&'static str),
        }
        let matrix: Vec<(&str, serde_json::Value, Expect)> = vec![
            // Surface-touching audio tools: the standalone session has no
            // playing surface — retryable typed refusal, never a panic.
            (
                registry::PAUSE_PLAYBACK,
                json!({}),
                Expect::Err(CODE_PLAYBACK_UNAVAILABLE),
            ),
            (
                registry::RESUME_PLAYBACK,
                json!({}),
                Expect::Err(CODE_PLAYBACK_UNAVAILABLE),
            ),
            (
                registry::SET_PLAYBACK_SPEED,
                json!({"speed": 1.5}),
                Expect::Err(CODE_PLAYBACK_UNAVAILABLE),
            ),
            (
                registry::REPLAY_PREVIOUS_PARAGRAPH,
                json!({}),
                Expect::Err(CODE_PLAYBACK_UNAVAILABLE),
            ),
            (
                registry::REPLAY_PREVIOUS_PAGE,
                json!({}),
                Expect::Err(CODE_PLAYBACK_UNAVAILABLE),
            ),
            (
                registry::REPLAY_LAST_MINUTE,
                json!({}),
                Expect::Err(CODE_PLAYBACK_UNAVAILABLE),
            ),
            // seek_relative checks the EMPTY snapshot first (K10): honest
            // no-book refusal before the surface is ever touched.
            (
                registry::SEEK_RELATIVE,
                json!({"seconds": 30.0}),
                Expect::Err(CODE_NO_CONTEXT),
            ),
            // Book tools: typed no-book failures (F6 P6 wording law).
            (
                registry::JUMP_TO_CHAPTER,
                json!({"chapter": 0}),
                Expect::Err(CODE_UNKNOWN_CHAPTER),
            ),
            (
                registry::JUMP_TO_PAGE,
                json!({"page": 1}),
                Expect::Err(CODE_UNKNOWN_PAGE),
            ),
            (
                registry::SUMMARIZE_SECTION,
                json!({}),
                Expect::Err(CODE_NO_CONTEXT),
            ),
            (
                registry::GET_CONTEXT_AROUND_POSITION,
                json!({"position_ms": 0}),
                Expect::Err(CODE_NO_CONTEXT),
            ),
            // Sight: no vision port in a standalone session — honest refusal.
            (
                registry::LOOK_AT_PAGE,
                json!({}),
                Expect::Err("page_image_unavailable"),
            ),
            // Book-independent: the profile rides with or without a book.
            (
                registry::UPDATE_LEARNING_PROFILE,
                json!({"key": "pace", "value": "slower"}),
                Expect::Ok,
            ),
            // Owner-approved capability F8-D1b: the bounded-range fetch
            // refuses standalone exactly like its Tier-0 siblings.
            (
                registry::GET_CONTEXT_FOR_RANGE,
                json!({"start_position": 0, "end_position": 1000}),
                Expect::Err(CODE_NO_CONTEXT),
            ),
        ];
        assert_eq!(matrix.len(), 14, "the full §5 core-tool table");

        for (at, (name, args, _)) in matrix.iter().enumerate() {
            rig.handle(tool_call(&format!("call-{at}"), name, args.clone()))
                .await
                .expect("every outcome is an answer, never a send failure");
        }
        let results = rig.driver.tool_results();
        assert_eq!(results.len(), 14, "one ack per call, none swallowed");
        for ((name, _, expect), (call_id, result)) in matrix.iter().zip(results.iter()) {
            match (expect, result) {
                (Expect::Ok, ToolResult::Ok(_)) => {}
                (Expect::Err(code), ToolResult::Err { code: got, .. }) => {
                    assert_eq!(got, code, "{name} ({call_id}) answers its documented code");
                }
                (_, other) => panic!("{name} ({call_id}): unexpected outcome {other:?}"),
            }
        }
        // The no-book wording law rides the book tools' refusals.
        // Index 13 is F8-D1b's bounded-range fetch (owner-approved roster
        // growth): its standalone refusal carries the same wording law.
        for at in [7usize, 8, 10, 13] {
            let ToolResult::Err { message, .. } = &results[at].1 else {
                unreachable!("checked above");
            };
            assert!(
                message.contains("no book is attached"),
                "{}: standalone wording names the missing book: {message}",
                matrix[at].0
            );
        }
    }

    /// The #205 descope through the real pump: a live section anchor acks
    /// the loud `unsupported_anchor_kind`, surfaces nothing, and poisons
    /// nothing — the next live figure anchor emits normally (the anchor path
    /// never touches the Tier-1 rate window, so the refusal is budget-free
    /// by construction).
    #[tokio::test]
    async fn a_live_section_anchor_refuses_loudly_and_poisons_nothing() {
        let mut rig = rig(true).await;
        rig.handle(tool_call(
            "anchor-sec",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "section", "id": "sec-9"}),
        ))
        .await
        .expect("the refusal ack still goes out");
        assert!(
            rig.forwarded.is_empty(),
            "a refused section anchor must not move the presentation"
        );
        assert!(matches!(
            &rig.driver.tool_results()[0].1,
            ToolResult::Err { code, .. } if code == UNSUPPORTED_ANCHOR_KIND_CODE
        ));

        rig.handle(tool_call(
            "anchor-fig",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "figure", "id": "fig-3"}),
        ))
        .await
        .expect("send succeeds");
        assert_eq!(
            rig.forwarded,
            vec![ChannelEvent::TeacherAnchor {
                anchor: AnchorRef {
                    kind: AnchorKind::Figure,
                    id: "fig-3".to_string(),
                },
                gen: 0,
            }],
            "the refusal must not block later valid anchors"
        );
        assert!(matches!(&rig.driver.tool_results()[1].1, ToolResult::Ok(_)));
    }

    /// The H3 budget-free claim, pinned behaviorally: an anchor storm — five
    /// refused section anchors plus one applied figure anchor — leaves the
    /// ENTIRE Tier-1 window free (all [`TIER_ONE_MAX_CALLS`] slots still
    /// accept a playback mutation afterwards), and the window is then
    /// genuinely full (the next mutation rate-limits), so the free-window
    /// assertion is not vacuous. If anchor traffic ever starts spending
    /// Tier-1 budget — routed through the dispatcher, or charged in the
    /// unrouted arm — the fifth playback call REDs here.
    #[tokio::test]
    async fn anchor_traffic_leaves_the_whole_tier_one_window_free() {
        let mut rig = rig(true).await;
        for n in 0..TIER_ONE_MAX_CALLS {
            rig.handle(tool_call(
                &format!("anchor-sec-{n}"),
                TEACHER_ANCHOR_TOOL,
                json!({"kind": "section", "id": format!("sec-{n}")}),
            ))
            .await
            .expect("refusal acks send");
        }
        rig.handle(tool_call(
            "anchor-fig",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "figure", "id": "fig-1"}),
        ))
        .await
        .expect("send succeeds");

        let playback_calls = [
            registry::PAUSE_PLAYBACK,
            registry::RESUME_PLAYBACK,
            registry::PAUSE_PLAYBACK,
            registry::RESUME_PLAYBACK,
            registry::PAUSE_PLAYBACK,
        ];
        assert_eq!(playback_calls.len(), TIER_ONE_MAX_CALLS);
        for (n, name) in playback_calls.into_iter().enumerate() {
            rig.handle(tool_call(&format!("play-{n}"), name, json!({})))
                .await
                .expect("send succeeds");
        }
        rig.handle(tool_call("play-over", registry::RESUME_PLAYBACK, json!({})))
            .await
            .expect("the rate-limit answer still sends");

        let acks = rig.driver.tool_results();
        assert_eq!(acks.len(), 2 * TIER_ONE_MAX_CALLS + 2);
        for (id, ack) in &acks[..TIER_ONE_MAX_CALLS] {
            assert!(
                matches!(ack, ToolResult::Err { code, .. } if code == UNSUPPORTED_ANCHOR_KIND_CODE),
                "{id} must be the loud refusal, got {ack:?}"
            );
        }
        assert!(matches!(&acks[TIER_ONE_MAX_CALLS].1, ToolResult::Ok(_)));
        for (id, ack) in &acks[TIER_ONE_MAX_CALLS + 1..2 * TIER_ONE_MAX_CALLS + 1] {
            assert!(
                matches!(ack, ToolResult::Ok(_)),
                "anchor traffic spent Tier-1 budget: {id} answered {ack:?}"
            );
        }
        assert!(
            matches!(
                &acks[2 * TIER_ONE_MAX_CALLS + 1].1,
                ToolResult::Err { code, .. } if code == CODE_RATE_LIMITED
            ),
            "the window must be genuinely full afterwards, or the pin is vacuous"
        );
        assert_eq!(
            rig.forwarded.len(),
            1,
            "exactly the one applied anchor surfaced"
        );
    }

    #[tokio::test]
    async fn a_gated_live_anchor_is_normalized_surfaced_and_acked_never_unrouted() {
        let mut rig = rig(true).await;
        rig.handle(tool_call(
            "anchor-1",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "figure", "id": "fig-3"}),
        ))
        .await
        .expect("send succeeds");

        assert_eq!(
            rig.forwarded,
            vec![ChannelEvent::TeacherAnchor {
                anchor: AnchorRef {
                    kind: AnchorKind::Figure,
                    id: "fig-3".to_string(),
                },
                gen: 0,
            }],
            "the normalized event surfaces through the pump's sink"
        );
        let acks = rig.driver.tool_results();
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].0, "anchor-1");
        assert!(
            matches!(&acks[0].1, ToolResult::Ok(payload) if payload == &json!({"applied": true})),
            "the §11.1 success ack, not the dispatcher's {CODE_UNROUTED_TOOL}"
        );
    }

    #[tokio::test]
    async fn an_anchor_from_the_superseded_response_is_acked_but_never_surfaced() {
        let mut rig = rig(true).await;
        // The response was committed at the pump's opening generation (0);
        // a barge-in bumps the live counter while the anchor is in flight.
        rig.generation.bump();
        rig.handle(tool_call(
            "anchor-2",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "section", "id": "sec-9"}),
        ))
        .await
        .expect("the ack still goes out");
        assert!(
            rig.forwarded.is_empty(),
            "a superseded anchor must not move the presentation"
        );
        assert!(matches!(
            &rig.driver.tool_results()[0].1,
            ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE
        ));

        // The next turn boundary commits the new response: anchors are live
        // again under the bumped generation.
        rig.handle(ChannelEvent::UtteranceEnd).await.unwrap();
        rig.handle(tool_call(
            "anchor-3",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "table", "id": "tbl-1"}),
        ))
        .await
        .expect("send succeeds");
        assert_eq!(
            rig.forwarded,
            vec![
                ChannelEvent::UtteranceEnd,
                ChannelEvent::TeacherAnchor {
                    anchor: AnchorRef {
                        kind: AnchorKind::Table,
                        id: "tbl-1".to_string(),
                    },
                    gen: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_malformed_gated_anchor_acks_out_of_schema_without_surfacing() {
        let mut rig = rig(true).await;
        rig.handle(tool_call(
            "anchor-4",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "paragraph", "id": "p-1"}),
        ))
        .await
        .expect("send succeeds");
        assert!(rig.forwarded.is_empty());
        assert!(matches!(
            &rig.driver.tool_results()[0].1,
            ToolResult::Err { code, .. } if code == OUT_OF_SCHEMA_CODE
        ));
    }

    #[tokio::test]
    async fn an_ungated_anchor_falls_through_to_the_dispatcher_as_unknown_tool() {
        let mut rig = rig(false).await;
        rig.handle(tool_call(
            "anchor-5",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "figure", "id": "fig-3"}),
        ))
        .await
        .expect("send succeeds");
        rig.handle(tool_call("hall-1", "definitely_not_a_tool", json!({})))
            .await
            .expect("send succeeds");

        assert!(rig.forwarded.is_empty(), "nothing surfaces");
        let acks = rig.driver.tool_results();
        let (
            ToolResult::Err { code, message },
            ToolResult::Err {
                code: hc,
                message: hm,
            },
        ) = (&acks[0].1, &acks[1].1)
        else {
            panic!("both must be errors, got {acks:?}");
        };
        assert_eq!(code, CODE_UNKNOWN_TOOL);
        assert_eq!(
            (code, message),
            (hc, hm),
            "pin1 through the pump: an ungated teacher_anchor is byte-identical \
             on the wire to a hallucinated name"
        );
    }

    #[tokio::test]
    async fn a_failed_ack_send_is_recoverable_without_reexecuting_or_reemitting() {
        let mut rig = rig(true).await;
        // A playback mutation and a live anchor, both with the send armed to
        // fail: the pump surfaces the error, the answers stay cached.
        rig.driver
            .fail_next_send(ChannelError::new(ErrorKind::Transport, true, "ws drop"));
        rig.handle(tool_call(
            "seek-1",
            registry::SEEK_RELATIVE,
            json!({"seconds": 1}),
        ))
        .await
        .expect_err("the armed failure surfaces");
        assert_eq!(rig.playback.position_ms, 2500, "the seek ran once");

        rig.driver
            .fail_next_send(ChannelError::new(ErrorKind::Transport, true, "ws drop"));
        rig.handle(tool_call(
            "anchor-6",
            TEACHER_ANCHOR_TOOL,
            json!({"kind": "equation", "id": "eq-2"}),
        ))
        .await
        .expect_err("the armed failure surfaces");
        assert_eq!(
            rig.forwarded.len(),
            1,
            "the anchor event surfaced on the first (and only) normalization"
        );
        assert!(rig.driver.tool_results().is_empty(), "no ghost acks");

        // The harness's recovery: re-send everything cached, in order.
        resend_pending_answers(&mut rig.dispatcher, &mut rig.channel)
            .await
            .expect("the channel is healthy again");
        assert_eq!(rig.playback.position_ms, 2500, "still exactly one seek");
        assert_eq!(
            rig.forwarded.len(),
            1,
            "the recovery re-sends acks; it never re-emits events"
        );
        let acks = rig.driver.tool_results();
        assert_eq!(
            acks.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["seek-1", "anchor-6"],
            "both delayed acks land, arrival order kept"
        );
        assert_eq!(acks[0].1, ToolResult::Ok(json!({ "position_ms": 2500 })));
        assert_eq!(acks[1].1, ToolResult::Ok(json!({ "applied": true })));
    }

    #[tokio::test]
    async fn run_stops_on_a_send_failure_and_resumes_from_the_same_receiver() {
        let mut rig = rig(true).await;
        let (tx, mut events) = mpsc::channel(16);
        tx.try_send(tool_call("call-a", registry::PAUSE_PLAYBACK, json!({})))
            .expect("fits");
        tx.try_send(tool_call("call-b", registry::RESUME_PLAYBACK, json!({})))
            .expect("fits");
        drop(tx);

        rig.driver
            .fail_next_send(ChannelError::new(ErrorKind::Transport, true, "ws drop"));
        rig.pump
            .run(
                &mut events,
                &mut rig.channel,
                &mut rig.dispatcher,
                &mut rig.playback,
                &mut rig.excerpts,
                &mut |event| rig.forwarded.push(event),
            )
            .await
            .expect_err("the first send is armed to fail");

        resend_pending_answers(&mut rig.dispatcher, &mut rig.channel)
            .await
            .expect("recovery send lands");
        rig.pump
            .run(
                &mut events,
                &mut rig.channel,
                &mut rig.dispatcher,
                &mut rig.playback,
                &mut rig.excerpts,
                &mut |event| rig.forwarded.push(event),
            )
            .await
            .expect("the rest of the stream drains clean");

        assert_eq!(
            rig.driver
                .tool_results()
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["call-a", "call-b"],
            "one ack per call across the failure: none lost, none doubled"
        );
        assert!(!rig.playback.paused, "call-b executed after resumption");
    }
    /// The #201 end-to-end shape, deterministic twin of the live smoke's
    /// phase 4: the REAL Gemini adapter on the wire-fixture rig feeding the
    /// REAL pump. The regression this pins: after a barge-in bumped the
    /// live generation, the pump's `response_gen` could never catch up
    /// (the adapter told it nothing), so every later teacher_anchor was
    /// acked `stale_generation` and never surfaced — while the tutor's
    /// transcript claimed the preview moved. With the fix, the adapter
    /// commits each fresh response-turn open BEFORE the turn's first
    /// surfaced call, the pump re-arms, and the fresh-turn anchor
    /// surfaces. The third turn is the honest negative: a bump landing
    /// after the turn committed but before its anchor is judged stays
    /// stale — re-arming is exactly as strong as §11.1 allows, no more.
    #[tokio::test]
    async fn a_fresh_turn_anchor_after_a_barge_in_surfaces_through_the_real_gemini_adapter() {
        use std::path::PathBuf;

        use crate::voice::channel::fixture::{load_fixture, replay, WireServer};
        use crate::voice::channel::gemini::{GeminiLiveChannel, GeminiLiveConfig};
        use crate::voice::keys::store::{Backend, KeyStore};
        use crate::voice::keys::{ApiKey, KeyManager};

        let keys = {
            let store = KeyStore::with_forced_backend(
                PathBuf::from("/unused/keys.vault"),
                Backend::Session(Default::default()),
            );
            // The named provider's fixed base URL (gemini.rs GEMINI_KEY_BASE_URL).
            store
                .store(
                    "https://generativelanguage.googleapis.com",
                    &ApiKey::new("fixture-gemini-key-7d2f".into()),
                )
                .unwrap();
            std::sync::Arc::new(KeyManager::with_store(store))
        };

        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys,
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-pump-rearm");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel
            .connect(SessionContext {
                book_id: Some("pump-book".to_string()),
                session_id: "pump-session".to_string(),
                system_prompt: "tutor".to_string(),
                excerpt: DelimitedExcerpt {
                    delimited_text: "<s>x</s>".to_string(),
                },
                rolling_summary: None,
                playback_pos_ms: 0,
                tool_schemas: ToolRegistry::new(true).schemas(),
            })
            .await
            .expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");

        let mut pump = EventPump::new(generation.clone());
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(true));
        let mut playback = InMemoryPlayback::at(1500);
        let mut excerpts = selector();
        let mut forwarded = Vec::new();

        // Turn 1: fresh turn before any bump — the anchor is live at gen 0
        // (Connected, then the fix's UtteranceEnd, then the anchor call).
        async fn next(events: &mut mpsc::Receiver<ChannelEvent>) -> ChannelEvent {
            tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .expect("an event arrives within the budget")
                .expect("the event stream stays open")
        }

        for _ in 0..3 {
            let event = next(&mut events).await;
            pump.handle(
                event,
                &mut channel,
                &mut dispatcher,
                &mut playback,
                &mut excerpts,
                &mut |event| forwarded.push(event),
            )
            .await
            .expect("no send fails in this script");
        }
        assert_eq!(
            forwarded.last(),
            Some(&ChannelEvent::TeacherAnchor {
                anchor: AnchorRef {
                    kind: AnchorKind::Figure,
                    id: "fig-2".to_string(),
                },
                gen: 0,
            }),
            "phase 1: the pre-barge-in anchor surfaces at the opening generation"
        );

        // The barge-in: the documented-harmless double bump (barge_in +
        // interrupt on the live rig), 0 -> 2, while the wire's `interrupted`
        // ack retires the adapter's turn state.
        generation.bump();
        generation.bump();

        // Turn 2: the #201 regression scenario. The fresh turn's
        // UtteranceEnd re-arms the pump to the bumped generation BEFORE the
        // anchor is judged - it must surface, not ack stale. The wire's
        // `interrupted` first closes turn 1 (its anchor was answered, so no
        // answer is owed): one `TurnComplete` (review 2026-10-09, J2).
        for _ in 0..3 {
            let event = next(&mut events).await;
            pump.handle(
                event,
                &mut channel,
                &mut dispatcher,
                &mut playback,
                &mut excerpts,
                &mut |event| forwarded.push(event),
            )
            .await
            .expect("no send fails in this script");
        }
        assert_eq!(
            forwarded.last(),
            Some(&ChannelEvent::TeacherAnchor {
                anchor: AnchorRef {
                    kind: AnchorKind::Table,
                    id: "tbl-1".to_string(),
                },
                gen: 2,
            }),
            "phase 2 (#201): the fresh-turn anchor after a real barge-in \
             surfaces - the boundary re-armed the pump"
        );

        // Turn 3: the honest negative. The turn commits at gen 2, then a
        // bump lands before its anchor is judged: stale, acked, never
        // surfaced (§11.1 - re-arming must not make anchors immortal).
        // Turn 2's own end arrives first (J2): pumped like any boundary.
        let closed = next(&mut events).await;
        assert_eq!(closed, ChannelEvent::TurnComplete);
        pump.handle(
            closed,
            &mut channel,
            &mut dispatcher,
            &mut playback,
            &mut excerpts,
            &mut |event| forwarded.push(event),
        )
        .await
        .expect("no send fails");
        let boundary = next(&mut events).await;
        assert_eq!(boundary, ChannelEvent::UtteranceEnd);
        pump.handle(
            boundary,
            &mut channel,
            &mut dispatcher,
            &mut playback,
            &mut excerpts,
            &mut |event| forwarded.push(event),
        )
        .await
        .expect("no send fails");
        generation.bump();
        let stale_call = next(&mut events).await;
        pump.handle(
            stale_call,
            &mut channel,
            &mut dispatcher,
            &mut playback,
            &mut excerpts,
            &mut |event| forwarded.push(event),
        )
        .await
        .expect("the stale ack still goes out");

        replayer.await.expect(
            "the recorded fixture replays green - including the wire-visible \
             applied/stale acks the pump sent",
        );

        let surfaced: Vec<_> = forwarded
            .iter()
            .filter(|event| matches!(event, ChannelEvent::TeacherAnchor { .. }))
            .collect();
        assert_eq!(
            surfaced.len(),
            2,
            "exactly two anchors moved the presentation"
        );

        channel.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn an_unknown_anchor_acks_error_and_does_not_surface() {
        let mut rig = rig(true).await;
        rig.handle(tool_call(
            "anchor-unknown",
            TEACHER_ANCHOR_TOOL,
            serde_json::json!({"kind": "figure", "id": "fig-nonexistent"}),
        ))
        .await
        .expect("the error ack still sends");

        assert!(
            rig.forwarded.is_empty(),
            "an unknown anchor must not move the presentation"
        );
        let acks = rig.driver.tool_results();
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].0, "anchor-unknown");
        assert!(
            matches!(
                &acks[0].1,
                ToolResult::Err { code, message }
                    if code == "unknown_anchor" && message.contains("fig-nonexistent")
            ),
            "got unexpected ack: {:?}",
            acks[0].1
        );
        let nav = rig.dispatcher.drain_navigation_outcomes();
        assert_eq!(nav.len(), 1);
        assert_eq!(nav[0].tool, TEACHER_ANCHOR_TOOL);
        assert_eq!(
            nav[0].result,
            Err((
                "unknown_anchor".to_string(),
                "figure id \"fig-nonexistent\" not found in the open book".to_string()
            ))
        );
    }
}
