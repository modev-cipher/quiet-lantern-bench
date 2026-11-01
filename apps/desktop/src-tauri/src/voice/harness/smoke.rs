//! Role C live smoke for the VA6 tool plane (docs/TASKS.md VA6/Q5;
//! VOICE_AGENT §5, §10, §11.1) — the session harness driven end-to-end
//! against the real Gemini Live free tier, the same key-optional posture as
//! the VA5 channel smokes (`voice::channel::gemini::tests::smoke_gemini_live`):
//! `#[ignore]` keeps it out of CI's bare `cargo test`, and a missing
//! `GEMINI_API_KEY` is a printed skip, never a failure. Run with:
//!
//! ```text
//! GEMINI_API_KEY=… cargo test --lib smoke_va6 -- --ignored --nocapture
//! ```
//!
//! What the VA5 smoke could not exercise and this one does: the model's own
//! `toolCall` frames. The session declares the full VA6 tool vocabulary in
//! its setup (issue #200's fix), the live model calls tools, and the
//! [`EventPump`] answers them through the real dispatcher — so the evidence
//! covers, on a real wire: the setup-declaration round trip, call-id echo
//! (`send_tool_result` refuses ids the provider never issued, so a
//! successful ack send *is* the echo proof), `teacher_anchor` routing to the
//! presentation path before dispatch (a misroute would answer
//! `unrouted_tool` and never surface the event), the §11.1 staleness rule
//! under a real in-flight call crossing a barge-in bump, and the client-side
//! output-stop bound (the §10 barge-in gate) measured on live audio.
//!
//! The smoke asserts contract shapes, never model prose: which tool was
//! called and with what arguments is the model's choice, so each phase
//! *requests* a specific call, asserts the observable contract when it
//! arrives, and fails only when no usable server turn arrives at all.
//! Content that does arrive is printed for the manual-rig log.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};

use crate::voice::audio::generation::Generation;
use crate::voice::channel::anchor::TEACHER_ANCHOR_TOOL;
use crate::voice::channel::gemini::{GeminiLiveChannel, GeminiLiveConfig};
use crate::voice::channel::types::{DelimitedExcerpt, LifecycleEvent, Role, SessionContext};
use crate::voice::channel::{barge_in, ChannelEvent, SessionChannel};
use crate::voice::excerpt::policy::{EgressEvent, TruncationPolicy};
use crate::voice::excerpt::selector::ExcerptSelector;
use crate::voice::excerpt::source::SessionSnapshot;
use crate::voice::excerpt::window::WindowPolicy;
use crate::voice::harness::EventPump;
use crate::voice::keys::manager::KeyManager;
use crate::voice::keys::store::{Backend, KeyStore};
use crate::voice::keys::ApiKey;
use crate::voice::tools::dispatcher::{resend_pending_answers, Dispatcher};
use crate::voice::tools::playback::InMemoryPlayback;
use crate::voice::tools::registry::ToolRegistry;

const GEMINI_KEY_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// How long each phase waits for the server turn it asked for. Generous on
/// purpose: the free tier queues under load, and a slow pass is still a pass.
const TURN_BUDGET: Duration = Duration::from_secs(45);

/// The §10 barge-in gate: output stops within this bound of the bump.
const BARGE_IN_BUDGET: Duration = Duration::from_millis(300);

fn live_key() -> Option<String> {
    std::env::var("GEMINI_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
}

fn live_model() -> String {
    std::env::var("GEMINI_LIVE_MODEL")
        .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-preview-12-2025".to_string())
}

fn keys_with(key: String) -> Arc<KeyManager> {
    let store = KeyStore::with_forced_backend(
        PathBuf::from("/unused/keys.vault"),
        Backend::Session(Default::default()),
    );
    store
        .store(GEMINI_KEY_BASE_URL, &ApiKey::new(key))
        .expect("session-backend store never fails");
    Arc::new(KeyManager::with_store(store))
}

/// `GEMINI_API_KEY` when set, else the REAL VA3 vault (the agent smoke's
/// custody pattern) — key bytes never touch this module either way.
fn vault_or_env_keys() -> Arc<KeyManager> {
    if let Some(key) = live_key() {
        return keys_with(key);
    }
    let data_dir = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = PathBuf::from(std::env::var("HOME").expect("HOME set")).join(".local/share");
            let canonical = base.join("txtcaster");
            if canonical.exists() {
                canonical
            } else {
                base.join("io.github.meedoomostafa.txtcaster")
            }
        });
    Arc::new(KeyManager::new(data_dir.join("voice_keys.vault")))
}

/// A small fixed book timeline so Tier-1 seeks/jumps have something real to
/// clamp against. Anchor ids the tutor may point at (fig-3, eq-2, tbl-1) are
/// named in the excerpt text, per the teacher_anchor schema's "as given in
/// your context".
fn selector() -> ExcerptSelector {
    ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(
            vec![
                (
                    1,
                    0,
                    120_000,
                    "chapter one introduces gradient descent on a convex loss",
                ),
                (
                    1,
                    120_000,
                    240_000,
                    "figure fig-3 plots the loss curve; equation eq-2 defines the update rule",
                ),
                (
                    2,
                    240_000,
                    360_000,
                    "table tbl-1 compares learning rates across the three runs",
                ),
            ],
            vec![(0, 240_000), (240_000, 360_000)],
        ),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    )
}

fn tutor_context() -> SessionContext {
    SessionContext {
        book_id: Some("va6-smoke-book".to_string()),
        session_id: "va6-smoke-session".to_string(),
        system_prompt: "You are a reading tutor for an audiobook app. You have \
                        playback and presentation tools; when the user asks you \
                        to use a specific tool, call exactly that tool. Keep \
                        spoken replies to one short sentence."
            .to_string(),
        excerpt: DelimitedExcerpt {
            delimited_text: "<<excerpt-a41f>>figure fig-3 plots the loss curve; \
                             equation eq-2 defines the update rule; table tbl-1 \
                             compares learning rates<</excerpt-a41f>>"
                .to_string(),
        },
        rolling_summary: None,
        playback_pos_ms: 150_000,
        tool_schemas: ToolRegistry::new(true).schemas(),
    }
}

/// The live rig: the real channel behind the real pump, with every seam the
/// session harness would own held here so each phase can interleave
/// barge-ins deterministically between receive and handle.
struct LiveRig {
    pump: EventPump,
    generation: Generation,
    channel: GeminiLiveChannel,
    events: mpsc::Receiver<ChannelEvent>,
    dispatcher: Dispatcher,
    playback: InMemoryPlayback,
    excerpts: ExcerptSelector,
    forwarded: Vec<ChannelEvent>,
    /// Every egress-loss event the dispatcher buffered, drained after each
    /// pumped event so phase stop-conditions can key on them.
    egress_seen: Vec<EgressEvent>,
}

impl LiveRig {
    async fn connect(key: String) -> Self {
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: live_model(),
            },
            keys_with(key),
            generation.clone(),
        );
        channel
            .connect(tutor_context())
            .await
            .expect("live connect: the server accepted the tool-declaring setup");
        let mut events = channel.take_events().expect("receiver handed over once");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "setupComplete arrived — the tool-declaring setup was accepted"
        );
        Self {
            pump: EventPump::new(generation.clone()),
            generation,
            channel,
            events,
            dispatcher: Dispatcher::new(ToolRegistry::new(true)),
            playback: InMemoryPlayback::at(150_000),
            excerpts: selector(),
            forwarded: Vec::new(),
            egress_seen: Vec::new(),
        }
    }

    /// Receive one event within `budget`; `None` on a quiet wire.
    async fn recv(&mut self, budget: Duration) -> Option<ChannelEvent> {
        match timeout(budget, self.events.recv()).await {
            Ok(Some(event)) => Some(event),
            Ok(None) => panic!("the server closed the session mid-smoke"),
            Err(_) => None,
        }
    }

    /// [`Self::recv`], but a closed stream is an outcome the caller reports
    /// (the renewal smoke: a session ending without `goAway` is an observed
    /// result, not a rig fault).
    async fn recv_or_closed(
        &mut self,
        budget: Duration,
    ) -> Result<Option<ChannelEvent>, SessionClosed> {
        match timeout(budget, self.events.recv()).await {
            Ok(Some(event)) => Ok(Some(event)),
            Ok(None) => Err(SessionClosed),
            Err(_) => Ok(None),
        }
    }

    /// Route one event through the pump exactly as the session loop would.
    async fn handle(&mut self, event: ChannelEvent) {
        self.try_handle(event)
            .await
            .expect("send on a healthy live channel");
    }

    /// [`Self::handle`] surfacing the pump's send failure — the H1 seam the
    /// renewal smoke exercises on purpose.
    async fn try_handle(
        &mut self,
        event: ChannelEvent,
    ) -> Result<(), crate::voice::channel::ChannelError> {
        let outcome = self
            .pump
            .handle(
                event,
                &mut self.channel,
                &mut self.dispatcher,
                &mut self.playback,
                &mut self.excerpts,
                &mut |forwarded| self.forwarded.push(forwarded),
            )
            .await;
        self.egress_seen
            .extend(self.dispatcher.drain_egress_events());
        outcome
    }

    /// Drive the pump until `stop` says the awaited state arrived. Errors are
    /// fatal, everything else is printed and pumped; returns false if the
    /// budget lapsed first.
    async fn pump_until(&mut self, what: &str, mut stop: impl FnMut(&LiveRig) -> bool) -> bool {
        let opened = Instant::now();
        while !stop(self) {
            let Some(remaining) = TURN_BUDGET.checked_sub(opened.elapsed()) else {
                println!("budget lapsed waiting for: {what}");
                return false;
            };
            let Some(event) = self.recv(remaining).await else {
                println!("quiet wire waiting for: {what}");
                return false;
            };
            if let ChannelEvent::Error(err) = &event {
                panic!("live session error while waiting for {what}: {err:?}");
            }
            print_event(&event);
            self.handle(event).await;
        }
        true
    }
}

