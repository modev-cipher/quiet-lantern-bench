use sqlx::SqlitePool;
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

use super::state::OrchestratorError;
use crate::storage::job;

pub const THRESHOLD: f64 = 0.15;
pub const FLAG_VERIFY_FAILED: &str = "verify_failed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Ok,
    Failed,
    Missing,
    UnscoreableRef,
    Unchecked,
}

impl Band {
    pub fn as_str(self) -> &'static str {
        match self {
            Band::Ok => "ok",
            Band::Failed => "failed",
            Band::Missing => "missing",
            Band::UnscoreableRef => "unscoreable_ref",
            Band::Unchecked => "unchecked",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub band: Band,
    pub wer: Option<f64>,
    pub reference_words: usize,
    pub hypothesis_words: usize,
}

pub fn normalize(text: &str) -> Vec<String> {
    text.nfkc()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' => '\'',
            other => other,
        })
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '\'' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .map(|token| token.trim_matches('\'').to_string())
        .filter(|token| !token.is_empty())
        .collect()
}

pub fn word_error_rate(reference: &[String], hypothesis: &[String]) -> Option<f64> {
    if reference.is_empty() {
        return None;
    }
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0usize; hypothesis.len() + 1];
    for (i, reference_word) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, hypothesis_word) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(reference_word != hypothesis_word);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    Some(previous[hypothesis.len()] as f64 / reference.len() as f64)
}

pub fn judge(reference_text: &str, hypothesis_text: &str) -> Verdict {
    let reference = normalize(reference_text);
    let hypothesis = normalize(hypothesis_text);
    let Some(rate) = word_error_rate(&reference, &hypothesis) else {
        return Verdict {
            band: Band::UnscoreableRef,
            wer: None,
            reference_words: 0,
            hypothesis_words: hypothesis.len(),
        };
    };
    if hypothesis.is_empty() {
        return Verdict {
            band: Band::Missing,
            wer: None,
            reference_words: reference.len(),
            hypothesis_words: 0,
        };
    }
    Verdict {
        band: if rate > THRESHOLD {
            Band::Failed
        } else {
            Band::Ok
        },
        wer: Some(rate),
        reference_words: reference.len(),
        hypothesis_words: hypothesis.len(),
    }
}

pub async fn score_segment(
    pool: &SqlitePool,
    segment: &job::ScriptSegment,
    hypothesis_text: &str,
) -> Result<Verdict, OrchestratorError> {
    let verdict = judge(&segment.text, hypothesis_text);
    if let Some(mut alignment) = job::get_alignment(pool, &segment.id).await? {
        alignment.wer = verdict.wer;
        job::upsert_alignment(pool, &alignment).await?;
    }
    if verdict.band == Band::Failed {
        let mut flags: Vec<String> = segment
            .flags_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        if !flags.iter().any(|flag| flag == FLAG_VERIFY_FAILED) {
            flags.push(FLAG_VERIFY_FAILED.to_string());
            job::set_segment_flags(pool, &segment.id, &serde_json::to_string(&flags)?).await?;
        }
    }
    Ok(verdict)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    Off,
    Scored,
    NothingScored { scoreable: i64 },
}

impl VerifyOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            VerifyOutcome::Off => "off",
            VerifyOutcome::Scored => "scored",
            VerifyOutcome::NothingScored { .. } => "nothing_scored",
        }
    }

    pub fn scored_nothing(self) -> bool {
        matches!(self, VerifyOutcome::NothingScored { .. })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    pub ok: i64,
    pub failed: i64,
    pub missing: i64,
    pub unscoreable_ref: i64,
    pub unchecked: i64,
    pub worst: Vec<serde_json::Value>,
}

pub const WORST_LIMIT: usize = 20;

impl Tally {
    pub fn record(&mut self, segment: &job::ScriptSegment, verdict: &Verdict) {
        match verdict.band {
            Band::Ok => self.ok += 1,
            Band::Failed => self.failed += 1,
            Band::Missing => self.missing += 1,
            Band::UnscoreableRef => self.unscoreable_ref += 1,
            Band::Unchecked => self.unchecked += 1,
        }
        if matches!(verdict.band, Band::Failed | Band::Missing) {
            self.worst.push(serde_json::json!({
                "segment_id": segment.id,
                "chapter_id": segment.chapter_id,
                "idx": segment.idx,
                "band": verdict.band.as_str(),
                "wer": verdict.wer,
                "reference_words": verdict.reference_words,
            }));
        }
    }

    pub fn scored(&self) -> i64 {
        self.ok + self.failed
    }

    pub fn scoreable(&self) -> i64 {
        self.ok + self.failed + self.missing + self.unchecked
    }

