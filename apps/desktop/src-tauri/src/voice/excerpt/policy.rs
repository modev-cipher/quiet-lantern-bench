//! Per-provider truncation policy (VOICE_AGENT §4: "caps are tokenizer-measured
//! per provider — byte caps are unsafe for multilingual text").
//!
//! The policy is **provider-shaped, never provider-named**: core code holds a
//! token budget and a counting function and has no idea whose tokenizer is
//! counting. Each channel adapter (VA5) ships the policy in its channel config —
//! its provider's excerpt budget and its provider's tokenizer — and this module
//! applies it identically for all of them.
//!
//! Truncation is **loud, never silent**: every application that drops content
//! emits an [`EgressEvent`] into the caller's event sink *and* marks the
//! selection result. A truncated excerpt cannot reach a channel without both
//! signals existing, so content loss is always observable in the session's
//! event stream.

/// A provider's excerpt budget plus its own way of counting. Lives with the
/// channel config; constructed by channel adapters and test rigs only.
pub struct TruncationPolicy {
    max_excerpt_tokens: usize,
    count_tokens: Box<dyn Fn(&str) -> usize + Send + Sync>,
}

impl TruncationPolicy {
    pub fn new(
        max_excerpt_tokens: usize,
        count_tokens: impl Fn(&str) -> usize + Send + Sync + 'static,
    ) -> Self {
        Self {
            max_excerpt_tokens,
            count_tokens: Box::new(count_tokens),
        }
    }

    pub fn max_excerpt_tokens(&self) -> usize {
        self.max_excerpt_tokens
    }

    pub fn count(&self, text: &str) -> usize {
        (self.count_tokens)(text)
    }

    pub fn fits(&self, text: &str) -> bool {
        self.count(text) <= self.max_excerpt_tokens
    }

    /// The largest char-boundary prefix of `text` the budget accepts. Binary
    /// search over char boundaries: correct for any monotonic counter, and a
    /// bounded number of counting calls for tokenizers that charge per call.
    pub fn largest_fitting_prefix<'t>(&self, text: &'t str) -> &'t str {
        self.largest_prefix_within(text, self.max_excerpt_tokens)
    }

    /// [`TruncationPolicy::largest_fitting_prefix`] generalized to an
    /// arbitrary token `budget` (F8-D1b: the range fetch cuts its chunks to
    /// HALF the excerpt budget).
    pub fn largest_prefix_within<'t>(&self, text: &'t str, budget: usize) -> &'t str {
        if self.count(text) <= budget {
            return text;
        }
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(at, _)| at)
            .chain(std::iter::once(text.len()))
            .collect();
        // Invariant: prefix at boundaries[low] fits, at boundaries[high] does not.
        let (mut low, mut high) = (0usize, boundaries.len() - 1);
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if self.count(&text[..boundaries[mid]]) <= budget {
                low = mid;
            } else {
                high = mid;
            }
        }
        &text[..boundaries[low]]
    }

    /// The suffix twin of [`TruncationPolicy::largest_prefix_within`]: the
    /// largest char-boundary SUFFIX of `text` within `budget` tokens (F8-D1b:
    /// the tail chunk of a split range carries the range's true end).
    pub fn largest_suffix_within<'t>(&self, text: &'t str, budget: usize) -> &'t str {
        if self.count(text) <= budget {
            return text;
        }
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(at, _)| at)
            .chain(std::iter::once(text.len()))
            .collect();
        // Invariant: suffix at boundaries[high] fits (the empty suffix always
        // does), at boundaries[low] does not (the whole text — checked above).
        let (mut low, mut high) = (0usize, boundaries.len() - 1);
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if self.count(&text[boundaries[mid]..]) <= budget {
                high = mid;
            } else {
                low = mid;
            }
        }
        &text[boundaries[high]..]
    }
}

/// What happened to the excerpt on its way out. Consumed by the session event
/// stream (surfaced by the Phase 5 UI task); emitted only from the excerpt
/// module — nothing else has content loss to report.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressEvent {
    /// The provider token budget dropped window content.
    ExcerptTruncated {
        budget_tokens: usize,
        window_tokens: usize,
        dropped_leading_segments: usize,
        dropped_trailing_segments: usize,
        anchor_partially_cut: bool,
    },
    /// The final backstop fired (should be unreachable below a sane token
    /// budget — loud precisely because it means the budget arithmetic is off).
    HardByteCapApplied {
        kept_bytes: usize,
        dropped_bytes: usize,
    },
}

/// The result-borne twin of [`EgressEvent::ExcerptTruncated`]: rides the
/// selection so the immediate caller cannot miss it even if it drops the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationNotice {
    pub budget_tokens: usize,
    pub window_tokens: usize,
    pub dropped_leading_segments: usize,
    pub dropped_trailing_segments: usize,
    pub anchor_partially_cut: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn per_char() -> TruncationPolicy {
        TruncationPolicy::new(4, |text: &str| text.chars().count())
    }

    #[test]
    fn a_fitting_text_is_returned_whole() {
        assert_eq!(per_char().largest_fitting_prefix("abcd"), "abcd");
        assert_eq!(per_char().largest_fitting_prefix(""), "");
    }

    #[test]
    fn an_oversized_text_is_cut_to_the_budget_exactly() {
        assert_eq!(per_char().largest_fitting_prefix("abcdefgh"), "abcd");
    }

    #[test]
    fn the_cut_lands_on_char_boundaries_for_multibyte_text() {
        // Five 3-byte chars: the budget is 4 tokens, so 4 chars = 12 bytes.
        assert_eq!(per_char().largest_fitting_prefix("語語語語語"), "語語語語");
    }

    #[test]
    fn a_zero_budget_yields_an_empty_prefix() {
        let policy = TruncationPolicy::new(0, |text: &str| text.chars().count());
        assert_eq!(policy.largest_fitting_prefix("abc"), "");
    }

    #[test]
    fn suffix_cuts_keep_the_text_end_within_an_arbitrary_budget() {
        let policy = TruncationPolicy::new(4, |text: &str| text.chars().count());
        assert_eq!(policy.largest_suffix_within("abcdefgh", 3), "fgh");
        assert_eq!(policy.largest_suffix_within("abc", 5), "abc");
        assert_eq!(
            policy.largest_suffix_within("語語語語語", 2),
            "語語",
            "the cut lands on char boundaries for multibyte text"
        );
        assert_eq!(policy.largest_suffix_within("abc", 0), "");
    }

    #[test]
    fn prefix_cuts_honor_an_arbitrary_budget_below_the_policy_max() {
        let policy = TruncationPolicy::new(100, |text: &str| text.chars().count());
        assert_eq!(policy.largest_prefix_within("abcdefgh", 3), "abc");
        assert_eq!(policy.largest_prefix_within("ab", 3), "ab");
    }

    #[test]
    fn a_word_grained_counter_never_splits_below_its_own_grain_mid_count() {
        // A counter that charges per whitespace-separated word: the prefix cut
        // must still respect the budget under that counter's own arithmetic.
        let policy = TruncationPolicy::new(2, |text: &str| text.split_whitespace().count());
        let cut = policy.largest_fitting_prefix("alpha beta gamma");
        assert!(policy.fits(cut));
        assert_eq!(
            cut, "alpha beta ",
            "the largest boundary where the word counter still reads 2"
        );
    }
}