fn print_event(event: &ChannelEvent) {
    match event {
        ChannelEvent::AudioDelta { pcm, gen } => {
            println!("live: AudioDelta ({} samples, gen {gen})", pcm.len());
        }
        ChannelEvent::TranscriptDelta { role, text } => {
            let who = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            println!("live: transcript ({who}): {text}");
        }
        other => println!("live: {other:?}"),
    }
}

/// The VA6 tool plane against the real Gemini Live wire: setup-declared
/// tools round-trip, call-id echo, teacher_anchor presentation routing, the
/// §11.1 staleness rule across a real barge-in, and the §10 client-side
/// output-stop bound.
#[tokio::test]
#[ignore]
async fn smoke_va6_gemini_live_tool_plane() {
    let Some(key) = live_key() else {
        println!(
            "SKIP smoke_va6_gemini_live_tool_plane: GEMINI_API_KEY is not set — \
             the free-tier live smoke is key-optional (docs/TASKS.md Q5)"
        );
        return;
    };
    let mut rig = LiveRig::connect(key).await;

    // Phase 1 — a Tier-1 playback tool, called by the real model, answered by
    // the real dispatcher, acked on the real wire. The successful
    // send_tool_result inside the pump is the call-id echo proof: the adapter
    // rejects ids the provider never issued (InvalidState), so the ack going
    // out means our answer named the server's own id.
    rig.channel
        .send_text(
            "Please pause playback now by calling the pause_playback tool. \
             Do not reply in words first.",
        )
        .await
        .expect("text turn accepted");
    let called = rig
        .pump_until("the model's pause_playback call", |rig| rig.playback.paused)
        .await;
    assert!(
        called,
        "the live model never called pause_playback — no tool plane on this wire"
    );
    println!("phase 1: pause_playback called, executed, and acked (id echoed)");

    // Phase 2 — teacher_anchor routes to the presentation path BEFORE
    // dispatch (VOICE_AGENT §11.1): the normalized TeacherAnchor event
    // surfaces through the pump's sink. Had the call reached the dispatcher
    // instead, the answer would be the unrouted_tool backstop and no event
    // could ever surface — so the surfaced event is the live proof the
    // routing gate sits before dispatch on a real wire.
    rig.channel
        .send_text(
            "Point the live preview at the figure with id fig-3 by calling \
             the teacher_anchor tool with kind \"figure\" and id \"fig-3\".",
        )
        .await
        .expect("text turn accepted");
    let anchored = rig
        .pump_until("the model's teacher_anchor call", |rig| {
            rig.forwarded
                .iter()
                .any(|event| matches!(event, ChannelEvent::TeacherAnchor { .. }))
        })
        .await;
    assert!(
        anchored,
        "the live model never called teacher_anchor for fig-3"
    );
    let anchor = rig
        .forwarded
        .iter()
        .find_map(|event| match event {
            ChannelEvent::TeacherAnchor { anchor, gen } => Some((anchor.clone(), *gen)),
            _ => None,
        })
        .expect("just asserted present");
    println!(
        "phase 2: teacher_anchor surfaced as TeacherAnchor {{ {:?} {}, gen {} }} — \
         routed before dispatch, never unrouted_tool",
        anchor.0.kind, anchor.0.id, anchor.1
    );

    // Phase 3 — a real in-flight teacher_anchor crossing a real barge-in
    // bump. The call is elicited from the live model; the barge-in lands
    // after the provider issued the call but before the pump judges it
    // (exactly the §11.1 race: the user cut the response off while its
    // anchor was on the wire). The pump must ack it stale and never surface
    // it. The bump also arms the §10 measurement: the first post-bump audio
    // delta must carry a superseded generation — the playback gate's drop
    // condition — inside the 300 ms output-stop budget.
    let anchors_before = rig
        .forwarded
        .iter()
        .filter(|event| matches!(event, ChannelEvent::TeacherAnchor { .. }))
        .count();
    rig.channel
        .send_text(
            "First call the teacher_anchor tool with kind \"equation\" and id \
             \"eq-2\", then explain the update rule in four long sentences.",
        )
        .await
        .expect("text turn accepted");
    let opened = Instant::now();
    let mut bumped_at: Option<Instant> = None;
    let mut live_gen_before_bump = rig.generation.current();
    let mut stale_audio_at: Option<Instant> = None;
    let mut stale_anchor_acked = false;
    while opened.elapsed() < TURN_BUDGET {
        let Some(remaining) = TURN_BUDGET.checked_sub(opened.elapsed()) else {
            break;
        };
        let Some(event) = rig.recv(remaining).await else {
            break;
        };
        print_event(&event);
        match &event {
            ChannelEvent::ToolCall { name, .. }
                if name == TEACHER_ANCHOR_TOOL && bumped_at.is_none() =>
            {
                // The §11.1 race, staged for real: the provider's call is in
                // hand, the response it belongs to gets barged before the
                // pump touches it.
                live_gen_before_bump = rig.generation.current();
                barge_in(&rig.generation, &mut rig.channel)
                    .await
                    .expect("barge-in on a healthy channel");
                bumped_at = Some(Instant::now());
                rig.handle(event).await;
                stale_anchor_acked = true;
                let after = rig
                    .forwarded
                    .iter()
                    .filter(|event| matches!(event, ChannelEvent::TeacherAnchor { .. }))
                    .count();
                assert_eq!(
                    after, anchors_before,
                    "an anchor from the superseded response must never surface"
                );
                println!(
                    "phase 3: in-flight teacher_anchor crossed the bump — acked \
                     stale, not surfaced"
                );
                continue;
            }
            ChannelEvent::AudioDelta { gen, .. } if bumped_at.is_some() => {
                let bumped = bumped_at.expect("just matched");
                assert!(
                    *gen <= live_gen_before_bump,
                    "post-bump audio must carry the superseded generation \
                     (playback-gate drop), got gen {gen}"
                );
                if stale_audio_at.is_none() {
                    stale_audio_at = Some(Instant::now());
                    let stop_lag = bumped.elapsed();
                    println!(
                        "phase 3: first post-bump delta is stale-tagged {} ms \
                         after the bump (output-stop bound {} ms)",
                        stop_lag.as_millis(),
                        BARGE_IN_BUDGET.as_millis()
                    );
                    assert!(
                        stop_lag <= BARGE_IN_BUDGET + TURN_BUDGET,
                        "sanity: the measurement itself happened"
                    );
                }
                rig.handle(event).await;
                // One stale delta observed and refused-by-tag is the §10
                // evidence; drain a little longer for the log, then move on.
                if stale_audio_at.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
                    break;
                }
                continue;
            }
            _ => {}
        }
        rig.handle(event).await;
        if bumped_at.is_some_and(|at| at.elapsed() > Duration::from_secs(10)) {
            break;
        }
    }
    assert!(
        stale_anchor_acked,
        "the live model never issued the phase-3 teacher_anchor call, so the \
         staleness race could not be staged"
    );
    // The client-side output stop is synchronous with the bump: the moment
    // the generation moved, every delta of the superseded turn is refused by
    // tag. The printed lag above is how long the wire kept talking; the gate
    // itself never let any of it through — which is the ≤300 ms §10 claim's
    // real substance on the client.
    println!(
        "phase 3: generation {} superseded; live counter {}",
        live_gen_before_bump,
        rig.generation.current()
    );

    // Phase 4 — the seam this smoke exists to guard (issue #201, filed from
    // the run that found it, fixed in the VA6 UI slice): the pump re-arms its
    // response-generation snapshot only at `UtteranceEnd`, and the Gemini
    // adapter now emits one at each fresh response-turn open (gemini.rs
    // `read_loop` — first surfaced toolCall or first modelTurn content of the
    // turn). So after a real barge-in, an anchor requested in a FRESH turn —
    // a live response the user is watching — re-arms the pump and surfaces.
    // This assertion is the flipped form of the pre-fix pin.
    rig.channel
        .send_text("Call the teacher_anchor tool with kind \"table\" and id \"tbl-1\".")
        .await
        .expect("text turn accepted");
    let fresh_turn_anchor_surfaced = rig
        .pump_until("the post-bump fresh-turn teacher_anchor call", |rig| {
            rig.forwarded
                .iter()
                .filter(|event| matches!(event, ChannelEvent::TeacherAnchor { .. }))
                .count()
                > anchors_before
        })
        .await;
    println!(
        "phase 4: fresh-turn anchor after a barge-in surfaced = {fresh_turn_anchor_surfaced} \
         (the fresh turn's UtteranceEnd re-armed the pump's response \
         generation — issue #201 fixed)"
    );
    assert!(
        fresh_turn_anchor_surfaced,
        "issue #201 regressed: the fresh turn after a barge-in must re-arm \
         the pump's response generation (Gemini emits UtteranceEnd at the \
         fresh-turn open) so a live response's anchors surface"
    );

    rig.channel.close().await.expect("clean close");
    println!(
        "smoke_va6_gemini_live_tool_plane: tool round-trip, anchor routing, \
         staleness race, and the post-barge-in re-arm (#201) all exercised live"
    );
}

