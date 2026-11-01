//! Book Agent IPC layer (charter B-4, feature `book-agent`): the Tauri
//! commands that mount `SessionMode` over the existing one-session
//! `VoiceSessionSlot`, execute the §2.2 switch law, and close the B-1
//! applied-effect gate with a real webview ack round-trip.
//!
//! Lives beside `voice_session.rs`/`agent_session.rs`, outside the voice
//! scan tree.

use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::agent_session::{
    self, AgentEgressAuthorization, SessionMode, TeardownStep, TEARDOWN_ORDER,
};
use crate::providers::connection::{ConnectionRecord, ConnectionStore};
use crate::providers::registry::{PhaseCapability, ProviderKind};
use crate::voice::tools::agent::{AgentHighlightPort, HighlightStatus, PassageCoord};
use crate::voice_presentation::AgentHighlightPayload;
use crate::voice_session::{SessionProvider, SessionRequest};
use crate::VoiceSessionSlot;

/// The Tauri event that clears the Agent citation on mode switch/teardown
/// (§2.2 step 5). Payload-free; the webview model resets.
pub const AGENT_HIGHLIGHT_CLEAR_EVENT: &str = "voice-agent-highlight-clear";

/// Which mode the live slot session is running (the slot itself only knows
/// liveness), tagged with THAT session's own mailbox so death watchers can
/// prove they are clearing their own session's tag and never a successor's.
#[derive(Default)]
pub struct AgentModeTag(
    pub  Mutex<
        Option<(
            SessionMode,
            tokio::sync::mpsc::Sender<crate::voice_session::SessionCommand>,
        )>,
    >,
);

impl AgentModeTag {
    /// The live mode, `None` when unset or when the tagged session is dead.
    pub fn live_mode(&self) -> Option<SessionMode> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(_, mailbox)| !mailbox.is_closed())
            .map(|(mode, _)| *mode)
    }
}

/// The production `AgentHighlightPort`: EMIT-ONLY transport. It fires the
/// id-only citation event carrying the tool call id and returns
/// [`HighlightStatus::Pending`] — the honest applied/not-applied answer
/// arrives from the webview's post-render ack as a
/// `SessionCommand::AgentHighlightAck`, and the session actor answers the
/// provider then. The pump is never blocked waiting (Hermes finale item 7a),
/// and emission alone is never Applied (the B-1 gate's law).
pub struct TauriHighlightPort {
    app: tauri::AppHandle,
    generation: crate::voice::audio::generation::Generation,
}

impl TauriHighlightPort {
    pub fn new(
        app: tauri::AppHandle,
        generation: crate::voice::audio::generation::Generation,
    ) -> Self {
        Self { app, generation }
    }
}

impl AgentHighlightPort for TauriHighlightPort {
    fn apply(
        &mut self,
        call_id: &str,
        coord: PassageCoord,
        span: Option<(u32, u32)>,
    ) -> HighlightStatus {
        let payload = AgentHighlightPayload {
            chapter: coord.chapter,
            page: coord.page,
            segment_idx: coord.idx,
            start_word: span.map(|(s, _)| s),
            word_count: span.map(|(_, c)| c),
            gen: self.generation.current(),
            ack_call_id: Some(call_id.to_string()),
        };
        if !crate::voice_presentation::emit_agent_highlight(&self.app, &payload) {
            return HighlightStatus::NotApplied {
                reason: "no_reading_surface".to_string(),
            };
        }
        HighlightStatus::Pending
    }
}

/// Mode truth survives every death path (Hermes finale item 4): when THIS
/// session's actor dies — End button, provider failure, expiry, switch — the
/// tag clears and the citation surface resets. Keyed on the handle's own
/// mailbox, so a newer session's tag is never clobbered by an older watcher
/// (the newer start overwrites the tag after this watcher's session ends,
/// and a stale watcher only clears a tag its own liveness check just proved).
pub fn watch_session_death(app: tauri::AppHandle, handle: &crate::voice_session::SessionHandle) {
    let mailbox = handle.mailbox_watch();
    tauri::async_runtime::spawn(async move {
        mailbox.closed().await;
        let _ = tauri::Emitter::emit(&app, AGENT_HIGHLIGHT_CLEAR_EVENT, ());
        if let Some(tag) = app.try_state::<AgentModeTag>() {
            let mut tag = tag.0.lock().unwrap();
            // Clear only OUR session's tag: a successor session re-tagged
            // with its own (live) mailbox is never clobbered by a stale
            // watcher (same_channel proves identity, is_closed proves death).
            if tag
                .as_ref()
                .is_some_and(|(_, tagged)| tagged.same_channel(&mailbox))
            {
                *tag = None;
            }
        }
    });
}

