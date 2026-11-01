//! Book Agent tool ports (charter B-1). Compiled only under the `book-agent`
//! feature; no production session wires these yet.
//!
//! The dispatcher stays free of retrieval internals the same way it stays
//! free of the playing surface: everything the four Agent tools *do* runs
//! through the two ports here, wired per session like `PlaybackControl` and
//! the learning-profile sink. Raw retrieval coordinates cross this seam only
//! as [`PassageCoord`] — already resolved from an opaque session token by the
//! retrieval side; the dispatcher never sees or mints tokens' backing state.
//!
//! Honesty contract: [`AgentHighlightPort::apply`] reports what the reading
//! surface actually did. An adapter that only fired an event and cannot know
//! whether anything rendered must answer [`HighlightStatus::NotApplied`] with
//! a reason — the charter's applied-effect gate forbids treating emission as
//! application.

use serde_json::Value;

/// A resolved passage location: the retrieval side has already validated the
/// opaque token against the active book/render/index generation. Carries no
/// text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassageCoord {
    /// 0-based chapter index in reading order (the `jump_to_chapter` space).
    pub chapter: u32,
    /// Printed page number.
    pub page: u32,
    /// Segment index within the active render (`SessionSegment::idx` space).
    pub idx: u32,
}

/// What the reading surface did with a citation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HighlightStatus {
    /// The citation is visibly applied on the reading surface.
    Applied,
    /// Honest non-application: absent surface, absent geometry, disabled
    /// setting, stale generation. `reason` is a bounded code, never book text.
    NotApplied { reason: String },
    /// The request left for the surface; the REAL applied/not-applied answer
    /// arrives asynchronously (the production path: the webview acks after
    /// render, and the session actor answers the provider then — the pump is
    /// never blocked waiting). Only transport-style ports return this.
    Pending,
}

/// Retrieval surface offered to the dispatcher. The adapter owns query
/// validation depth, opaque handle minting/resolution, and bounded
/// serialization; the returned `Value` is the exact LLM-facing tool payload
/// (already capped — the dispatcher adds nothing).
pub trait AgentRetrievalPort: Send {
    /// The render job this port's handles are bound to. WIRING INVARIANT
    /// (charter §4.3/T10): the session wiring MUST construct this port and
    /// the dispatcher's `SessionSnapshot` from the same immutable session
    /// binding (book, job, render) and assert these agree — the per-call
    /// idx/page check in the dispatcher is a proxy, not a cross-render
    /// identity proof. B-2's binding snapshot owns enforcement; fixture rigs
    /// assert it directly.
    fn job_id(&self) -> &str;
    /// `search_book`: 1-4 lexical variants, optional chapter scope.
    fn search_book(&mut self, queries: &[String], chapter: Option<u32>) -> Value;
    /// `open_passage`: one live opaque handle token.
    fn open_passage(&mut self, token: &str) -> Value;
    /// Resolve a live token to its coordinate for navigation. `None` =
    /// unknown/stale/expired token (refused, never remapped — T10).
    /// Navigation may target generated (related) passages; citation may not.
    fn resolve(&self, token: &str) -> Option<PassageCoord>;
    /// Resolve a live token for citation: additionally `None` when the hit is
    /// generated prose, which is retrieval metadata and never visible
    /// evidence (T9). Callers distinguish the two refusals via [`Self::resolve`].
    fn resolve_citable(&self, token: &str) -> Option<PassageCoord>;
}

/// Citation surface offered to the dispatcher for `highlight_passage`.
pub trait AgentHighlightPort: Send {
    /// Ask the reading surface to show one playhead-decoupled citation.
    /// `call_id` is the tool call the eventual ack answers; `span` is an
    /// optional validated (start_word, word_count) within the passage.
    /// Returning [`HighlightStatus::Pending`] defers the honest answer to
    /// the surface's post-render ack — emission alone is NEVER Applied.
    fn apply(
        &mut self,
        call_id: &str,
        coord: PassageCoord,
        span: Option<(u32, u32)>,
    ) -> HighlightStatus;
}
