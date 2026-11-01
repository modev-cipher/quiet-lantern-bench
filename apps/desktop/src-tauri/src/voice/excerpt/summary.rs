//! The cached rolling summary for everything before the window (VOICE_AGENT §4:
//! context = segment window + "the cached Teacher-mode rolling summary for
//! everything before the window"; the map-reduce shape reuses PIPELINE_SPEC §5's
//! rolling-summary machinery).
//!
//! **Cache key = the prefix boundary**: entry `b` holds the summary of segments
//! `[0, b)` of the session snapshot. Why this key and nothing else:
//!
//! - Within a session the snapshot is immutable, so a segment's position fully
//!   identifies its content — the boundary index is a complete key.
//! - The batch `segment_id` is explicitly rejected: it has **zero cross-render
//!   stability by construction** (the scripting stage mints fresh ids every
//!   render), so any cache keyed on it would silently go stale-but-plausible
//!   across a re-render. Position keys make the cache *unable* to outlive the
//!   session: the cache lives in the session-scoped engine, is never written
//!   anywhere, and dies with it — which is also exactly what §7's never-persist
//!   mode requires of session context ("held in memory only").
//!
//! **Rolling reuse**: advancing the window extends the nearest cached prefix
//! with only the newly-passed segments (the §5 `update rolling_summary` step)
//! instead of re-summarizing the whole prefix. Re-requesting a boundary already
//! cached returns it with **zero** summarizer invocations — the summarizer is a
//! session-LLM call (VOICE_AGENT §3: same LLM as the session, never a second
//! model), so a recompute is a real cost, and the tests pin the call counts.

use std::collections::BTreeMap;

use super::source::SessionSnapshot;

/// One summarizer step: fold `new_text` (the segments the window just moved
/// past, joined as paragraphs) into `previous` (the cached summary they extend,
/// `None` at the start of the book). Wired to the session LLM in VA5+; tests
/// use counting closures.
///
/// **Contract for implementors (VA5):** `new_text` is book text — it crosses
/// this seam already hard-capped at [`HARD_EGRESS_BYTE_CAP`] (the same final
/// backstop every excerpt passes; a real window advance is orders of magnitude
/// smaller, so the cap firing means a pathological render, not lost lecture).
/// It is **not yet sentinel-delimited**: before it reaches a provider prompt it
/// must go through [`ExcerptSelector::delimit_untrusted`] like every other
/// untrusted prompt field (VOICE_AGENT §4 — the delimiting happens at prompt
/// assembly, not here, because the summary prompt embeds it once and the
/// sentinel scan must run on the final serialization).
///
/// [`HARD_EGRESS_BYTE_CAP`]: super::selector::HARD_EGRESS_BYTE_CAP
/// [`ExcerptSelector::delimit_untrusted`]: super::selector::ExcerptSelector::delimit_untrusted
pub trait Summarize {
    fn extend(&mut self, previous: Option<&str>, new_text: &str) -> String;
}

impl<F> Summarize for F
where
    F: FnMut(Option<&str>, &str) -> String,
{
    fn extend(&mut self, previous: Option<&str>, new_text: &str) -> String {
        self(previous, new_text)
    }
}

const SEGMENT_JOIN: &str = "\n\n";

#[derive(Default)]
pub struct RollingSummaryCache {
    entries: BTreeMap<usize, String>,
}