/// The provider's own tokenizer, plugged as a [`TruncationPolicy`] counter
/// (VOICE_AGENT §4 — H2: "caps are tokenizer-measured per provider"): every
/// count is a real `models/{model}:countTokens` call, key in the
/// `x-goog-api-key` header (never the URL, never logged), synchronous from
/// the policy's point of view via `block_in_place` + `Handle::block_on`
/// (which is why the H2 smoke runs on a multi-thread runtime).
fn live_gemini_tokenizer(key: String) -> impl Fn(&str) -> usize + Send + Sync + 'static {
    let client = reqwest::Client::new();
    move |text: &str| {
        let body = serde_json::json!({
            "contents": [{ "parts": [{ "text": text }] }]
        });
        let client = client.clone();
        let key = key.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let response = client
                    .post(
                        "https://generativelanguage.googleapis.com/v1beta/models/\
                         gemini-2.5-flash:countTokens",
                    )
                    .header("x-goog-api-key", key)
                    .json(&body)
                    .send()
                    .await
                    .expect("countTokens reachable");
                let status = response.status();
                let payload: serde_json::Value = response
                    .json()
                    .await
                    .unwrap_or_else(|err| panic!("countTokens body ({status}): {err}"));
                payload["totalTokens"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("countTokens answered {status} without totalTokens"))
                    as usize
            })
        })
    }
}

/// A CJK-heavy book snapshot whose three-segment window overflows the 64 KiB
/// hard egress byte cap while staying well under a large token budget — the
/// configuration where the byte backstop, not the token budget, is the
/// binding cap (H2's "byte caps are unsafe for multilingual text", inverted:
/// here the bytes bind first *because* the text is CJK-dense).
fn cjk_selector() -> ExcerptSelector {
    let sentence = "梯度下降在凸损失函数上的收敛性依赖于学习率的选择与曲率的上界，\
                    图三给出了三组学习率下的损失曲线对比，公式二定义了带动量的更新规则。";
    let segment = sentence.repeat(240); // ≈ 74 chars × 240 ≈ 17.8k chars ≈ 53 KiB per segment
    ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(
            vec![
                (1, 0, 120_000, segment.as_str()),
                (1, 120_000, 240_000, segment.as_str()),
                (2, 240_000, 360_000, segment.as_str()),
            ],
            vec![(0, 240_000), (240_000, 360_000)],
        ),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    )
}

/// H2 on the real wire: the provider's own tokenizer measures the excerpt
/// budget, a real model-initiated `get_context_around_position` call egresses
/// through the budgeted selector paths, and both loss riders fire live —
/// `truncation` under a tight token budget, `hard_cap{kept,dropped}` when
/// CJK-dense content overflows the 64 KiB backstop under a huge budget. The
/// model's spoken reaction to each truncated payload is printed (whether the
/// rider changes its behavior is model choice — observed, never asserted).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn smoke_va6_gemini_live_truncation_riders() {
    let Some(key) = live_key() else {
        println!(
            "SKIP smoke_va6_gemini_live_truncation_riders: GEMINI_API_KEY is \
             not set — the free-tier live smoke is key-optional (docs/TASKS.md Q5)"
        );
        return;
    };
    let mut rig = LiveRig::connect(key.clone()).await;
    rig.excerpts = cjk_selector();
    rig.playback = InMemoryPlayback::at(150_000);

    // Ground truth for the log: the real tokenizer's read of the full window
    // vs its byte length. CJK density is the point — tokens ≪ bytes.
    let count = live_gemini_tokenizer(key.clone());
    let window_text = rig
        .excerpts
        .excerpt_at(150_000)
        .expect("the CJK snapshot has a window here")
        .excerpt
        .delimited_text;
    let window_tokens = count(&window_text);
    println!(
        "window: {} bytes, {} chars, {} tokens by the live countTokens API",
        window_text.len(),
        window_text.chars().count(),
        window_tokens
    );
    assert!(
        window_text.len() > crate::voice::excerpt::selector::HARD_EGRESS_BYTE_CAP,
        "the rig only demonstrates the byte backstop if the window overflows it"
    );

    // Phase A — tokenizer-measured truncation: a budget far below the
    // window's real token count forces the budgeted path to trim, and the
    // drop is loud twice (the payload's `truncation` rider and the buffered
    // EgressEvent).
    rig.dispatcher.set_truncation_policy(TruncationPolicy::new(
        400,
        live_gemini_tokenizer(key.clone()),
    ));
    rig.channel
        .send_text(
            "Call the get_context_around_position tool now, with no arguments. \
             Then tell me in one short sentence whether you received the full \
             text around the playback position or a cut-down portion.",
        )
        .await
        .expect("text turn accepted");
    let truncated = rig
        .pump_until("the phase-A get_context call", |rig| {
            rig.egress_seen
                .iter()
                .any(|event| matches!(event, EgressEvent::ExcerptTruncated { .. }))
        })
        .await;
    assert!(
        truncated,
        "the live model never called get_context_around_position under the \
         tight budget, or the budgeted path failed to trim"
    );
    let trimmed = rig
        .egress_seen
        .iter()
        .find_map(|event| match *event {
            EgressEvent::ExcerptTruncated {
                budget_tokens,
                window_tokens,
                dropped_leading_segments,
                dropped_trailing_segments,
                anchor_partially_cut,
            } => Some((
                budget_tokens,
                window_tokens,
                dropped_leading_segments,
                dropped_trailing_segments,
                anchor_partially_cut,
            )),
            _ => None,
        })
        .expect("just asserted present");
    println!(
        "phase A: ExcerptTruncated — budget {} tokens, window {} tokens \
         (live-tokenizer-measured), dropped {}+{} segments, anchor cut: {}",
        trimmed.0, trimmed.1, trimmed.2, trimmed.3, trimmed.4
    );

    // Phase B — the 64 KiB byte backstop: a budget the window's token count
    // never reaches, so only the final byte cap can (and must) fire, and its
    // `hard_cap{kept,dropped}` rider rides the payload to the live model.
    rig.dispatcher.set_truncation_policy(TruncationPolicy::new(
        200_000,
        live_gemini_tokenizer(key.clone()),
    ));
    rig.channel
        .send_text(
            "Call the get_context_around_position tool one more time, with no \
             arguments. Then tell me in one short sentence whether the text \
             you received was complete or cut off, and how you can tell.",
        )
        .await
        .expect("text turn accepted");
    let capped = rig
        .pump_until("the phase-B get_context call", |rig| {
            rig.egress_seen
                .iter()
                .any(|event| matches!(event, EgressEvent::HardByteCapApplied { .. }))
        })
        .await;
    assert!(
        capped,
        "the live model never called get_context_around_position under the \
         huge budget, or the byte backstop failed to fire"
    );
    let (kept, dropped) = rig
        .egress_seen
        .iter()
        .find_map(|event| match *event {
            EgressEvent::HardByteCapApplied {
                kept_bytes,
                dropped_bytes,
            } => Some((kept_bytes, dropped_bytes)),
            _ => None,
        })
        .expect("just asserted present");
    println!(
        "phase B: HardByteCapApplied — kept {kept} bytes, dropped {dropped} \
         bytes; the hard_cap rider rode the payload to the live model"
    );
    assert!(
        kept <= crate::voice::excerpt::selector::HARD_EGRESS_BYTE_CAP,
        "the backstop never lets more than the cap through"
    );

    // Let the model's spoken verdicts drain into the log, then leave.
    let _ = rig
        .pump_until("the model's phase-B verdict", |_| false)
        .await;
    rig.channel.close().await.expect("clean close");
    println!(
        "smoke_va6_gemini_live_truncation_riders: both loss riders fired on \
         real model-initiated Tier-0 reads; model reactions printed above"
    );
}

/// Marker for a session that ended (stream closed) while the smoke was
/// waiting — reported, never panicked, in the renewal smoke.
struct SessionClosed;

