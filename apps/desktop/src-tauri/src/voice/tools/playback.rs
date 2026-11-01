//! The port the Tier-1 playback tools act through (VOICE_AGENT §5:
//! "All tools execute locally in Rust against playback or presentation
//! state").
//!
//! The playing surface itself lives in the webview (`playbackStore.ts` /
//! `PlaybackEngine.tsx`); Rust owns the decision of *what* a tool does and the
//! webview owns *doing* it. [`PlaybackControl`] is that seam: the dispatcher
//! resolves every tool into plain effects on this trait — pause, resume, an
//! absolute seek target it computed itself, a speed — and the VA6 UI slice
//! ships the one adapter that bridges these calls over Tauri events to the
//! store. Keeping target-position arithmetic on the dispatcher side (not the
//! bridge) means the contract each tool promises is pinned by tests with no
//! webview anywhere.
//!
//! Every method is fallible: a session can outlive the playing surface (the
//! user closes the book mid-session, the webview reloads). The dispatcher
//! maps [`PlaybackUnavailable`] to a retryable tool error rather than
//! panicking or stalling the tool protocol.

/// The playing surface is not there to act on right now (no open book, bridge
/// torn down mid-session). Carries no detail on purpose: callers can only
/// report "try again once playback exists", whatever the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackUnavailable;

impl std::fmt::Display for PlaybackUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "playback surface unavailable")
    }
}

impl std::error::Error for PlaybackUnavailable {}

/// Playback effects, provider- and UI-neutral. Positions are milliseconds on
/// the book timeline — the same clock `ContextUpdate::PlaybackPosition`
/// carries and the excerpt window resolves against.
///
/// `Send` for the same reason [`SessionChannel`] carries it: the session
/// runtime's actor task owns one of these across await points.
///
/// [`SessionChannel`]: crate::voice::channel::SessionChannel
pub trait PlaybackControl: Send {
    fn position_ms(&self) -> Result<u64, PlaybackUnavailable>;
    fn pause(&mut self) -> Result<(), PlaybackUnavailable>;
    fn resume(&mut self) -> Result<(), PlaybackUnavailable>;
    /// Absolute seek. The dispatcher clamps targets to the book timeline
    /// before calling; implementations may clamp again but must not error on
    /// an in-range target.
    fn seek_to_ms(&mut self, pos_ms: u64) -> Result<(), PlaybackUnavailable>;
    /// Speed multiplier, already validated to the spec bounds
    /// ([`super::registry::SPEED_MIN`]..=[`super::registry::SPEED_MAX`]) by
    /// the dispatcher.
    fn set_speed(&mut self, speed: f64) -> Result<(), PlaybackUnavailable>;
}

/// F7 jobless-grounded sessions: the in-memory book cursor injected instead
/// of `WebviewPlayback` when the session has a book but NO narration job.
/// "Positions" are **character offsets** into the book's canonical text (the
/// spec's e-reader location model riding the existing ms fields), so the
/// excerpt window, `page_at`, and chapter navigation work untouched.
///
/// Only the position surface is real: `position_ms`/`seek_to_ms` move the
/// cursor. `pause`/`resume`/`set_speed` answer [`PlaybackUnavailable`] as a
/// backstop — the dispatcher refuses those tools typed
/// (`CODE_NO_NARRATION`) before they ever reach this port.
pub struct BookCursor {
    position: u64,
}

impl BookCursor {
    pub fn new(position: u64) -> Self {
        Self { position }
    }
}

impl PlaybackControl for BookCursor {
    fn position_ms(&self) -> Result<u64, PlaybackUnavailable> {
        Ok(self.position)
    }

    fn pause(&mut self) -> Result<(), PlaybackUnavailable> {
        Err(PlaybackUnavailable)
    }

    fn resume(&mut self) -> Result<(), PlaybackUnavailable> {
        Err(PlaybackUnavailable)
    }

    fn seek_to_ms(&mut self, pos_ms: u64) -> Result<(), PlaybackUnavailable> {
        self.position = pos_ms;
        Ok(())
    }

    fn set_speed(&mut self, _speed: f64) -> Result<(), PlaybackUnavailable> {
        Err(PlaybackUnavailable)
    }
}

/// Deterministic in-memory playing surface for dispatcher rigs (this crate's
/// unit tests only). Flip [`InMemoryPlayback::available`] off to exercise the
/// unavailable path.
#[cfg(test)]
pub(crate) struct InMemoryPlayback {
    pub position_ms: u64,
    pub paused: bool,
    pub speed: f64,
    pub available: bool,
}

#[cfg(test)]
impl InMemoryPlayback {
    pub fn at(position_ms: u64) -> Self {
        Self {
            position_ms,
            paused: false,
            speed: 1.0,
            available: true,
        }
    }

    fn ready(&self) -> Result<(), PlaybackUnavailable> {
        if self.available {
            Ok(())
        } else {
            Err(PlaybackUnavailable)
        }
    }
}

#[cfg(test)]
impl PlaybackControl for InMemoryPlayback {
    fn position_ms(&self) -> Result<u64, PlaybackUnavailable> {
        self.ready()?;
        Ok(self.position_ms)
    }

    fn pause(&mut self) -> Result<(), PlaybackUnavailable> {
        self.ready()?;
        self.paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), PlaybackUnavailable> {
        self.ready()?;
        self.paused = false;
        Ok(())
    }

    fn seek_to_ms(&mut self, pos_ms: u64) -> Result<(), PlaybackUnavailable> {
        self.ready()?;
        self.position_ms = pos_ms;
        Ok(())
    }

    fn set_speed(&mut self, speed: f64) -> Result<(), PlaybackUnavailable> {
        self.ready()?;
        self.speed = speed;
        Ok(())
    }
}
