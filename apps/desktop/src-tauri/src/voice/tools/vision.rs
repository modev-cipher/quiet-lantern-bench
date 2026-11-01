//! F7 (owner ruling 2026-08-30): the sight port — how a live session gets
//! the CURRENT page's rendered pixels. The trait lives here (tool layer) so
//! the pump can drive it; the implementation lives OUTSIDE `src/voice`
//! (voice_session.rs), where the rendered-page cache is reachable — this
//! module never touches book text or storage, keeping the excerpt scan
//! gate's single-egress-point rule intact (pixels are a separate, consented
//! egress category; the consent copy names page images).

use async_trait::async_trait;

/// One rendered page still, whole-page by law (owner ruling: never cropped
/// to parsed regions — figures, charts, diagrams, tables, and rendered
/// equations are all "the page").
pub struct VisionFrame {
    pub page_no: i64,
    pub bytes: Vec<u8>,
    /// `image/png` today (the render cache's format).
    pub mime: &'static str,
}

/// Resolves one page number to its rendered pixels (the caller resolves
/// position to page through the excerpt selector's navigation data). Errors
/// are short user-safe reasons (no paths), surfaced verbatim in the tool
/// refusal.
#[async_trait]
pub trait VisionPort: Send {
    async fn frame_for_page(&mut self, page_no: i64) -> Result<VisionFrame, String>;
}