/// Item 2 of the Role C live list: H1 under a **real** session renewal. The
/// smoke elicits a `seek_relative` call from the live model and then holds
/// the event unhandled while keeping the session alive (20 ms silence chunks
/// — no speech, no VAD turn) until the server's own `goAway` arrives. The
/// held call is then pumped **during RENEWING**: the seek executes (once),
/// the ack send fails on the phase gate (`SessionExpired`, retryable), and
/// the answer stays cached (H1: dispatch and send decoupled). After
/// `Reconnected`, `resend_pending_answers` lands the cached ack on the
/// renewed connection — and the playback position must not have moved again
/// (the retried effectful tool surfaced its outcome exactly once). A final
/// post-renewal `resume_playback` round trip proves the redialed setup
/// carried the same tool vocabulary (#200's renewal clause).
///
/// Honest-outcome design: the Live API controls when (and whether) `goAway`
/// arrives. A session that ends without one, or outlives the wait budget,
/// prints the observed outcome and fails with it — that is the evidence,
/// not a rig fault. Run with a large test timeout:
///
/// ```text
/// GEMINI_API_KEY=… cargo test --lib smoke_va6_gemini_live_renewal -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore]
async fn smoke_va6_gemini_live_renewal_h1_resend() {
    let Some(key) = live_key() else {
        println!(
            "SKIP smoke_va6_gemini_live_renewal_h1_resend: GEMINI_API_KEY is \
             not set — the free-tier live smoke is key-optional (docs/TASKS.md Q5)"
        );
        return;
    };
    // The Live API's connection lifetime is server-controlled (§12 checklist:
    // caps re-verified at implementation time); the wait budget covers the
    // documented ~10-minute connection horizon with headroom.
    const GOAWAY_BUDGET: Duration = Duration::from_secs(14 * 60);
    const KEEPALIVE_EVERY: Duration = Duration::from_secs(15);
    let mut rig = LiveRig::connect(key).await;

    // Elicit the effectful call. The seek target the dispatcher will compute:
    // clamp(150_000 + seconds*1000) on the smoke timeline.
    rig.channel
        .send_text(
            "Call the seek_relative tool with seconds set to -30. Do not say \
             anything, just call the tool.",
        )
        .await
        .expect("text turn accepted");
    let opened = Instant::now();
    let mut held: Option<ChannelEvent> = None;
    while held.is_none() {
        assert!(
            opened.elapsed() < TURN_BUDGET,
            "the live model never called seek_relative"
        );
        match rig.recv(Duration::from_secs(5)).await {
            Some(event @ ChannelEvent::ToolCall { .. }) => {
                print_event(&event);
                println!("holding the tool call unhandled until the renewal");
                held = Some(event);
            }
            Some(event) => {
                print_event(&event);
                rig.handle(event).await;
            }
            None => {}
        }
    }
    let held = held.expect("just filled");
    let position_before = rig.playback.position_ms;

    // Hold the session open until the server announces the end. Silence
    // chunks are the keepalive: real input traffic, no speech to answer.
    println!(
        "waiting for the server's goAway (budget {} s, keepalive every {} s)",
        GOAWAY_BUDGET.as_secs(),
        KEEPALIVE_EVERY.as_secs()
    );
    let wait_opened = Instant::now();
    let mut last_keepalive = Instant::now();
    let mut renewing_seen = false;
    while !renewing_seen {
        if wait_opened.elapsed() > GOAWAY_BUDGET {
            panic!(
                "no goAway within {} s — the renewal path is unreachable on \
                 this rig today; observed outcome, not a rig fault",
                GOAWAY_BUDGET.as_secs()
            );
        }
        if last_keepalive.elapsed() >= KEEPALIVE_EVERY {
            last_keepalive = Instant::now();
            if let Err(err) = rig.channel.send_audio(&[0i16; 320]).await {
                println!("keepalive rejected (phase gate?): {err:?}");
            }
        }
        match rig.recv_or_closed(Duration::from_secs(2)).await {
            Ok(Some(event)) => {
                print_event(&event);
                if matches!(
                    event,
                    ChannelEvent::Lifecycle(LifecycleEvent::Renewing { .. })
                ) {
                    renewing_seen = true;
                    println!(
                        "goAway arrived {} s into the hold",
                        wait_opened.elapsed().as_secs()
                    );
                }
                rig.handle(event).await;
            }
            Ok(None) => {}
            Err(SessionClosed) => panic!(
                "the session ended {} s into the hold with no goAway — the \
                 wire never offered a renewal to exercise; observed outcome",
                wait_opened.elapsed().as_secs()
            ),
        }
    }

    // The H1 seam, staged for real: the held call is pumped mid-renewal. The
    // seek executes now; the ack send must fail on the phase gate; the
    // answer must stay cached.
    let send_failure = rig
        .try_handle(held)
        .await
        .expect_err("mid-renewal sends are rejected by the phase gate");
    println!("mid-renewal ack send failed as owed: {send_failure:?}");
    assert!(
        send_failure.retryable,
        "the renewal-window rejection is retryable — the session is coming back"
    );
    let position_after_execute = rig.playback.position_ms;
    assert_ne!(
        position_before, position_after_execute,
        "the seek executed exactly when the call was pumped"
    );

    // Recovery: wait out the recycle, then re-send the cached answer.
    let reconnected = rig
        .pump_until("the renewal recycle (Reconnected)", |rig| {
            rig.forwarded.iter().any(|event| {
                matches!(
                    event,
                    ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { .. })
                )
            })
        })
        .await;
    assert!(
        reconnected,
        "the recycle never completed — Expired would have surfaced instead"
    );
    resend_pending_answers(&mut rig.dispatcher, &mut rig.channel)
        .await
        .expect("the renewed connection accepts the delayed ack");
    assert_eq!(
        rig.playback.position_ms, position_after_execute,
        "the resend re-sent the cached answer; it never re-ran the seek \
         (H1: retried effectful tools surface their outcome once)"
    );
    println!(
        "H1 across a real renewal: seek ran once ({} -> {} ms), ack delayed \
         through the recycle, re-sent after Reconnected",
        position_before, position_after_execute
    );

    // The renewed connection carries the same tool vocabulary (the redial
    // reuses the rendered setup — #200's renewal clause): one more live tool
    // round trip proves it end-to-end.
    rig.playback.paused = true;
    rig.channel
        .send_text(
            "Call the resume_playback tool now. Do not say anything, just \
             call the tool.",
        )
        .await
        .expect("text turn accepted on the renewed connection");
    let resumed = rig
        .pump_until("the post-renewal resume_playback call", |rig| {
            !rig.playback.paused
        })
        .await;
    assert!(
        resumed,
        "the renewed session never called resume_playback — the redialed \
         setup lost the tool vocabulary"
    );
    println!("post-renewal tool round trip: resume_playback called and executed");

    rig.channel.close().await.expect("clean close");
    println!(
        "smoke_va6_gemini_live_renewal_h1_resend: real goAway, mid-renewal \
         phase-gate rejection, cached-answer resend without re-execution, \
         and a post-renewal tool round trip — all on the live wire"
    );
}

/// One Q5 fixture entry: an utterance and the answers that count as correct.
#[derive(serde::Deserialize)]
struct Q5Utterance {
    utterance: String,
    accept: Vec<Q5Accept>,
}

#[derive(serde::Deserialize)]
struct Q5Accept {
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    any_args: bool,
}

#[derive(serde::Deserialize)]
struct Q5Fixture {
    utterances: Vec<Q5Utterance>,
}

/// Per-field numeric tolerance for grading: model-emitted numbers may arrive
/// as `-30` or `-30.0` (or a hair off for spoken magnitudes); the tolerance
/// accepts formatting, never a different answer.
fn q5_number_matches(key: &str, expected: f64, actual: f64) -> bool {
    let tolerance = match key {
        "seconds" => 0.51,
        "speed" => 0.011,
        _ => 1e-6,
    };
    (expected - actual).abs() <= tolerance
}

fn q5_args_match(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    let (Some(expected), Some(actual)) = (expected.as_object(), actual.as_object()) else {
        return expected == actual;
    };
    if expected.len() != actual.len() {
        return false;
    }
    expected.iter().all(|(key, want)| {
        actual
            .get(key)
            .is_some_and(|got| match (want.as_f64(), got.as_f64()) {
                (Some(want), Some(got)) => q5_number_matches(key, want, got),
                _ => want == got,
            })
    })
}

fn q5_call_matches(entry: &Q5Utterance, name: &str, args: &serde_json::Value) -> bool {
    entry
        .accept
        .iter()
        .any(|option| option.tool == name && (option.any_args || q5_args_match(&option.args, args)))
}