    pub fn outcome(&self, enabled: bool) -> VerifyOutcome {
        if !enabled {
            return VerifyOutcome::Off;
        }
        let scoreable = self.scoreable();
        if scoreable > 0 && self.scored() == 0 {
            return VerifyOutcome::NothingScored { scoreable };
        }
        VerifyOutcome::Scored
    }

    pub fn into_report(mut self, enabled: bool) -> serde_json::Value {
        self.worst.sort_by(|a, b| {
            let left = a["wer"].as_f64().unwrap_or(f64::INFINITY);
            let right = b["wer"].as_f64().unwrap_or(f64::INFINITY);
            right.total_cmp(&left)
        });
        self.worst.truncate(WORST_LIMIT);
        let outcome = self.outcome(enabled);
        let scored = self.scored();
        let scoreable = self.scoreable();
        serde_json::json!({
            "enabled": enabled,
            "threshold": THRESHOLD,
            "outcome": outcome.as_str(),
            "scored": scored,
            "scoreable": scoreable,
            "counts": {
                "ok": self.ok,
                "failed": self.failed,
                "missing": self.missing,
                "unscoreable_ref": self.unscoreable_ref,
                "unchecked": self.unchecked,
            },
            "worst": self.worst,
        })
    }
}

pub async fn write_verify_report(
    pool: &SqlitePool,
    job_id: &str,
    verify: bool,
    segments: &[job::ScriptSegment],
    verdicts: &HashMap<String, Verdict>,
) -> Result<VerifyOutcome, OrchestratorError> {
    let mut tally = Tally::default();
    if verify {
        for segment in segments {
            let verdict = match verdicts.get(&segment.id) {
                Some(verdict) => verdict.clone(),
                None => match job::get_alignment(pool, &segment.id)
                    .await?
                    .and_then(|row| row.wer)
                {
                    Some(wer) => Verdict {
                        band: if wer > THRESHOLD {
                            Band::Failed
                        } else {
                            Band::Ok
                        },
                        wer: Some(wer),
                        reference_words: normalize(&segment.text).len(),
                        hypothesis_words: 0,
                    },
                    None => Verdict {
                        band: Band::Unchecked,
                        wer: None,
                        reference_words: normalize(&segment.text).len(),
                        hypothesis_words: 0,
                    },
                },
            };
            tally.record(segment, &verdict);
        }
    }
    if tally.unchecked > 0 {
        tracing::warn!(
            "verify incomplete for job {job_id}: {} of {} sentence(s) were never \
             scored; the accuracy check did not read them back",
            tally.unchecked,
            segments.len()
        );
    }
    let outcome = tally.outcome(verify);
    if outcome.scored_nothing() {
        tracing::error!(
            "verify scored nothing for job {job_id}: 0 of {} scoreable sentence(s) \
             produced a score ({} heard nothing, {} never checked); this report is not \
             evidence that the audio is right",
            tally.scoreable(),
            tally.missing,
            tally.unchecked
        );
    }
    job::merge_report_value(pool, job_id, "verify", tally.into_report(verify)).await?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn no_module_outside_this_one_computes_or_writes_a_wer() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path.strip_prefix(&root).expect("under src").to_path_buf();
                if relative == std::path::Path::new("orchestrator/verify.rs")
                    || relative == std::path::Path::new("storage/job.rs")
                {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read source");
                for (number, line) in body.lines().enumerate() {
                    if line.contains("word_error_rate(") || line.contains(".wer = ") {
                        offenders.push(format!("{}:{}", relative.display(), number + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the word error rate has one implementation; these compute or write it directly: {offenders:?}"
        );
    }

    #[test]
    fn normalize_folds_case_punctuation_and_unicode() {
        assert_eq!(
            normalize("The  QUICK, brown\u{2014}fox\u{2019}s tail!"),
            words(&["the", "quick", "brown", "fox's", "tail"])
        );
        assert_eq!(normalize("ﬁgure ①"), words(&["figure", "1"]));
    }

    #[test]
    fn normalize_is_idempotent() {
        let once = normalize("Hello, world -- again!");
        let twice = normalize(&once.join(" "));
        assert_eq!(once, twice);
    }

    #[test]
    fn word_error_rate_counts_each_edit_kind() {
        assert_eq!(
            word_error_rate(&words(&["a", "b", "c"]), &words(&["a", "b", "c"])),
            Some(0.0)
        );
        assert_eq!(
            word_error_rate(&words(&["a", "b", "c"]), &words(&["a", "x", "c"])),
            Some(1.0 / 3.0)
        );
        assert_eq!(
            word_error_rate(&words(&["a", "b", "c"]), &words(&["a", "c"])),
            Some(1.0 / 3.0)
        );
        assert_eq!(
            word_error_rate(&words(&["a", "b", "c"]), &words(&["a", "b", "x", "c"])),
            Some(1.0 / 3.0)
        );
    }

    #[test]
    fn an_empty_reference_is_never_scored() {
        assert_eq!(word_error_rate(&[], &words(&["all", "right"])), None);
    }

    #[test]
    fn a_quote_only_segment_is_unscoreable_never_nan() {
        let verdict = judge("'", "All right.");
        assert_eq!(verdict.band, Band::UnscoreableRef);
        assert_eq!(verdict.wer, None);
        assert_eq!(verdict.reference_words, 0);
    }

    #[test]
    fn zero_hypothesis_words_is_missing_not_a_total_failure() {
        let verdict = judge("The line rises steadily.", "   ");
        assert_eq!(verdict.band, Band::Missing);
        assert_eq!(verdict.wer, None);
        assert_eq!(verdict.hypothesis_words, 0);
    }

    #[test]
    fn the_threshold_is_exclusive() {
        let reference = "one two three four five six seven eight nine ten \
                         eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen \
                         nineteen twenty";
        let three_wrong = "one two three four five six seven eight nine ten \
                           eleven twelve thirteen fourteen fifteen sixteen seventeen x y z";
        let verdict = judge(reference, three_wrong);
        assert_eq!(verdict.wer, Some(0.15));
        assert_eq!(verdict.band, Band::Ok);

        let four_wrong = "one two three four five six seven eight nine ten \
                          eleven twelve thirteen fourteen fifteen sixteen w x y z";
        assert_eq!(judge(reference, four_wrong).band, Band::Failed);
    }

    #[test]
    fn a_clean_round_trip_from_the_measured_corpus_scores_zero() {
        let text = "We also need a way to measure how steep a line is, \
                    which is crucial for understanding direction or change.";
        assert_eq!(judge(text, text).wer, Some(0.0));
        assert_eq!(judge(text, text).band, Band::Ok);
    }

    #[test]
    fn an_unscoreable_reference_stays_unscoreable_when_nothing_was_heard_either() {
        let verdict = judge("'", "");
        assert_eq!(
            verdict.band,
            Band::UnscoreableRef,
            "the fourth quadrant: empty reference AND empty hypothesis. unscoreable_ref is a \
             property of the reference alone (PIPELINE_SPEC: 'the reference itself normalizes to \
             zero words'), so testing the hypothesis first stole this case into `missing` and \
             invented a sentence the user was told he could not hear"
        );
        assert_eq!(verdict.wer, None);
        assert_eq!(verdict.reference_words, 0);
        assert_eq!(verdict.hypothesis_words, 0);
    }

    #[test]
    fn a_job_of_only_unscoreable_references_does_not_raise_a_scored_nothing_alarm() {
        let segment = job::ScriptSegment {
            id: "s".into(),
            job_id: "j".into(),
            chapter_id: "c".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "'".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        };
        let mut tally = Tally::default();
        tally.record(&segment, &judge("'", ""));
        assert_eq!(tally.missing, 0);
        assert_eq!(tally.unscoreable_ref, 1);
        assert_eq!(
            tally.outcome(true),
            VerifyOutcome::Scored,
            "scoreable() excludes unscoreable_ref precisely so a job of stray-punctuation \
             segments is not a checker failure. Banding this as `missing` put it back in the \
             denominator and raised nothing_scored on a checker that was working fine"
        );
    }

    #[test]
    fn a_checker_that_ran_and_heard_nothing_is_still_missing_never_unchecked() {
        let verdict = judge("The line rises steadily.", "");
        assert_eq!(
            verdict.band,
            Band::Missing,
            "splitting the band must not launder real silence into 'not checked': the checker \
             read this segment back and heard nothing, which is the one thing in this report \
             that is the user's audio problem rather than ours"
        );
    }

    #[test]
    fn an_unchecked_segment_stays_out_of_the_worst_list() {
        let mut tally = Tally::default();
        let segment = job::ScriptSegment {
            id: "s".into(),
            job_id: "j".into(),
            chapter_id: "c".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "x".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        };
        tally.record(
            &segment,
            &Verdict {
                band: Band::Unchecked,
                wer: None,
                reference_words: 1,
                hypothesis_words: 0,
            },
        );
        let report = tally.into_report(true);
        assert_eq!(report["counts"]["unchecked"], 1);
        assert_eq!(
            report["worst"].as_array().expect("worst list").len(),
            0,
            "a checker failure applies to every segment at once, so listing them would bury \
             the handful of sentences the user can actually act on under hundreds he cannot; \
             the count carries this fact instead"
        );
    }

    #[test]
    fn the_report_is_sorted_worst_first_and_capped() {
        let mut tally = Tally::default();
        for idx in 0..(WORST_LIMIT as i64 + 5) {
            let segment = job::ScriptSegment {
                id: format!("s{idx}"),
                job_id: "j".into(),
                chapter_id: "c".into(),
                idx,
                page_no: 1,
                kind: "text".into(),
                figure_id: None,
                text: "x".into(),
                source_span_json: None,
                flags_json: None,
                audio_hash: None,
                duration_ms: None,
                original_text: None,
            };
            tally.record(
                &segment,
                &Verdict {
                    band: Band::Failed,
                    wer: Some(0.2 + idx as f64 / 100.0),
                    reference_words: 10,
                    hypothesis_words: 10,
                },
            );
        }
        let report = tally.into_report(true);
        let worst = report["worst"].as_array().expect("worst list");
        assert_eq!(worst.len(), WORST_LIMIT);
        assert!(worst[0]["wer"].as_f64() > worst[1]["wer"].as_f64());
        assert_eq!(report["counts"]["failed"], WORST_LIMIT as i64 + 5);
        assert_eq!(report["threshold"], THRESHOLD);
    }

    fn tally_of(ok: i64, failed: i64, missing: i64, unscoreable: i64, unchecked: i64) -> Tally {
        Tally {
            ok,
            failed,
            missing,
            unscoreable_ref: unscoreable,
            unchecked,
            worst: Vec::new(),
        }
    }

    #[test]
    fn a_checker_that_scored_none_of_what_it_could_have_scored_is_not_a_pass() {
        let tally = tally_of(0, 0, 469, 0, 0);
        assert_eq!(
            tally.outcome(true),
            VerifyOutcome::NothingScored { scoreable: 469 },
            "this is the production report verbatim: 469 sentences read back, not one scored. \
             Zero-of-zero is not success, and the job reported DONE on exactly this"
        );
        assert_eq!(tally.into_report(true)["outcome"], "nothing_scored");
    }

    #[test]
    fn a_single_score_is_enough_to_be_a_pass() {
        assert_eq!(
            tally_of(1, 0, 468, 0, 0).outcome(true),
            VerifyOutcome::Scored
        );
        assert_eq!(
            tally_of(0, 1, 468, 0, 0).outcome(true),
            VerifyOutcome::Scored
        );
    }

    #[test]
    fn a_report_with_nothing_scoreable_in_it_is_not_a_scoring_failure() {
        let tally = tally_of(0, 0, 0, 7, 0);
        assert_eq!(
            tally.outcome(true),
            VerifyOutcome::Scored,
            "a reference with no words cannot be scored by a healthy checker either, so counting \
             it as scoreable would fail jobs for a defect that is not there"
        );
    }

    #[test]
    fn segments_the_checker_never_read_back_still_count_against_it() {
        assert_eq!(
            tally_of(0, 0, 0, 0, 12).outcome(true),
            VerifyOutcome::NothingScored { scoreable: 12 },
            "'not checked' is still a sentence the user asked to have verified and did not"
        );
    }

    #[test]
    fn a_verify_pass_that_was_switched_off_is_not_a_scoring_failure() {
        assert_eq!(tally_of(0, 0, 0, 0, 0).outcome(false), VerifyOutcome::Off);
        assert_eq!(tally_of(0, 0, 9, 0, 0).outcome(false), VerifyOutcome::Off);
        assert_eq!(tally_of(0, 0, 0, 0, 0).outcome(true), VerifyOutcome::Scored);
    }

    #[test]
    fn the_report_carries_the_scored_and_scoreable_counts_it_was_judged_on() {
        let report = tally_of(2, 1, 4, 3, 5).into_report(true);
        assert_eq!(report["scored"], 3);
        assert_eq!(
            report["scoreable"], 12,
            "ok+failed+missing+unchecked, never unscoreable_ref: {report}"
        );
        assert_eq!(report["outcome"], "scored");
    }

    #[test]
    fn every_band_lands_in_exactly_one_counter() {
        let mut tally = Tally::default();
        let segment = job::ScriptSegment {
            id: "s".into(),
            job_id: "j".into(),
            chapter_id: "c".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "x".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        };
        for band in [
            Band::Ok,
            Band::Failed,
            Band::Missing,
            Band::UnscoreableRef,
            Band::Unchecked,
        ] {
            tally.record(
                &segment,
                &Verdict {
                    band,
                    wer: None,
                    reference_words: 1,
                    hypothesis_words: 1,
                },
            );
        }
        let report = tally.into_report(true);
        let counts = &report["counts"];
        let total: i64 = ["ok", "failed", "missing", "unscoreable_ref", "unchecked"]
            .iter()
            .map(|key| counts[key].as_i64().expect("count"))
            .sum();
        assert_eq!(total, 5);
    }
}
