//! The presentation half of the VA6 UI bridge: forwarding normalized
//! [`ChannelEvent::TeacherAnchor`] events from the event pump's sink to the
//! webview's live preview (VOICE_AGENT §11.1; `teacherAnchorModel.ts` /
//! `ReadingSpotlight.tsx` on the other side).
//!
//! Only live-generation anchors ever reach this seam — the pump drops and
//! acks stale ones ([`crate::voice::channel::anchor`]) before its sink runs —
//! so the payload carries `gen` purely as an ordering guard: events cross the
//! Tauri boundary asynchronously, and the webview model refuses a lower
//! generation arriving after a higher one (its only staleness signal; it
//! cannot see the barge-in counter). Everything else the §11.1 contract owes
//! (change-only scroll, scroll pause, the spotlight, the disable setting)
//! lives in the webview model, where the page geometry is.

use serde::Serialize;

use crate::voice::channel::AnchorRef;

/// The Tauri event carrying live tutor anchors to the reading surface.
pub const TEACHER_ANCHOR_EVENT: &str = "voice-teacher-anchor";

/// The wire form of one anchor: the §5 tool-arg vocabulary for `kind`
/// (`section` / `figure` / `equation` / `table`), the existence-checkable id,
/// and the response generation for ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeacherAnchorPayload {
    pub kind: &'static str,
    pub id: String,
    pub gen: u64,
}

impl TeacherAnchorPayload {
    pub fn new(anchor: &AnchorRef, gen: u64) -> Self {
        Self {
            kind: anchor.kind.as_tool_arg(),
            id: anchor.id.clone(),
            gen,
        }
    }
}

/// The one call the session runtime makes from its event-pump sink for a
/// surfaced `TeacherAnchor { anchor, gen }`. Returns `false` when the webview
/// could not be reached — presentation sync is best-effort by design (§11.1
/// mutates presentation state only; the tool ack already went out).
pub fn emit_teacher_anchor(app: &tauri::AppHandle, anchor: &AnchorRef, gen: u64) -> bool {
    tauri::Emitter::emit(
        app,
        TEACHER_ANCHOR_EVENT,
        TeacherAnchorPayload::new(anchor, gen),
    )
    .is_ok()
}

/// The Tauri event carrying Book Agent citations to the reading surface
/// (charter §4.5). Independent of the playback clock and of
/// [`TEACHER_ANCHOR_EVENT`]: playback movement neither creates nor relocates
/// an Agent citation.
#[cfg(feature = "book-agent")]
pub const AGENT_HIGHLIGHT_EVENT: &str = "voice-agent-highlight";

/// The wire form of one Agent citation: validated navigation ids only —
/// chapter/page/segment coordinates, optional word span, session generation
/// for ordering. Never a passage body (charter §4.5).
#[cfg(feature = "book-agent")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHighlightPayload {
    pub chapter: u32,
    pub page: u32,
    pub segment_idx: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_word: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_count: Option<u32>,
    pub gen: u64,
    /// Applied-effect ack correlation (B-4): the webview answers
    /// `agent_highlight_ack` with this tool call id AFTER the render commit
    /// resolved the citation (applied, or honestly not).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_call_id: Option<String>,
}

/// Best-effort emit, same seam shape as [`emit_teacher_anchor`]. The tool ack
/// must NOT be derived from this return value alone — the applied-effect
/// answer comes from the reading surface's applier via the highlight port;
/// this emit is the transport.
#[cfg(feature = "book-agent")]
pub fn emit_agent_highlight(app: &tauri::AppHandle, payload: &AgentHighlightPayload) -> bool {
    tauri::Emitter::emit(app, AGENT_HIGHLIGHT_EVENT, payload.clone()).is_ok()
}

/// The Tauri event asking the reading view to steer to page mode.
pub const PAGE_MODE_STEER_EVENT: &str = "voice-page-mode-steer";

/// Emitted once when a live tutor session starts (§11.1 scope, PR #206
/// verifier option 3): the spotlight is a page-surface contract, so the
/// runtime steers the reading view onto the surface that can honor it. A
/// steer, not a lock — the user can switch back, and the §11.1 pause/resume
/// affordances own everything after this nudge. Best-effort like the anchor
/// emit; a session started with no reading view open steers nothing.
pub fn emit_page_mode_steer(app: &tauri::AppHandle) -> bool {
    tauri::Emitter::emit(app, PAGE_MODE_STEER_EVENT, ()).is_ok()
}