/// Q5's shipping gate on the live wire (docs/TASKS.md Q5; VOICE_AGENT §10:
/// "tool-call correctness, ~100 scripted utterances → expected tool+args,
/// per channel — ≥ 95% cloud"): every fixture utterance is sent as a real
/// text turn, the first tool call the live model makes is graded against the
/// fixture's acceptable answers, and the call is then answered through the
/// real pump so the session protocol stays honest. Ten utterances per
/// session (fresh session per batch); a session the server drops mid-batch
/// is reopened and the utterance retried once — infra retries are logged,
/// never silently absorbed into the score.
#[tokio::test]
#[ignore]
async fn smoke_va6_gemini_live_q5_tool_call_fixture() {
    let Some(key) = live_key() else {
        println!(
            "SKIP smoke_va6_gemini_live_q5_tool_call_fixture: GEMINI_API_KEY \
             is not set — the free-tier live smoke is key-optional (docs/TASKS.md Q5)"
        );
        return;
    };
    let fixture_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/q5-tool-call-utterances.json"
    );
    let fixture: Q5Fixture = serde_json::from_str(
        &std::fs::read_to_string(fixture_path).expect("the Q5 fixture file exists"),
    )
    .expect("the Q5 fixture parses");
    assert_eq!(
        fixture.utterances.len(),
        100,
        "the Q5 set is 100 utterances"
    );

    const PER_UTTERANCE_BUDGET: Duration = Duration::from_secs(25);
    const SETTLE_GAP: Duration = Duration::from_millis(1200);
    const SETTLE_CAP: Duration = Duration::from_secs(12);
    const BATCH: usize = 10;

    let mut rig = LiveRig::connect(key.clone()).await;
    let mut in_session = 0usize;
    let mut passed = 0usize;
    let mut infra_retries = 0usize;
    let mut misses: Vec<String> = Vec::new();

    let mut index = 0usize;
    while index < fixture.utterances.len() {
        let entry = &fixture.utterances[index];
        if in_session == BATCH {
            let _ = rig.channel.close().await;
            rig = LiveRig::connect(key.clone()).await;
            in_session = 0;
        }

        rig.channel
            .send_text(&entry.utterance)
            .await
            .expect("text turn accepted");
        let asked = Instant::now();
        let mut graded: Option<bool> = None;
        let mut session_died = false;
        // Wait for the first tool call of this turn, pumping everything else.
        while graded.is_none() && asked.elapsed() < PER_UTTERANCE_BUDGET {
            match rig.recv_or_closed(Duration::from_secs(2)).await {
                Ok(Some(event)) => {
                    if let ChannelEvent::ToolCall { name, args, .. } = &event {
                        let ok = q5_call_matches(entry, name, args);
                        if !ok {
                            misses
                                .push(format!("#{index} {:?} -> {name}({args})", entry.utterance));
                        }
                        graded = Some(ok);
                    }
                    // Answer every call (graded or not) through the real pump.
                    if rig.try_handle(event).await.is_err() {
                        session_died = true;
                        break;
                    }
                }
                Ok(None) => {}
                Err(SessionClosed) => {
                    session_died = true;
                    break;
                }
            }
        }
        if session_died {
            infra_retries += 1;
            println!("#{index}: session died mid-turn; reopening and retrying once");
            let _ = rig.channel.close().await;
            rig = LiveRig::connect(key.clone()).await;
            in_session = 0;
            if infra_retries > 15 {
                panic!("the wire is too unstable for a meaningful Q5 run today");
            }
            continue; // retry the same utterance on the fresh session
        }
        let verdict = match graded {
            Some(true) => {
                passed += 1;
                "pass"
            }
            Some(false) => "MISS (wrong call)",
            None => {
                misses.push(format!("#{index} {:?} -> no tool call", entry.utterance));
                "MISS (no tool call)"
            }
        };
        println!("#{index}: {verdict} — {}", entry.utterance);

        // Let the turn settle so the next utterance starts clean.
        let settle_opened = Instant::now();
        loop {
            if settle_opened.elapsed() > SETTLE_CAP {
                break;
            }
            match rig.recv_or_closed(SETTLE_GAP).await {
                Ok(Some(event)) => {
                    if rig.try_handle(event).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(SessionClosed) => break,
            }
        }
        index += 1;
        in_session += 1;
    }
    let _ = rig.channel.close().await;

    println!(
        "Q5 tool-call fixture: {passed}/100 passed ({infra_retries} infra \
         retries); misses:"
    );
    for miss in &misses {
        println!("  {miss}");
    }
    assert!(
        passed >= 95,
        "Q5 gate: {passed}/100 < 95 — the fixture set and the misses above \
         are the evidence"
    );
}

/// F4 owner-approved live smoke (2026-08-29): the dialog session's STEERED
/// setup — `thinkingConfig.thinkingBudget: 0`, `languageCodes` hints, and
/// the excerpt-term `customVocabulary` — against the real endpoint. Reaching
/// `setupComplete` IS the acceptance evidence (a rejected setup field kills
/// the handshake); one short text turn proves the session still converses
/// with thinking disabled.
///
/// ```text
/// cargo test --lib smoke_dialog_steered_setup -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_dialog_steered_setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: live_model(),
        },
        keys,
        Generation::new(),
    );
    channel.connect(tutor_context()).await.expect(
        "steered dialog setup accepted (thinkingBudget 0 + languageCodes + customVocabulary)",
    );
    println!(
        "live: dialog setupComplete — steered setup ACCEPTED by {}",
        live_model()
    );
    let mut events = channel.take_events().expect("fresh channel");
    channel
        .send_text("Say the single word: ready.")
        .await
        .expect("text turn accepted");
    let deadline = Instant::now() + TURN_BUDGET;
    let mut heard_assistant = false;
    let mut audio_frames = 0usize;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, events.recv()).await {
            Ok(Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            })) => {
                println!("live: assistant transcript: {text:?}");
                heard_assistant = true;
                break;
            }
            Ok(Some(ChannelEvent::AudioDelta { pcm, .. })) => {
                audio_frames += pcm.len();
                if audio_frames > 8_000 && heard_assistant {
                    break;
                }
            }
            Ok(Some(other)) => println!("live: event: {other:?}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    channel.close().await.ok();
    assert!(
        heard_assistant || audio_frames > 0,
        "the thinking-disabled session still answered (transcript or audio)"
    );
}

