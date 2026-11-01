//! Local voice tools (task VA6, VOICE_AGENT §5; ARCHITECTURE §8.1).
//!
//! Everything the tutor can *do* executes locally in Rust against playback or
//! presentation state — the cloud never touches SQLite, and voice tools never
//! touch `render_job`/checkpoint rows. This module owns the Tier-0/1 tool
//! surface end to end:
//!
//! - [`registry`]: the closed tool list and its strict, provider-neutral
//!   schemas ([`crate::voice::channel::types::SessionContext::tool_schemas`]).
//! - [`playback`]: the port the playback tools act through. The playing
//!   surface lives in the webview (`playbackStore.ts`); the dispatcher speaks
//!   to it only through [`playback::PlaybackControl`], so every tool effect is
//!   testable with no UI and the UI bridge is one adapter, wired in the VA6
//!   UI slice.
//! - [`dispatcher`]: `ToolCall` event → validate → execute →
//!   [`crate::voice::channel::types::ToolResult`], enforcing the §8.1 tool-tier
//!   invariants (strict schemas, bounded numerics, Tier-1 rate limit,
//!   Tier-0 read-only).
//!
//! Book text egress: the two Tier-0 tools return book text and they do it
//! **only** through `ExcerptSelector` — the single egress point, hard byte cap
//! and per-session sentinel included. Nothing in this module can read book
//! text any other way, and the source scan in `voice::excerpt::scan` fails the
//! build if that ever changes.

#[cfg(feature = "book-agent")]
pub mod agent;
pub mod dispatcher;
pub mod learning;
mod pins;
pub mod playback;
pub mod registry;
pub mod vision;

pub use dispatcher::{
    answer_once, answer_tool_call, resend_pending_answers, Dispatcher, ToolOutcome, ValidatedCall,
};
pub use playback::{PlaybackControl, PlaybackUnavailable};
pub use registry::{ToolRegistry, ToolTier};