impl RollingSummaryCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The summary of segments `[0, boundary)`, from cache when present.
    ///
    /// - `boundary == 0` (window at the top of the book): nothing precedes the
    ///   window — `None`, and the summarizer is not consulted.
    /// - Exact cache entry: returned as-is, zero summarizer calls.
    /// - Otherwise: one summarizer call extending the largest cached prefix
    ///   below `boundary` (from scratch when there is none — e.g. after a seek
    ///   back below every cached prefix, where a longer summary cannot be
    ///   "unsummarized" and honesty costs a recompute).
    pub fn summary_before(
        &mut self,
        boundary: usize,
        snapshot: &SessionSnapshot,
        summarize: &mut dyn Summarize,
    ) -> Option<&str> {
        let boundary = boundary.min(snapshot.len());
        if boundary == 0 {
            return None;
        }
        if !self.entries.contains_key(&boundary) {
            let base = self
                .entries
                .range(..boundary)
                .next_back()
                .map(|(b, summary)| (*b, summary.clone()));
            let (from, previous) = match &base {
                Some((b, summary)) => (*b, Some(summary.as_str())),
                None => (0, None),
            };
            let delta = snapshot.segments()[from..boundary]
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(SEGMENT_JOIN);
            // The summarizer seam is book-text egress like any other: the same
            // final backstop applies before text crosses it (see the trait
            // contract above).
            let (capped_delta, _) = super::selector::cap_at_char_boundary(
                &delta,
                super::selector::HARD_EGRESS_BYTE_CAP,
            );
            let extended = summarize.extend(previous, capped_delta);
            self.entries.insert(boundary, extended);
        }
        self.entries.get(&boundary).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> SessionSnapshot {
        SessionSnapshot::from_test_parts(vec![
            (1, 0, 1000, "s0"),
            (1, 1000, 2000, "s1"),
            (2, 2000, 3000, "s2"),
            (3, 3600, 4600, "s3"),
            (3, 4600, 5600, "s4"),
            (4, 5600, 6600, "s5"),
        ])
    }

    /// A summarizer whose output makes its inputs auditable: `[prev] + delta`.
    fn recording<'a>(
        calls: &'a mut Vec<(Option<String>, String)>,
    ) -> impl FnMut(Option<&str>, &str) -> String + 'a {
        move |previous: Option<&str>, new_text: &str| {
            calls.push((previous.map(str::to_string), new_text.to_string()));
            format!("[{}]+({new_text})", previous.unwrap_or(""))
        }
    }

    #[test]
    fn a_window_at_the_top_of_the_book_has_no_summary_and_costs_nothing() {
        let mut cache = RollingSummaryCache::new();
        let mut calls = Vec::new();
        let mut summarize = recording(&mut calls);
        assert_eq!(cache.summary_before(0, &snapshot(), &mut summarize), None);
        drop(summarize);
        assert!(calls.is_empty(), "no summarizer call for an empty prefix");
    }

    #[test]
    fn the_first_request_summarizes_the_whole_prefix_from_scratch() {
        let mut cache = RollingSummaryCache::new();
        let mut calls = Vec::new();
        let mut summarize = recording(&mut calls);
        let summary = cache
            .summary_before(3, &snapshot(), &mut summarize)
            .expect("non-zero prefix")
            .to_string();
        drop(summarize);
        assert_eq!(summary, "[]+(s0\n\ns1\n\ns2)");
        assert_eq!(
            calls,
            vec![(None, "s0\n\ns1\n\ns2".to_string())],
            "one call, previous=None, delta = exactly the prefix segments"
        );
    }

    #[test]
    fn a_rerequest_within_the_session_is_a_cache_hit_with_zero_recompute() {
        let mut cache = RollingSummaryCache::new();
        let mut calls = Vec::new();
        let mut summarize = recording(&mut calls);
        let first = cache
            .summary_before(3, &snapshot(), &mut summarize)
            .expect("cached")
            .to_string();
        let again = cache
            .summary_before(3, &snapshot(), &mut summarize)
            .expect("cached")
            .to_string();
        drop(summarize);
        assert_eq!(first, again);
        assert_eq!(
            calls.len(),
            1,
            "the re-request must be a hit: a second summarizer call is a session-LLM \
             request the cache exists to avoid"
        );
    }

    #[test]
    fn advancing_extends_the_cached_prefix_instead_of_starting_over() {
        let mut cache = RollingSummaryCache::new();
        let mut calls = Vec::new();
        let mut summarize = recording(&mut calls);
        cache.summary_before(3, &snapshot(), &mut summarize);
        let advanced = cache
            .summary_before(5, &snapshot(), &mut summarize)
            .expect("cached")
            .to_string();
        drop(summarize);
        assert_eq!(
            advanced, "[[]+(s0\n\ns1\n\ns2)]+(s3\n\ns4)",
            "the boundary-5 summary is built ON the boundary-3 summary"
        );
        assert_eq!(calls.len(), 2, "advance costs exactly one extension call");
        assert_eq!(
            calls[1],
            (
                Some("[]+(s0\n\ns1\n\ns2)".to_string()),
                "s3\n\ns4".to_string()
            ),
            "the extension call receives the cached summary and ONLY the delta segments"
        );
    }

    #[test]
    fn a_seek_below_every_cached_prefix_recomputes_honestly_then_caches() {
        let mut cache = RollingSummaryCache::new();
        let mut calls = Vec::new();
        let mut summarize = recording(&mut calls);
        cache.summary_before(4, &snapshot(), &mut summarize);
        let rewound = cache
            .summary_before(2, &snapshot(), &mut summarize)
            .expect("non-zero prefix")
            .to_string();
        let rewound_again = cache
            .summary_before(2, &snapshot(), &mut summarize)
            .expect("cached")
            .to_string();
        drop(summarize);
        assert_eq!(
            rewound, "[]+(s0\n\ns1)",
            "a longer summary cannot be unsummarized"
        );
        assert_eq!(rewound, rewound_again);
        assert_eq!(
            calls.len(),
            2,
            "one for boundary 4, one for the rewind; the rewind \
             re-request was a hit"
        );
    }

    #[test]
    fn the_delta_crossing_the_summarizer_seam_is_hard_capped() {
        // Test-local literal, matching the selector's pinned cap.
        const CAP: usize = 65536;
        let oversized = "x".repeat(CAP + 7);
        let snapshot = SessionSnapshot::from_test_parts(vec![
            (1, 0, 1000, oversized.as_str()),
            (1, 1000, 2000, "next"),
        ]);
        let mut cache = RollingSummaryCache::new();
        let seen_len = std::cell::Cell::new(0usize);
        let mut summarize = |_: Option<&str>, new_text: &str| {
            seen_len.set(new_text.len());
            "summary".to_string()
        };
        cache.summary_before(1, &snapshot, &mut summarize);
        assert_eq!(
            seen_len.get(),
            CAP,
            "the summarizer is a book-text egress seam and gets the same final backstop \
             every excerpt gets — never the raw uncapped delta"
        );
    }

    #[test]
    fn a_boundary_past_the_end_clamps_to_the_whole_book() {
        let mut cache = RollingSummaryCache::new();
        let mut calls = Vec::new();
        let mut summarize = recording(&mut calls);
        let all = cache
            .summary_before(99, &snapshot(), &mut summarize)
            .expect("non-zero prefix")
            .to_string();
        let clamped = cache
            .summary_before(6, &snapshot(), &mut summarize)
            .expect("cached")
            .to_string();
        drop(summarize);
        assert_eq!(
            all, clamped,
            "99 and 6 are the same prefix of a 6-segment book"
        );
        assert_eq!(calls.len(), 1, "and therefore the same cache entry");
    }
}