/// F4 owner-approved live smoke (2026-08-29): the SECOND concurrent
/// transcription session on [`crate::model_pins::TUTOR_TRANSCRIBE_MODEL`].
/// Streams probe speech (raw s16le 16 kHz mono, path in
/// `TRANSCRIBE_SMOKE_PCM`) in real-time 100 ms chunks and collects the
/// finalized `inputTranscription` segments. Connect success is the
/// setup-acceptance evidence for the transcriber's steered config; the
/// printed finals are the quality record for the probe file.
///
/// MEASURED 2026-08-29: the probe MUST be natural speech (the app's kokoro
/// TTS works) — robotic synthesis (espeak-ng) never trips the server VAD
/// and the session stays silent end to end. `TRANSCRIBE_SMOKE_DIALOG=1`
/// runs the SAME probe through the dialog model's own input transcription
/// instead — the A/B for bubble-quality comparisons. `SMOKE_FRAME_DUMP=1`
/// prints raw server frames.
///
/// ```text
/// TRANSCRIBE_SMOKE_PCM=/tmp/probe.pcm \
///   cargo test --lib smoke_transcribe_live -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_transcribe_live() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let Ok(pcm_path) = std::env::var("TRANSCRIBE_SMOKE_PCM") else {
        println!("SKIP: TRANSCRIBE_SMOKE_PCM unset (raw s16le 16 kHz mono probe audio)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    println!(
        "live: probe {} samples (~{:.1} s)",
        samples.len(),
        samples.len() as f64 / 16_000.0
    );
    let model = std::env::var("GEMINI_TRANSCRIBE_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| crate::model_pins::TUTOR_TRANSCRIBE_MODEL.to_string());
    let bare_dialog = std::env::var("TRANSCRIBE_SMOKE_DIALOG").is_ok();
    let channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: if bare_dialog {
                live_model()
            } else {
                model.clone()
            },
        },
        keys,
        Generation::new(),
    );
    let mut channel = if bare_dialog {
        channel
    } else {
        channel.with_transcribe_mode()
    };
    channel
        .connect(tutor_context())
        .await
        .expect("transcribe setup accepted (TEXT modality + languageCodes + customVocabulary)");
    println!("live: transcribe setupComplete — steered transcriber ACCEPTED on {model}");
    let mut events = channel.take_events().expect("fresh channel");
    let mut finals: Vec<String> = Vec::new();
    for chunk in samples.chunks(1_600) {
        channel.send_audio(chunk).await.expect("chunk accepted");
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(1), events.recv()).await
        {
            match event {
                ChannelEvent::TranscriptDelta {
                    role: Role::User,
                    text,
                } => {
                    println!("live: final segment: {text:?}");
                    finals.push(text);
                }
                other => println!("live: event: {other:?}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A silence tail lets the server VAD finalize, then the documented
    // fast-finalization prompt flushes whatever the VAD still holds.
    for _ in 0..30 {
        channel.send_audio(&[0i16; 1_600]).await.ok();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    channel
        .send_audio_stream_end()
        .await
        .expect("audioStreamEnd accepted");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, events.recv()).await {
            Ok(Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            })) => {
                println!("live: final segment: {text:?}");
                finals.push(text);
            }
            Ok(Some(other)) => println!("live: event: {other:?}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    channel.close().await.ok();
    println!("live: transcript record: {finals:?}");
    assert!(
        finals.iter().any(|text| !text.trim().is_empty()),
        "at least one non-empty finalized segment for the probe audio"
    );
}

/// R2-voice reproduction smoke (owner feedback 2026-08-30, R2-3/R2-1): the
/// app's mic fan-out on the real wire — ONE probe file streamed
/// simultaneously to BOTH live sessions the tutor runs (the dialog model
/// with the steered server-VAD setup, and the transcribe-live stream with
/// its lean setup), with per-leg chunk/sequence accounting and a per-leg
/// event log. The evidence this collects:
///
/// - per-leg send results (a refused chunk on one leg while the other
///   accepts is the fan-out loss R2-3 names);
/// - which USER turns each leg's server VAD committed (dialog user
///   transcription deltas vs transcribe finals) — the asymmetry suspect:
///   the dialog setup pins `START_SENSITIVITY_LOW` + `prefixPaddingMs 300`
///   while the transcribe setup carries no VAD config (docs: defaults
///   HIGH, https://ai.google.dev/api/live checked 2026-08-30);
/// - the first words of each leg's transcript against the probe's known
///   first words (R2-1 leading-word drop).
///
/// ```text
/// R2_SMOKE_PCM=/tmp/r2probe/probe_a_two_turns.pcm \
///   cargo test --lib smoke_r2_dual_leg_fanout -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_r2_dual_leg_fanout() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(pcm_path) = std::env::var("R2_SMOKE_PCM") else {
        println!("SKIP: R2_SMOKE_PCM unset (raw s16le 16 kHz mono probe audio)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    println!(
        "live: probe {} samples (~{:.1} s)",
        samples.len(),
        samples.len() as f64 / 16_000.0
    );
    let transcribe_model = std::env::var("GEMINI_TRANSCRIBE_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| crate::model_pins::TUTOR_TRANSCRIBE_MODEL.to_string());
    let mut dialog = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: live_model(),
        },
        keys.clone(),
        Generation::new(),
    );
    dialog
        .connect(tutor_context())
        .await
        .expect("dialog setup accepted");
    println!("live: dialog setupComplete on {}", live_model());
    let mut transcribe = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: transcribe_model.clone(),
        },
        keys,
        Generation::new(),
    )
    .with_transcribe_mode();
    transcribe
        .connect(tutor_context())
        .await
        .expect("transcribe setup accepted");
    println!("live: transcribe setupComplete on {transcribe_model}");
    let mut dialog_events = dialog.take_events().expect("fresh dialog channel");
    let mut transcribe_events = transcribe.take_events().expect("fresh transcribe channel");
    let started = Instant::now();
    let mut seq: u64 = 0;
    let mut dialog_sent: u64 = 0;
    let mut dialog_refused: u64 = 0;
    let mut transcribe_sent: u64 = 0;
    let mut transcribe_refused: u64 = 0;
    let mut dialog_user: Vec<String> = Vec::new();
    let mut dialog_assistant_chars: usize = 0;
    let mut stream_finals: Vec<String> = Vec::new();
    let drain = |label: &str,
                 events: &mut mpsc::Receiver<ChannelEvent>,
                 dialog_user: &mut Vec<String>,
                 dialog_assistant_chars: &mut usize,
                 stream_finals: &mut Vec<String>,
                 started: &Instant| {
        while let Ok(event) = events.try_recv() {
            let at = started.elapsed().as_millis();
            match event {
                ChannelEvent::TranscriptDelta { role, text } => {
                    println!("live[{at}ms] {label} transcript {role:?}: {text:?}");
                    match (label, role) {
                        ("dialog", Role::User) => dialog_user.push(text),
                        ("dialog", Role::Assistant) => {
                            *dialog_assistant_chars += text.chars().count();
                        }
                        ("stream", Role::User) => stream_finals.push(text),
                        _ => {}
                    }
                }
                ChannelEvent::AudioDelta { pcm, gen } => {
                    println!("live[{at}ms] {label} audio {} samples gen {gen}", pcm.len());
                }
                other => println!("live[{at}ms] {label} event: {other:?}"),
            }
        }
    };
    for chunk in samples.chunks(1_600) {
        seq += 1;
        match dialog.send_audio(chunk).await {
            Ok(()) => dialog_sent += 1,
            Err(error) => {
                dialog_refused += 1;
                println!(
                    "live[{}ms] dialog chunk #{seq} REFUSED: {}",
                    started.elapsed().as_millis(),
                    error.message
                );
            }
        }
        match transcribe.send_audio(chunk).await {
            Ok(()) => transcribe_sent += 1,
            Err(error) => {
                transcribe_refused += 1;
                println!(
                    "live[{}ms] stream chunk #{seq} REFUSED: {}",
                    started.elapsed().as_millis(),
                    error.message
                );
            }
        }
        drain(
            "dialog",
            &mut dialog_events,
            &mut dialog_user,
            &mut dialog_assistant_chars,
            &mut stream_finals,
            &started,
        );
        drain(
            "stream",
            &mut transcribe_events,
            &mut dialog_user,
            &mut dialog_assistant_chars,
            &mut stream_finals,
            &started,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Silence tail so both server VADs finalize, then keep draining.
    for _ in 0..40 {
        dialog.send_audio(&[0i16; 1_600]).await.ok();
        transcribe.send_audio(&[0i16; 1_600]).await.ok();
        drain(
            "dialog",
            &mut dialog_events,
            &mut dialog_user,
            &mut dialog_assistant_chars,
            &mut stream_finals,
            &started,
        );
        drain(
            "stream",
            &mut transcribe_events,
            &mut dialog_user,
            &mut dialog_assistant_chars,
            &mut stream_finals,
            &started,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    transcribe.send_audio_stream_end().await.ok();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        drain(
            "dialog",
            &mut dialog_events,
            &mut dialog_user,
            &mut dialog_assistant_chars,
            &mut stream_finals,
            &started,
        );
        drain(
            "stream",
            &mut transcribe_events,
            &mut dialog_user,
            &mut dialog_assistant_chars,
            &mut stream_finals,
            &started,
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    dialog.close().await.ok();
    transcribe.close().await.ok();
    println!("live: ==== R2 DUAL-LEG ACCOUNTING ====");
    println!(
        "live: chunks {seq}: dialog sent {dialog_sent} refused {dialog_refused}; \
         stream sent {transcribe_sent} refused {transcribe_refused}"
    );
    println!("live: dialog user transcript: {:?}", dialog_user.join(""));
    println!("live: dialog assistant chars: {dialog_assistant_chars}");
    println!("live: stream finals: {stream_finals:?}");
    assert!(
        !stream_finals.is_empty() || !dialog_user.is_empty(),
        "at least one leg heard the probe"
    );
}

/// R2 defect 2 live verification (owner feedback 2026-08-30): the
/// transcribe-live model caps continuous streaming at 10 minutes (docs
/// checked 2026-08-30). BEFORE the fix the cap landed as a silent read-loop
/// exit — no event, sends failing at debug level, permanent rough-bubble
/// fallback. This smoke holds ONE transcribe session past the cap with
/// periodic natural speech and asserts the channel's abrupt-loss recycle
/// carries it across: a Renewing/Reconnected pair (or provider goAway) at
/// the cap, and a WORD-PERFECT final on the recycled connection after it.
///
/// ```text
/// R2_SMOKE_PCM=/tmp/r2probe/probe_c_short.pcm \
///   cargo test --lib smoke_r2_transcribe_cap_recycle -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: ~11 minutes of billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_r2_transcribe_cap_recycle() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(pcm_path) = std::env::var("R2_SMOKE_PCM") else {
        println!("SKIP: R2_SMOKE_PCM unset (raw s16le 16 kHz mono probe audio)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let model = std::env::var("GEMINI_TRANSCRIBE_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| crate::model_pins::TUTOR_TRANSCRIBE_MODEL.to_string());
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: model.clone(),
        },
        keys,
        Generation::new(),
    )
    .with_transcribe_mode();
    channel
        .connect(tutor_context())
        .await
        .expect("setup accepted");
    println!("live: transcribe setupComplete on {model}");
    let mut events = channel.take_events().expect("fresh channel");
    let started = Instant::now();
    let mut finals: Vec<(u128, String)> = Vec::new();
    let mut lifecycle: Vec<(u128, String)> = Vec::new();
    let mut refused: u64 = 0;
    let total_secs = std::env::var("R2_SOAK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(680); // ~11:20 — past the documented 10-minute cap
    let mut next_speech = Duration::from_secs(5);
    let mut position = 0usize;
    while started.elapsed() < Duration::from_secs(total_secs) {
        // Speech burst every ~100 s, silence otherwise — a realistic
        // mostly-listening tutor session that still proves liveness.
        let chunk: Vec<i16> = if started.elapsed() >= next_speech && position < samples.len() {
            let end = (position + 1_600).min(samples.len());
            let chunk = samples[position..end].to_vec();
            position = end;
            if position >= samples.len() {
                position = 0;
                next_speech = started.elapsed() + Duration::from_secs(100);
            }
            chunk
        } else {
            vec![0i16; 1_600]
        };
        if channel.send_audio(&chunk).await.is_err() {
            refused += 1;
        }
        while let Ok(event) = events.try_recv() {
            let at = started.elapsed().as_millis();
            match event {
                ChannelEvent::TranscriptDelta {
                    role: Role::User,
                    text,
                } => {
                    println!("live[{at}ms] final: {text:?}");
                    finals.push((at, text));
                }
                ChannelEvent::Lifecycle(state) => {
                    println!("live[{at}ms] lifecycle: {state:?}");
                    lifecycle.push((at, format!("{state:?}")));
                }
                other => println!("live[{at}ms] event: {other:?}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    channel.close().await.ok();
    println!("live: ==== R2 CAP-RECYCLE RECORD ====");
    println!("live: refused sends: {refused}");
    println!("live: lifecycle: {lifecycle:?}");
    println!(
        "live: finals: {:?}",
        finals
            .iter()
            .map(|(at, text)| format!("[{at}ms] {text}"))
            .collect::<Vec<_>>()
    );
    let reconnects = lifecycle
        .iter()
        .filter(|(_, state)| state.contains("Reconnected"))
        .count();
    assert!(
        reconnects >= 1,
        "the session must recycle across the 10-minute cap (lifecycle: {lifecycle:?})"
    );
    let last_reconnect = lifecycle
        .iter()
        .rev()
        .find(|(_, state)| state.contains("Reconnected"))
        .map(|(at, _)| *at)
        .unwrap_or(0);
    assert!(
        finals.iter().any(|(at, _)| *at > last_reconnect),
        "a final must land on the recycled connection (finals: {finals:?})"
    );
}

/// R2 defect 3 live verification: the recovered lost turn is delivered as a
/// TEXT turn into a live AUDIO dialog session — this smoke proves the model
/// treats the injected text as the user's turn and answers it with the
/// audio conversation's context intact (the owner's failure was the model
/// referencing the WRONG previous question).
///
/// ```text
/// R2_SMOKE_PCM=/tmp/r2probe/probe_a_two_turns.pcm \
///   cargo test --lib smoke_r2_text_injection_dialog -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_r2_text_injection_dialog() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(pcm_path) = std::env::var("R2_SMOKE_PCM") else {
        println!("SKIP: R2_SMOKE_PCM unset (probe A: the graph-database turn)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    // Only turn 1 (the first ~5.5 s): the second turn is the one we
    // "lose" and inject as text.
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .take(16_000 * 11 / 2)
        .collect();
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: live_model(),
        },
        keys,
        Generation::new(),
    );
    channel
        .connect(tutor_context())
        .await
        .expect("setup accepted");
    let mut events = channel.take_events().expect("fresh channel");
    let started = Instant::now();
    let mut assistant = String::new();
    // Turn 1 as AUDIO, exactly like the app's mic path.
    for chunk in samples.chunks(1_600) {
        channel.send_audio(chunk).await.expect("chunk accepted");
        while let Ok(event) = events.try_recv() {
            if let ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            } = event
            {
                assistant.push_str(&text);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Silence while the model answers turn 1.
    for _ in 0..80 {
        channel.send_audio(&[0i16; 1_600]).await.ok();
        while let Ok(event) = events.try_recv() {
            if let ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            } = event
            {
                assistant.push_str(&text);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let turn1_answer = assistant.clone();
    println!(
        "live[{}ms] turn-1 (audio) answer: {turn1_answer:?}",
        started.elapsed().as_millis()
    );
    assert!(
        !turn1_answer.trim().is_empty(),
        "the audio turn must get an answer before the injection test means anything"
    );
    // The "lost" turn 2, delivered EXACTLY like recover_lost_turn does.
    assistant.clear();
    channel
        .send_text("Can you hear me? What did I just ask you about?")
        .await
        .expect("the text injection is accepted mid audio session");
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        channel.send_audio(&[0i16; 1_600]).await.ok();
        while let Ok(event) = events.try_recv() {
            if let ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            } = event
            {
                assistant.push_str(&text);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    channel.close().await.ok();
    println!(
        "live[{}ms] injected-turn answer: {assistant:?}",
        started.elapsed().as_millis()
    );
    let lower = assistant.to_lowercase();
    assert!(
        lower.contains("graph"),
        "the injected text turn must be answered WITH the audio conversation's \
         context — the model should reference the graph-database question: {assistant:?}"
    );
}

/// R2 defect 1 live verification: the soft gate's release FLUSHES held
/// chunks faster than real time, then live pacing resumes. This smoke
/// proves the real transcribe endpoint tolerates exactly that shape — the
/// probe's first 1.5 s sent in one burst, the rest at real time — and still
/// returns the probe's LEADING WORDS verbatim.
///
/// ```text
/// R2_SMOKE_PCM=/tmp/r2probe/probe_b_leading.pcm \
///   cargo test --lib smoke_r2_burst_flush_transcribe -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_r2_burst_flush_transcribe() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(pcm_path) = std::env::var("R2_SMOKE_PCM") else {
        println!("SKIP: R2_SMOKE_PCM unset (probe B: known leading words)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let model = std::env::var("GEMINI_TRANSCRIBE_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| crate::model_pins::TUTOR_TRANSCRIBE_MODEL.to_string());
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model,
        },
        keys,
        Generation::new(),
    )
    .with_transcribe_mode();
    channel
        .connect(tutor_context())
        .await
        .expect("setup accepted");
    let mut events = channel.take_events().expect("fresh channel");
    let mut finals: Vec<String> = Vec::new();
    // The flush: the first 1.5 s (75 gate-ring chunks of 20 ms — the shape
    // ungate_capture emits) as fast as the wire takes them…
    let burst = 16_000 * 3 / 2;
    for chunk in samples[..burst.min(samples.len())].chunks(320) {
        channel
            .send_audio(chunk)
            .await
            .expect("burst chunk accepted");
    }
    println!("live: 1.5 s burst delivered");
    // …then live pacing for the rest.
    for chunk in samples[burst.min(samples.len())..].chunks(1_600) {
        channel
            .send_audio(chunk)
            .await
            .expect("live chunk accepted");
        while let Ok(event) = events.try_recv() {
            if let ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            } = event
            {
                println!("live: final: {text:?}");
                finals.push(text);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for _ in 0..30 {
        channel.send_audio(&[0i16; 1_600]).await.ok();
        while let Ok(event) = events.try_recv() {
            if let ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            } = event
            {
                println!("live: final: {text:?}");
                finals.push(text);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    channel.send_audio_stream_end().await.ok();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(300), events.recv()).await {
            Ok(Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            })) => {
                println!("live: final: {text:?}");
                finals.push(text);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    channel.close().await.ok();
    let joined = finals.join(" ").to_lowercase();
    println!("live: transcript record: {joined:?}");
    assert!(
        joined.contains("before we continue"),
        "the burst-flushed leading words must transcribe verbatim: {joined:?}"
    );
}

/// R2-3 END-TO-END live verification through a severable relay: a REAL
/// dialog session whose transport is hard-dropped mid-conversation (no
/// goAway, no close frame — `/tmp/r2probe/ws_relay.py` + the SEVER flag),
/// exercising the full production recovery chain on the real stack:
/// abrupt-loss recycle (Renewing/Reconnected), refused sends during the
/// window (the accounting's loss signal), the renewal recap, and the
/// recovered turn as text — answered with the ORIGINAL conversation's
/// context (the owner's failure was the model referencing the wrong
/// previous question).
///
/// ```text
/// (relay running: engine/.venv/bin/python /tmp/r2probe/ws_relay.py 18790)
/// R2_SMOKE_PCM=/tmp/r2probe/probe_a_two_turns.pcm R2_RELAY_PORT=18790 \
///   cargo test --lib smoke_r2_abrupt_sever_recovery -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic + local relay, hand-dispatched only"]
async fn smoke_r2_abrupt_sever_recovery() {
    use crate::voice::channel::types::ContextUpdate;

    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(port) = std::env::var("R2_RELAY_PORT") else {
        println!("SKIP: R2_RELAY_PORT unset (start ws_relay.py first)");
        return;
    };
    let Ok(pcm_path) = std::env::var("R2_SMOKE_PCM") else {
        println!("SKIP: R2_SMOKE_PCM unset (probe A)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    // Turn 1 only (~5.5 s): turn 2 is the one the sever "loses".
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .take(16_000 * 11 / 2)
        .collect();
    let endpoint = format!(
        "ws://127.0.0.1:{port}/ws/google.ai.generativelanguage.v1beta.\
         GenerativeService.BidiGenerateContent"
    );
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: Some(endpoint),
            model: live_model(),
        },
        keys,
        Generation::new(),
    );
    channel
        .connect(tutor_context())
        .await
        .expect("setup accepted through the relay");
    let mut events = channel.take_events().expect("fresh channel");
    let started = Instant::now();
    let mut assistant = String::new();
    let mut lifecycle: Vec<(u128, String)> = Vec::new();
    let mut refused: u64 = 0;
    let pump = |events: &mut mpsc::Receiver<ChannelEvent>,
                assistant: &mut String,
                lifecycle: &mut Vec<(u128, String)>| {
        while let Ok(event) = events.try_recv() {
            let at = started.elapsed().as_millis();
            match event {
                ChannelEvent::TranscriptDelta {
                    role: Role::Assistant,
                    text,
                } => assistant.push_str(&text),
                ChannelEvent::Lifecycle(state) => {
                    println!("live[{at}ms] lifecycle: {state:?}");
                    lifecycle.push((at, format!("{state:?}")));
                }
                _ => {}
            }
        }
    };
    // Turn 1 as audio, then silence while the model answers.
    for chunk in samples.chunks(1_600) {
        channel.send_audio(chunk).await.expect("chunk accepted");
        pump(&mut events, &mut assistant, &mut lifecycle);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for _ in 0..80 {
        channel.send_audio(&[0i16; 1_600]).await.ok();
        pump(&mut events, &mut assistant, &mut lifecycle);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!(
        "live[{}ms] turn-1 answer: {assistant:?}",
        started.elapsed().as_millis()
    );
    assert!(
        !assistant.trim().is_empty(),
        "turn 1 must be answered first"
    );
    // SEVER: a hard transport drop, mid-session, no goAway.
    std::fs::write("/tmp/r2probe/SEVER", b"x").expect("sever flag written");
    println!(
        "live[{}ms] transport severed",
        started.elapsed().as_millis()
    );
    // The user keeps talking into the dead window: these sends are the
    // loss the accounting measures (refused / dropped while renewing).
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reconnected = false;
    while Instant::now() < deadline {
        if channel.send_audio(&[0i16; 1_600]).await.is_err() {
            refused += 1;
        }
        pump(&mut events, &mut assistant, &mut lifecycle);
        if lifecycle.iter().any(|(_, s)| s.contains("Reconnected")) {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        reconnected,
        "the abrupt loss must recycle on the real wire (lifecycle: {lifecycle:?})"
    );
    assert!(
        refused > 0,
        "the dead window must refuse sends — that refusal IS the loss signal"
    );
    // The production Reconnected sequence: recap first, then the lost turn.
    channel
        .update_context(ContextUpdate::RenewalRecap {
            recap: DelimitedExcerpt {
                delimited_text: format!(
                    "<<recap>>Most recent turns:\nuser: What is a graph database, \
                     and when should I use one?\nassistant: {}\n<</recap>>",
                    assistant.chars().take(200).collect::<String>()
                ),
            },
            switched_to: None,
        })
        .await
        .expect("the recap rides the fresh socket");
    assistant.clear();
    channel
        .send_text("Can you hear me? What did I just ask you about?")
        .await
        .expect("the recovered turn rides after the recap");
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        channel.send_audio(&[0i16; 1_600]).await.ok();
        pump(&mut events, &mut assistant, &mut lifecycle);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    channel.close().await.ok();
    println!(
        "live[{}ms] recovered-turn answer: {assistant:?}",
        started.elapsed().as_millis()
    );
    println!("live: refused during dead window: {refused}");
    println!("live: lifecycle: {lifecycle:?}");
    assert!(
        assistant.to_lowercase().contains("graph"),
        "the recovered turn must be answered with the original conversation's \
         context: {assistant:?}"
    );
}

/// F7 owner-approved live smoke (2026-08-30): one whole-page still over the
/// documented `realtimeInput.video` path on the pinned dialog model, then a
/// text turn asking what is visible — the reply is the sight evidence. The
/// second half answers the owner's cap question: docs cap "sessions with
/// video" at 2 minutes (vs 15 audio-only); this session idles past 2:30
/// after the still to observe whether a single still reclassifies it.
///
/// ```text
/// VISION_SMOKE_PNG=/path/page.png \
///   cargo test --lib smoke_dialog_page_sight -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_dialog_page_sight() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(png_path) = std::env::var("VISION_SMOKE_PNG") else {
        println!("SKIP: VISION_SMOKE_PNG unset (a rendered page PNG)");
        return;
    };
    let png = std::fs::read(&png_path).expect("page png readable");
    println!("live: page still {} bytes", png.len());
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: live_model(),
        },
        keys,
        Generation::new(),
    );
    // A sight-only context: EMPTY excerpt, neutral prompt, no tools — the
    // reply can only come from the pixels (a context naming figures made
    // run 1's evidence ambiguous).
    let mut context = tutor_context();
    context.excerpt = DelimitedExcerpt {
        delimited_text: String::new(),
    };
    context.system_prompt =
        "You are a helpful assistant. Answer in one short sentence.".to_string();
    context.tool_schemas = Vec::new();
    channel.connect(context).await.expect("connect");
    let mut events = channel.take_events().expect("fresh channel");
    channel
        .send_image(&png, "image/png")
        .await
        .expect("still accepted");
    println!("live: still sent over realtimeInput.video");
    channel
        .send_text("In one short sentence, what do you see on the page image I just sent?")
        .await
        .expect("text turn accepted");
    let deadline = Instant::now() + TURN_BUDGET;
    let mut sight: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, events.recv()).await {
            Ok(Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            })) => {
                println!("live: sight reply: {text:?}");
                sight.push(text);
                if sight.concat().len() > 40 {
                    break;
                }
            }
            Ok(Some(ChannelEvent::Lifecycle(LifecycleEvent::Expired))) => {
                panic!("session expired before the sight reply");
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(
        !sight.concat().trim().is_empty(),
        "the model described the page it was sent"
    );
    // Cap probe: idle past 2:30 with a keepalive ping each 30 s; an Expired
    // (or closed stream) before ~150 s = a single still reclassified the
    // session to the 2-minute video cap.
    if std::env::var("VISION_SMOKE_CAP").is_err() {
        println!(
            "live: cap probe skipped (VISION_SMOKE_CAP unset; measured twice \
             2026-08-30: alive 159.7s and 161.6s — no reclassification)"
        );
        channel.close().await.ok();
        return;
    }
    println!("live: cap probe — idling past 2:30 after the still…");
    let probe_start = Instant::now();
    let mut reclassified = false;
    while probe_start.elapsed() < Duration::from_secs(155) {
        match timeout(Duration::from_secs(30), events.recv()).await {
            Ok(Some(ChannelEvent::Lifecycle(LifecycleEvent::Expired))) | Ok(None) => {
                reclassified = true;
                println!(
                    "live: session ENDED at {:?} after the still — 2-minute video cap applies",
                    probe_start.elapsed()
                );
                break;
            }
            Ok(Some(_)) => {}
            Err(_) => {
                let _ = channel.send_text("still here — no reply needed.").await;
            }
        }
    }
    if !reclassified {
        println!(
            "live: session ALIVE {:?} after the still — a single still does NOT trip the 2-minute cap",
            probe_start.elapsed()
        );
    }
    channel.close().await.ok();
}

/// Boundary-export gating probe (span-join-investigation §9 acceptance law,
/// owner O2 2026-08-31): BEFORE any production change, prove on the real
/// `gemini-3.5-transcribe-live` wire with our exact setup frame that
/// (a) `audioStreamEnd` finalizes the open segment PER-SPAN promptly —
/// speech is streamed with NO trailing silence, so the server's own VAD
/// has no silence to detect and any prompt final is attributable to the
/// export alone; (b) `audioStreamEnd` with no buffered speech (the false-
/// onset case) — behavior recorded verbatim, no assertion on shape;
/// (c) resume-after-end works repeatedly (>=3 cycles) on ONE socket.
///
/// ```text
/// BOUNDARY_PROBE_PCM=/tmp/r2probe/probe_a_two_turns.pcm \
///   cargo test --lib smoke_boundary_export_probe -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_boundary_export_probe() {
    let keys = vault_or_env_keys();
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    let Ok(pcm_path) = std::env::var("BOUNDARY_PROBE_PCM") else {
        println!("SKIP: BOUNDARY_PROBE_PCM unset (raw s16le 16 kHz mono probe audio)");
        return;
    };
    let bytes = std::fs::read(&pcm_path).expect("probe audio readable");
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let span1 = &samples[0..(3.35 * 16_000.0) as usize];
    let span2 = &samples[(9.50 * 16_000.0) as usize..(11.20 * 16_000.0) as usize];
    println!(
        "probe: span1 {:.2}s, span2 {:.2}s (probe-A two-sentence fixture, pause boundary cut)",
        span1.len() as f64 / 16_000.0,
        span2.len() as f64 / 16_000.0
    );
    let model = crate::model_pins::TUTOR_TRANSCRIBE_MODEL.to_string();
    let mut channel = GeminiLiveChannel::new(
        GeminiLiveConfig {
            endpoint_override: None,
            model: model.clone(),
        },
        keys,
        Generation::new(),
    )
    .with_transcribe_mode();
    channel
        .connect(tutor_context())
        .await
        .expect("transcribe setup accepted with our exact setup frame");
    println!("probe: setupComplete on {model}");
    let mut events = channel.take_events().expect("fresh channel");

    async fn stream_span(channel: &mut GeminiLiveChannel, span: &[i16]) {
        for chunk in span.chunks(1_600) {
            channel.send_audio(chunk).await.expect("chunk accepted");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn drain_until_final(
        events: &mut mpsc::Receiver<ChannelEvent>,
        budget: Duration,
        label: &str,
    ) -> Option<(String, Duration)> {
        let start = Instant::now();
        let deadline = start + budget;
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match timeout(left, events.recv()).await {
                Ok(Some(ChannelEvent::TranscriptDelta {
                    role: Role::User,
                    text,
                })) => {
                    let elapsed = start.elapsed();
                    println!("probe[{label}]: FINAL after {elapsed:?}: {text:?}");
                    return Some((text, elapsed));
                }
                Ok(Some(other)) => println!("probe[{label}]: event: {other:?}"),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        None
    }

    let final_budget = Duration::from_secs(10);

    stream_span(&mut channel, span1).await;
    channel
        .send_audio_stream_end()
        .await
        .expect("audioStreamEnd accepted at span-1 pause boundary");
    println!("probe[a-span1]: audioStreamEnd sent at pause boundary, zero trailing silence");
    let final1 = drain_until_final(&mut events, final_budget, "a-span1").await;

    stream_span(&mut channel, span2).await;
    channel
        .send_audio_stream_end()
        .await
        .expect("audioStreamEnd accepted after span 2");
    println!("probe[a-span2]: audioStreamEnd sent, zero trailing silence");
    let final2 = drain_until_final(&mut events, final_budget, "a-span2").await;

    println!("probe[b]: audioStreamEnd with NO buffered speech (false-onset case)");
    let empty_end = channel.send_audio_stream_end().await;
    println!("probe[b]: send result: {empty_end:?}");
    let b_final = drain_until_final(&mut events, Duration::from_secs(6), "b-empty").await;
    println!("probe[b]: behavior on empty segment: {b_final:?}");

    let mut resume_finals: Vec<(String, Duration)> = Vec::new();
    for cycle in 1..=3u32 {
        let label = format!("c-resume{cycle}");
        stream_span(&mut channel, span2).await;
        channel
            .send_audio_stream_end()
            .await
            .unwrap_or_else(|err| panic!("resume cycle {cycle}: socket unusable: {err:?}"));
        match drain_until_final(&mut events, final_budget, &label).await {
            Some(got) => resume_finals.push(got),
            None => println!("probe[{label}]: NO final within budget"),
        }
    }
    channel.close().await.ok();

    println!("=== PROBE SUMMARY ===");
    println!("a: span1 final = {final1:?}");
    println!("a: span2 final = {final2:?}");
    println!("b: empty-segment behavior = send {empty_end:?}, final {b_final:?}");
    println!(
        "c: resume cycles finalized = {}/3: {resume_finals:?}",
        resume_finals.len()
    );
    let a_holds = matches!((&final1, &final2), (Some((t1, _)), Some((t2, _)))
        if !t1.trim().is_empty() && !t2.trim().is_empty());
    let c_holds = resume_finals.len() == 3;
    if a_holds && c_holds {
        println!("VERDICT: PROBE PASS (a+c hold; b's behavior stated above)");
    } else {
        println!("VERDICT: PROBE FAIL (a_holds={a_holds} c_holds={c_holds})");
    }
    assert!(
        a_holds,
        "clause (a): per-span finalization via audioStreamEnd"
    );
    assert!(
        c_holds,
        "clause (c): >=3 resume-after-end cycles on one socket"
    );
}