/// The enabled Gemini connection that carries Agent sessions, if any.
async fn agent_connection(
    pool: &crate::storage::SqlitePool,
) -> Result<Option<ConnectionRecord>, String> {
    let all = ConnectionStore::list(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(all.into_iter().find(|c| {
        c.enabled
            && c.provider == ProviderKind::Gemini
            && c.provider.phases().contains(&PhaseCapability::AgentLive)
    }))
}

/// The Book composer's pre-start truth (§7.3 subset the entry needs):
/// unsupported (no agent-capable connection), consent_required (with the
/// verbatim prompt facts), or ready.
#[tauri::command]
pub async fn agent_session_probe(
    pool: tauri::State<'_, crate::storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    mode_tag: tauri::State<'_, AgentModeTag>,
    book_id: String,
) -> Result<serde_json::Value, String> {
    // Runtime truth first (§7.3): a live session's mode comes from the tag,
    // never from UI guesses.
    {
        let session = slot.0.lock().await;
        if session.as_ref().is_some_and(|h| h.is_alive()) {
            let mode = mode_tag.live_mode();
            return Ok(serde_json::json!({
                "state": "live",
                "mode": match mode {
                    Some(SessionMode::Agent) => "agent",
                    _ => "tutor",
                },
            }));
        }
    }
    let Some(connection) = agent_connection(&pool).await? else {
        return Ok(serde_json::json!({ "state": "unsupported" }));
    };
    match agent_session::authorize_agent_egress(&pool, &book_id, &connection)
        .await
        .map_err(|e| e.to_string())?
    {
        AgentEgressAuthorization::ConsentRequired {
            hosts,
            data_categories,
            retention_url,
        } => Ok(serde_json::json!({
            "state": "consent_required",
            "connection_id": connection.id,
            "connection_name": connection.name,
            "hosts": hosts,
            "data_categories": data_categories,
            "retention_url": retention_url,
        })),
        AgentEgressAuthorization::Authorized { .. } | AgentEgressAuthorization::Loopback => {
            Ok(serde_json::json!({ "state": "ready" }))
        }
    }
}

/// Explicit consent grant from the UI flow: records exactly the displayed
/// host set (grant-time TOCTOU guard re-verifies it).
#[tauri::command]
pub async fn agent_consent_grant(
    pool: tauri::State<'_, crate::storage::SqlitePool>,
    book_id: String,
    connection_id: String,
    shown_hosts: Vec<String>,
) -> Result<(), String> {
    // The grant redeems EXACTLY the connection the prompt displayed: a
    // connection swapped between prompt and click is a re-prompt, never a
    // silent rebind (Hermes finale item 6).
    let connection = agent_connection(&pool)
        .await?
        .filter(|c| c.id == connection_id)
        .ok_or("the connection shown at the prompt is gone; re-open the consent prompt")?;
    agent_session::record_agent_consent(
        &pool,
        &book_id,
        &connection,
        &shown_hosts,
        crate::storage::now_ms() / 1_000,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Start (or explicitly switch to) a Book Agent session. Executes the §2.2
/// law over the one-session slot: same-mode no-op, cross-mode-while-live
/// requires `confirmed`, and the six-step teardown runs to completion before
/// the fresh session starts.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn agent_session_start(
    app: tauri::AppHandle,
    pool: tauri::State<'_, crate::storage::SqlitePool>,
    keys: tauri::State<'_, Arc<crate::voice::keys::KeyManager>>,
    surface: tauri::State<'_, Arc<crate::voice_playback::PlaybackSurface>>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    mode_tag: tauri::State<'_, AgentModeTag>,
    book_id: String,
    job_id: String,
    playback_pos_ms: u64,
    confirmed: bool,
) -> Result<(), String> {
    let mut session = slot.0.lock().await;
    let live_mode = if session.as_ref().is_some_and(|h| h.is_alive()) {
        Some(mode_tag.live_mode().unwrap_or(SessionMode::Tutor))
    } else {
        None
    };
    match agent_session::decide_mode_switch(live_mode, SessionMode::Agent, confirmed)
        .map_err(|e| e.to_string())?
    {
        agent_session::SwitchDecision::AlreadyLive => return Ok(()),
        agent_session::SwitchDecision::StartFresh { .. } => {}
        agent_session::SwitchDecision::SwitchAfterTeardown { steps, .. } => {
            debug_assert_eq!(steps, TEARDOWN_ORDER);
            for step in steps {
                match step {
                    // Steps 1-4 are the actor's own stop path: taking the
                    // handle stops tool intake here (nothing else can send),
                    // Stop interrupts output, bumps the generation, stales
                    // pending calls, closes the channel, releases audio, and
                    // settles the journal per retention.
                    TeardownStep::StopToolIntake
                    | TeardownStep::InterruptAndStalePending
                    | TeardownStep::CloseChannelReleaseAudio
                    | TeardownStep::SettleJournal => {
                        // Steps 1-4 are the actor's own stop path (Stop bumps
                        // the generation and interrupts output, run() closes
                        // the channel then settles the journal) and they must
                        // COMPLETE before step 6 (T12). stop_and_wait is a
                        // reliable send + JOIN on actor death — no polling,
                        // no lost Stop through a full mailbox. Ownership law:
                        // the handle STAYS IN THE SLOT until death is
                        // confirmed, so a timeout leaves the old session
                        // tracked and a retry cannot dial over it.
                        if let Some(handle) = session.as_ref() {
                            if !handle
                                .stop_and_wait(crate::voice_session::SESSION_STOP_WAIT)
                                .await
                            {
                                return Err(
                                    "the live session did not shut down; try again".to_string()
                                );
                            }
                            *session = None;
                        }
                    }
                    // Step 5: mode-transient presentation state.
                    TeardownStep::ClearModePresentation => {
                        let _ = tauri::Emitter::emit(&app, AGENT_HIGHLIGHT_CLEAR_EVENT, ());
                    }
                    // Step 6 is the fresh start below.
                    TeardownStep::StartFreshSession => {}
                }
            }
            *mode_tag.0.lock().unwrap() = None;
        }
    }

    let connection = agent_connection(&pool)
        .await?
        .ok_or("no agent-capable connection")?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let playback = Box::new(crate::voice_playback::WebviewPlayback::new(
        surface.inner().clone(),
        app.clone(),
    ));
    let sink = Box::new(crate::voice_session::TauriSessionSink::new(app.clone()));
    // The highlight port is built INSIDE session start with the session's
    // own generation — the payload gen and the barge-in counter are the same
    // counter, never two.
    let app_for_port = app.clone();
    let highlight_factory: crate::voice_session::HighlightPortFactory =
        Box::new(move |generation| Box::new(TauriHighlightPort::new(app_for_port, generation)));
    let handle = crate::voice_session::start_agent_session(
        pool.inner().clone(),
        keys.inner().clone(),
        SessionRequest {
            book_id: Some(book_id),
            job_id: Some(job_id),
            provider: SessionProvider::GeminiLive,
            playback_pos_ms,
            resume_from: None,
        },
        &data_dir.join("cache"),
        data_dir.join("books"),
        &connection,
        playback,
        sink,
        highlight_factory,
    )
    .await
    .map_err(|e| e.to_string())?;
    watch_session_death(app.clone(), &handle);
    let handle_mailbox = handle.mailbox_watch();
    *session = Some(handle);
    *mode_tag.0.lock().unwrap() = Some((SessionMode::Agent, handle_mailbox));
    Ok(())
}

/// The webview's applied-effect answer for one citation (closes the B-1
/// gate's round trip).
#[tauri::command]
pub async fn agent_highlight_ack(
    slot: tauri::State<'_, VoiceSessionSlot>,
    call_id: String,
    applied: bool,
    reason: Option<String>,
) -> Result<(), String> {
    // The ack rides the actor's own mailbox: the actor answers the provider
    // (or drops a stale/unknown ack) — no shared registry, no pump blocking.
    if let Some(handle) = slot.0.lock().await.as_ref() {
        handle.send(crate::voice_session::SessionCommand::AgentHighlightAck {
            call_id,
            applied,
            reason: reason.unwrap_or_default(),
        });
    }
    Ok(())
}

/// Stop whatever session is live and clear mode-transient state — the shared
/// end path both composers call.
#[tauri::command]
pub async fn agent_session_stop(
    app: tauri::AppHandle,
    slot: tauri::State<'_, VoiceSessionSlot>,
    mode_tag: tauri::State<'_, AgentModeTag>,
) -> Result<(), String> {
    let mut session = slot.0.lock().await;
    if let Some(handle) = session.as_ref() {
        // Same ownership law as the switch: the handle leaves the slot only
        // after confirmed death, so the Agent->Tutor ceremony that follows
        // this stop can never dial over a live socket (T12).
        if !handle
            .stop_and_wait(crate::voice_session::SESSION_STOP_WAIT)
            .await
        {
            return Err("the live session did not shut down; try again".to_string());
        }
        *session = None;
    }
    // Under the slot lock: the tag and the slot are one atom.
    let _ = tauri::Emitter::emit(&app, AGENT_HIGHLIGHT_CLEAR_EVENT, ());
    *mode_tag.0.lock().unwrap() = None;
    drop(session);
    Ok(())
}
