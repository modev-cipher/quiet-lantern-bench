//! ExcerptSelector + context engine (VA4, VOICE_AGENT §4).
//!
//! This module is the **single enforcement point for book text egress**: the
//! only code under `src/voice` allowed to read book text (page/segment text
//! from the reading structures) and turn it into a [`DelimitedExcerpt`] a
//! channel may carry off-device. That rule is not a convention — `scan.rs`
//! walks every voice source file and fails the build the moment any other
//! voice module names the page-text access surface.
//!
//! Nothing here performs egress. This module produces values; sending them
//! anywhere is the ByokChannel providers' job (task VA5), which also wires the
//! per-first-use-per-book consent gate (VOICE_AGENT §5) in front of the first
//! excerpt leaving the device. Until then no live egress path exists.
//!
//! [`DelimitedExcerpt`]: crate::voice::channel::types::DelimitedExcerpt

pub mod coverage;
pub mod policy;
pub mod selector;
pub mod source;
pub mod summary;
pub mod window;

mod sentinel;

// Gated from the inside (`#![cfg(test)]`), the `voice::keys::hygiene` pattern.
mod scan;

/// Latin-script terms of an excerpt's text, deduped in first-appearance
/// order, ≥ 4 letters, capped small. Extraction RE-LISTS words the provider
/// already receives inside the delimited excerpt — it never widens egress.
/// Two consumers: the JIT code-switch fragment (voice_session) and the
/// input-transcription vocabulary bias (channel::gemini, F4).
pub fn latin_terms(text: &str) -> Vec<String> {
    const CAP: usize = 40;
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();
    let mut word = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphabetic() {
            word.push(c.to_ascii_lowercase());
            continue;
        }
        if word.len() >= 4 && seen.insert(word.clone()) {
            terms.push(word.clone());
            if terms.len() == CAP {
                return terms;
            }
        }
        word.clear();
    }
    terms
}
