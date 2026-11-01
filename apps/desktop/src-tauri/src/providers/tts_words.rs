//! Native-timing validation for cloud TTS (charter §3.5, A21): provider
//! word timestamps are admitted into the versioned words envelope only when
//! the WHOLE set conforms — nonconforming, nonmonotonic, or
//! text-incompatible timing rejects the entire set and the segment falls
//! back to local align. Timing sources are never mixed within a segment,
//! and empty/malformed native timing never masquerades as aligned.
//!
//! The envelope word shape is the app's existing `{w, s, e}` (word, start
//! ms, end ms) consumed by `crate::words`; adapters normalize their wire
//! shapes into it before this gate.

/// Why a native timing set was rejected — logged and reported, never acted
/// on beyond the whole-set fallback (the remedy is always the same: local
/// align).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimingRejection(pub String);

fn text_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| {
            token
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// Longest common subsequence length between two token sequences — the
/// standard ordered-alignment measure (the same monotonic-matching shape
/// under WER/edit-distance scoring, Wagner–Fischer/Needleman–Wunsch class,
/// which forced-alignment evaluation uses to align hypothesis words to a
/// reference transcript IN ORDER). Chosen over bag-of-words membership
/// because membership admits reordered or repeated provider words as if
/// they timed this transcript; an ordered subsequence cannot. O(n·m) is
/// fine at sentence scale.
fn lcs_len(provider: &[String], transcript: &[String]) -> usize {
    let mut prev = vec![0usize; transcript.len() + 1];
    let mut row = vec![0usize; transcript.len() + 1];
    for p in provider {
        for (j, t) in transcript.iter().enumerate() {
            row[j + 1] = if p == t {
                prev[j] + 1
            } else {
                row[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[transcript.len()]
}

/// Words may co-articulate slightly at boundaries; overlap past this is a
/// different word's audio. Owner-reviewable constant, documented tolerance.
pub const OVERLAP_TOLERANCE_MS: i64 = 20;
/// The final word may end fractionally past the decoded duration through
/// rounding/trailing-silence trims; past this it is timing for audio that
/// does not exist. Owner-reviewable constant, documented tolerance.
pub const DURATION_TOLERANCE_MS: i64 = 250;

/// Validate one segment's normalized native words against the segment text
/// and the MEASURED audio duration.
///
/// Conformance (all required, whole-set — charter A21):
/// - non-empty set; every entry carries a non-blank `w`, integer `s`/`e` ms;
/// - `s <= e` within a word, starts never regress, and consecutive words do
///   not overlap past `OVERLAP_TOLERANCE_MS`;
/// - the final word ends within `DURATION_TOLERANCE_MS` of the decoded WAV
///   duration — timing beyond the audio is somebody else's audio;
/// - text compatibility, ORDER- and MULTIPLICITY-AWARE, BOTH DIRECTIONS:
///   the longest common subsequence of provider tokens against transcript
///   tokens (each transcript token consumable once) must cover ≥80% of the
///   provider words AND ≥80% of the transcript tokens. The provider-side
///   floor rejects reordered/repeated/foreign words; the transcript-side
///   floor rejects a short subset passing while most of the text has no
///   timing. (The 80% floors tolerate normalization drift on a minority of
///   tokens and remain the owner-flagged constants.)
pub fn validate_native_words(
    words: &[serde_json::Value],
    segment_text: &str,
    audio_duration_ms: i64,
) -> Result<(), TimingRejection> {
    if words.is_empty() {
        return Err(TimingRejection("provider returned no words".to_string()));
    }
    let tokens = text_tokens(segment_text);
    let mut previous_start: i64 = -1;
    let mut previous_end: i64 = 0;
    let mut provider_tokens: Vec<String> = Vec::with_capacity(words.len());
    for (idx, word) in words.iter().enumerate() {
        let w = word
            .get("w")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .ok_or_else(|| TimingRejection(format!("word {idx} has no text")))?;
        let s = word
            .get("s")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| TimingRejection(format!("word {idx} has no integer start")))?;
        let e = word
            .get("e")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| TimingRejection(format!("word {idx} has no integer end")))?;
        if s < 0 || e < s {
            return Err(TimingRejection(format!(
                "word {idx} has an impossible span {s}..{e}"
            )));
        }
        if s < previous_start {
            return Err(TimingRejection(format!(
                "word {idx} starts at {s}ms, before its predecessor at {previous_start}ms — \
                 nonmonotonic timing"
            )));
        }
        if s + OVERLAP_TOLERANCE_MS < previous_end {
            return Err(TimingRejection(format!(
                "word {idx} starts at {s}ms, overlapping its predecessor's end at \
                 {previous_end}ms beyond the {OVERLAP_TOLERANCE_MS}ms tolerance"
            )));
        }
        previous_start = s;
        previous_end = previous_end.max(e);
        provider_tokens.push(
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase(),
        );
    }
    if previous_end > audio_duration_ms + DURATION_TOLERANCE_MS {
        return Err(TimingRejection(format!(
            "timing ends at {previous_end}ms but the decoded audio is {audio_duration_ms}ms \
             (+{DURATION_TOLERANCE_MS}ms tolerance) — timing beyond the audio"
        )));
    }
    let aligned = lcs_len(&provider_tokens, &tokens);
    if aligned * 5 < words.len() * 4 {
        return Err(TimingRejection(format!(
            "only {aligned} of {} provider words align in order with the segment text — \
             text-incompatible timing",
            words.len()
        )));
    }
    if aligned * 5 < tokens.len() * 4 {
        return Err(TimingRejection(format!(
            "the aligned words cover only {aligned} of the transcript's {} tokens — a \
             subset cannot stand in as the whole segment's timing",
            tokens.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn word(w: &str, s: i64, e: i64) -> serde_json::Value {
        json!({"w": w, "s": s, "e": e})
    }

    #[test]
    fn a_conforming_set_is_admitted_whole() {
        let words = vec![word("Hello", 0, 400), word("world", 450, 900)];
        assert!(validate_native_words(&words, "Hello world.", 10_000).is_ok());
    }

    #[test]
    fn every_nonconforming_shape_rejects_the_whole_set() {
        let text = "Hello world.";
        for (label, words) in [
            ("empty", vec![]),
            ("no text", vec![json!({"s": 0, "e": 1})]),
            ("blank text", vec![word("  ", 0, 1)]),
            ("no start", vec![json!({"w": "Hello", "e": 1})]),
            ("no end", vec![json!({"w": "Hello", "s": 0})]),
            ("negative", vec![word("Hello", -1, 5)]),
            ("end before start", vec![word("Hello", 100, 50)]),
            (
                "nonmonotonic",
                vec![word("Hello", 500, 900), word("world", 100, 400)],
            ),
        ] {
            assert!(
                validate_native_words(&words, text, 10_000).is_err(),
                "{label} must reject the whole set"
            );
        }
    }

    #[test]
    fn timing_for_text_the_segment_does_not_contain_is_rejected() {
        let words = vec![
            word("completely", 0, 300),
            word("different", 300, 600),
            word("transcript", 600, 900),
            word("entirely", 900, 1200),
            word("unrelated", 1200, 1500),
        ];
        assert!(validate_native_words(&words, "Hello world.", 10_000).is_err());
    }

    #[test]
    fn punctuation_and_case_do_not_break_text_compatibility() {
        let words = vec![
            word("HELLO,", 0, 300),
            word("world!", 300, 600),
            word("(again)", 600, 900),
        ];
        assert!(validate_native_words(&words, "hello WORLD again.", 10_000).is_ok());
    }

    #[test]
    fn reordered_provider_words_are_rejected_not_admitted() {
        // Bag-of-words membership would pass this at 100%; the ordered gate
        // sees an LCS of 1 of 2 and rejects.
        let words = vec![word("world", 0, 400), word("Hello", 450, 900)];
        assert!(validate_native_words(&words, "Hello world.", 10_000).is_err());
    }

    #[test]
    fn repeated_provider_words_cannot_impersonate_the_transcript() {
        // Five copies of a word the transcript has once: membership scores
        // 5/5, the ordered subsequence scores 1/5.
        let words = vec![
            word("Hello", 0, 100),
            word("Hello", 100, 200),
            word("Hello", 200, 300),
            word("Hello", 300, 400),
            word("Hello", 400, 500),
        ];
        assert!(validate_native_words(&words, "Hello world.", 10_000).is_err());
        // While a transcript that legitimately repeats a token still admits
        // repeated provider words in order.
        let repeated = vec![word("the", 0, 100), word("the", 100, 200)];
        assert!(validate_native_words(&repeated, "the the", 10_000).is_ok());
    }

    #[test]
    fn a_subset_covering_little_of_the_transcript_is_rejected() {
        // Both words align perfectly, but the transcript has eight tokens:
        // the transcript-side coverage floor rejects partial timing.
        let words = vec![word("one", 0, 100), word("two", 100, 200)];
        assert!(
            validate_native_words(&words, "one two three four five six seven eight", 10_000)
                .is_err()
        );
    }

    #[test]
    fn overlap_beyond_tolerance_is_rejected_and_within_it_admitted() {
        let overlapping = vec![word("Hello", 0, 500), word("world", 300, 900)];
        assert!(validate_native_words(&overlapping, "Hello world.", 10_000).is_err());
        // Co-articulation inside the documented tolerance is fine.
        let brushing = vec![word("Hello", 0, 500), word("world", 490, 900)];
        assert!(validate_native_words(&brushing, "Hello world.", 10_000).is_ok());
    }

    #[test]
    fn timing_beyond_the_decoded_audio_is_rejected() {
        let words = vec![word("Hello", 0, 400), word("world", 450, 3_000)];
        assert!(validate_native_words(&words, "Hello world.", 1_000).is_err());
        // Within the documented tolerance is fine.
        let words = vec![word("Hello", 0, 400), word("world", 450, 1_100)];
        assert!(validate_native_words(&words, "Hello world.", 1_000).is_ok());
    }

    #[test]
    fn contractions_and_hyphens_fold_to_matching_tokens() {
        let words = vec![
            word("don't", 0, 300),
            word("re-enter", 300, 700),
            word("well-known", 700, 1_100),
        ];
        assert!(validate_native_words(&words, "Don't re-enter well-known.", 10_000).is_ok());
    }

    #[test]
    fn rtl_scripts_with_whitespace_admit_and_unsegmented_cjk_falls_back() {
        // Arabic is whitespace-separated; alphanumeric folding keeps the
        // letters, so in-order timing admits.
        let rtl = vec![
            word("\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}", 0, 400),
            word("\u{0628}\u{0643}", 450, 800),
        ];
        assert!(validate_native_words(
            &rtl,
            "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627} \u{0628}\u{0643}",
            10_000
        )
        .is_ok());
        // Unsegmented CJK text is one whitespace token; per-word provider
        // tokens cannot align, so the WHOLE set honestly falls back to
        // local align rather than half-matching (A21's safe direction).
        let cjk = vec![
            word("\u{4f60}\u{597d}", 0, 400),
            word("\u{4e16}\u{754c}", 450, 800),
        ];
        assert!(validate_native_words(&cjk, "\u{4f60}\u{597d}\u{4e16}\u{754c}", 10_000).is_err());
    }

    #[test]
    fn a_minority_of_unmatched_words_does_not_reject() {
        // 4 of 5 match: normalization drift on one word is tolerated (the
        // 80% floor), a different transcript is not.
        let words = vec![
            word("one", 0, 100),
            word("two", 100, 200),
            word("three", 200, 300),
            word("four", 300, 400),
            word("4", 400, 500),
        ];
        assert!(validate_native_words(&words, "one two three four four", 10_000).is_ok());
    }
}
