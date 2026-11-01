//! The learning-profile sink seam (owner ruling R1).
//!
//! `update_learning_profile` is a Tier-1 tool: reversible local state, no
//! book text, nothing leaves the device. But the dispatcher lives inside
//! `src/voice/`, where the VA4 egress scan bans every database token — so,
//! exactly like playback tools mutate through [`PlaybackControl`], the
//! profile tool mutates through this trait. The production implementation
//! ([`crate::learning_profile::SessionProfileSink`]) lives OUTSIDE the voice
//! tree, holds no book text, and enqueues writes to an async persist task —
//! `update` must be sync and non-blocking because tool execution runs inside
//! the pump's dispatch path.
//!
//! [`PlaybackControl`]: super::playback::PlaybackControl

/// Why an update was refused. The reason is answered to the model verbatim
/// (§5's answered-never-panicked rule), so it must be instructive, bounded,
/// and contain no user data beyond what the model already sent.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileRefused {
    pub reason: String,
}

impl ProfileRefused {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Accepts validated profile updates. `Ok` means the update was accepted
/// into the local store's write path — implementations may persist
/// asynchronously (documented on the implementation), but an `Ok` must never
/// be returned for an update that was dropped.
pub trait LearningProfileSink: Send {
    fn update(&mut self, key: &str, value: &str) -> Result<(), ProfileRefused>;
}
