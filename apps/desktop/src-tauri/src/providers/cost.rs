//! Static cost estimation for cloud cast phases (charter §5.2, A9/A10/A11;
//! Hermes MF6 as re-adjudicated in PROVIDER REVIEW 2).
//!
//! The rate source is ONE static catalog
//! (`fixtures/providers/rate_catalog.json`), hand-copied from provider
//! pricing pages on its `as_of` date, never fetched live, and pinned by an
//! EXACT digest test — editing a rate means deliberately moving the pin in
//! the same diff. Estimate truth is three-valued: Known (rate present and
//! fresh), Stale (rate present, catalog old — shown, visibly stale),
//! Unknown (no rate — the exact copy is "Cost unavailable from this app -
//! check your provider"). A missing rate NEVER renders as zero or Free
//! (T12). Currency is USD only (A11).
//!
//! Script estimates model the ACTUAL Teacher contract (A10): output tokens
//! = input × `TEACHER_OUTPUT_RATIO` (1.5, the contract's own constant),
//! with the high bound carrying a documented 1.3× allowance for the repair
//! ladder (≤3 attempts on malformed chunks) and rolling-summary calls —
//! an approximate RANGE, not a point. TTS Known rates (`tts-1`/`tts-1-hd`
//! official per-character pricing) bill GENERATED-script characters: equal
//! to source for Narrator, source × the Teacher ratio as the high bound
//! for Teacher mode.

use serde::{Deserialize, Serialize};

use crate::providers::script::teacher::TEACHER_OUTPUT_RATIO;

pub const RATE_CATALOG_JSON: &str = include_str!("../../fixtures/providers/rate_catalog.json");
/// Past this age the catalog still shows its numbers but wears the Stale
/// truth state (A9: update trust is visible, not assumed).
pub const STALE_AFTER_DAYS: i64 = 180;
const CHARS_PER_TOKEN: f64 = 4.0;
/// Documented high-bound allowance for the Teacher repair ladder and
/// rolling-summary calls on top of the single-pass token arithmetic.
const REPAIR_SUMMARY_ALLOWANCE: f64 = 1.3;

#[derive(Debug, Clone, Deserialize)]
struct ScriptRate {
    provider: String,
    model: String,
    input_usd_per_mtok: f64,
    output_usd_per_mtok: f64,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    valid_until: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TtsRate {
    provider: String,
    model: String,
    usd_per_mchar: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct Catalog {
    as_of: String,
    currency: String,
    script_rates: Vec<ScriptRate>,
    tts_rates: Vec<TtsRate>,
}

fn catalog() -> Catalog {
    serde_json::from_str(RATE_CATALOG_JSON).expect("the bundled rate catalog parses")
}

/// The three-valued cost truth (T12): a range with its basis, the same
/// visibly stale, or an honest Unknown. Never zero-by-default.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CostEstimate {
    Known {
        usd_low: f64,
        usd_high: f64,
        currency: String,
        as_of: String,
        basis: String,
    },
    Stale {
        usd_low: f64,
        usd_high: f64,
        currency: String,
        as_of: String,
        basis: String,
    },
    Unknown {
        message: String,
    },
}

fn unknown() -> CostEstimate {
    CostEstimate::Unknown {
        message: "Cost unavailable from this app - check your provider".to_string(),
    }
}

fn priced(
    catalog: &Catalog,
    usd_low: f64,
    usd_high: f64,
    basis: String,
    today_iso: &str,
    rate_date: Option<&str>,
    valid_until: Option<&str>,
) -> CostEstimate {
    let as_of = rate_date.unwrap_or(&catalog.as_of);
    if catalog_is_stale(as_of, today_iso) || valid_until.is_some_and(|limit| today_iso > limit) {
        CostEstimate::Stale {
            usd_low,
            usd_high,
            currency: catalog.currency.clone(),
            as_of: as_of.to_string(),
            basis,
        }
    } else {
        CostEstimate::Known {
            usd_low,
            usd_high,
            currency: catalog.currency.clone(),
            as_of: as_of.to_string(),
            basis,
        }
    }
}

fn catalog_is_stale(as_of: &str, today: &str) -> bool {
    fn ordinal(date: &str) -> Option<i64> {
        let mut parts = date.split('-');
        let year: i64 = parts.next()?.parse().ok()?;
        let month: i64 = parts.next()?.parse().ok()?;
        let day: i64 = parts.next()?.parse().ok()?;
        Some(year * 365 + month * 30 + day)
    }
    match (ordinal(as_of), ordinal(today)) {
        (Some(from), Some(now)) => now - from > STALE_AFTER_DAYS,
        _ => true,
    }
}

/// Estimate a cloud Script phase from the AUTHORITATIVE selected-scope
/// character count (derived by the writer from stored pages, never
/// caller-supplied).
pub fn estimate_script(
    provider_id: &str,
    model: &str,
    source_chars: u64,
    today_iso: &str,
) -> CostEstimate {
    let catalog = catalog();
    let Some(rate) = catalog
        .script_rates
        .iter()
        .find(|rate| rate.provider == provider_id && rate.model == model)
    else {
        return unknown();
    };
    let input_mtok = source_chars as f64 / CHARS_PER_TOKEN / 1_000_000.0;
    let output_mtok = input_mtok * TEACHER_OUTPUT_RATIO;
    let single_pass = input_mtok * rate.input_usd_per_mtok + output_mtok * rate.output_usd_per_mtok;
    let basis = format!(
        "~{source_chars} scoped source characters at ~4 chars/token; output = input x \
         {TEACHER_OUTPUT_RATIO} (the Teacher contract's ratio); high bound adds a \
         {REPAIR_SUMMARY_ALLOWANCE}x repair/summary allowance; excludes provider \
         taxes/fees/discounts; approximate"
    );
    priced(
        &catalog,
        single_pass,
        single_pass * REPAIR_SUMMARY_ALLOWANCE,
        basis,
        today_iso,
        rate.as_of.as_deref(),
        rate.valid_until.as_deref(),
    )
}

/// Estimate a cloud TTS phase from GENERATED-script characters: equal to
/// the source for Narrator; up to source × the Teacher ratio for Teacher
/// mode (the script does not exist yet at cast time, so the range is the
/// honest answer). Only models with an official per-character rate render
/// Known — `gpt-4o-mini-tts` (audio-output-token billed) and Cartesia
/// (plan credits) stay honestly Unknown.
pub fn estimate_tts(
    provider_id: &str,
    model: &str,
    source_chars: u64,
    teacher_mode: bool,
    today_iso: &str,
) -> CostEstimate {
    let catalog = catalog();
    let Some(rate) = catalog
        .tts_rates
        .iter()
        .find(|rate| rate.provider == provider_id && rate.model == model)
    else {
        return unknown();
    };
    let chars_low = source_chars as f64;
    let chars_high = if teacher_mode {
        source_chars as f64 * TEACHER_OUTPUT_RATIO
    } else {
        chars_low
    };
    let basis = format!(
        "script characters billed at the provider's published per-character rate; \
         {} the script is generated from ~{source_chars} scoped source characters; \
         excludes provider taxes/fees/discounts; approximate",
        if teacher_mode {
            "Teacher mode may expand text up to the contract ratio before"
        } else {
            "Narrator mode reads the source, so"
        }
    );
    priced(
        &catalog,
        chars_low / 1_000_000.0 * rate.usd_per_mchar,
        chars_high / 1_000_000.0 * rate.usd_per_mchar,
        basis,
        today_iso,
        None,
        None,
    )
}

/// The per-job snapshot the cast writer persists (pre-brief item 1).
#[derive(Debug, Clone, Serialize)]
pub struct CostSnapshot {
    pub phase: String,
    pub provider: String,
    pub model: String,
    pub estimate: CostEstimate,
    pub catalog_sha256: String,
}

/// Today's ISO date from unix seconds (Howard Hinnant's civil_from_days).
pub fn iso_date_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// The digest of the catalog's canonical LF text: a build from a CRLF
/// checkout (Windows with `core.autocrlf`) embeds different bytes, and the
/// digest recorded with every cast plan must not depend on which machine
/// compiled the binary.
pub fn catalog_sha256() -> String {
    use sha2::Digest;
    let canonical = RATE_CATALOG_JSON.replace("\r\n", "\n");
    format!("{:x}", sha2::Sha256::digest(canonical.as_bytes()))
}

pub fn pricing_provider(record: &super::ConnectionRecord) -> &'static str {
    match super::directory::identity(record) {
        "openai" => "openai_compatible",
        "cartesia" => "cartesia_tts",
        identity => identity,
    }
}

pub fn reviewed_input_price(provider: &str, model: &str) -> Option<f64> {
    let provider = if provider == "openai" {
        "openai_compatible"
    } else {
        provider
    };
    catalog()
        .script_rates
        .into_iter()
        .find(|rate| rate.provider == provider && rate.model == model)
        .map(|rate| rate.input_usd_per_mtok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE integrity pin (Hermes R2 fold-in 5): any catalog edit fails here
    /// until this constant is deliberately moved in the same diff.
    const PINNED_CATALOG_SHA256: &str =
        "3c88647d906a8bead680e2a81535978ae2a74e47c4bf79bfdd235bd5086a92f9";

    #[test]
    fn the_catalog_digest_is_exactly_pinned() {
        assert_eq!(
            catalog_sha256(),
            PINNED_CATALOG_SHA256,
            "the rate catalog changed; verify every rate against its source and move this \
             pin deliberately in the same diff"
        );
        let parsed = catalog();
        assert_eq!(parsed.currency, "USD");
        assert!(!parsed.script_rates.is_empty());
        assert!(!parsed.tts_rates.is_empty());
    }

    #[test]
    fn a_known_script_rate_estimates_a_contract_true_range() {
        let estimate = estimate_script("anthropic", "claude-sonnet-5", 400_000, "2026-08-28");
        match estimate {
            CostEstimate::Known {
                usd_low,
                usd_high,
                currency,
                as_of,
                basis,
            } => {
                // 100k tokens in at $2/MTok + 150k out at $10/MTok = $1.70;
                // high bound x1.3 = $2.21.
                assert!((usd_low - 1.7).abs() < 1e-9, "low {usd_low}");
                assert!((usd_high - 2.21).abs() < 1e-9, "high {usd_high}");
                assert_eq!(currency, "USD");
                assert_eq!(as_of, "2026-08-28");
                assert!(
                    basis.contains("1.5"),
                    "the contract ratio is disclosed: {basis}"
                );
                assert!(basis.contains("approximate"));
            }
            other => panic!("expected Known, got {other:?}"),
        }
    }

    #[test]
    fn official_character_priced_tts_models_render_known_ranges() {
        // tts-1 at $15/1M chars, narrator: 200k chars = $3 flat.
        let narrator = estimate_tts("openai_tts", "tts-1", 200_000, false, "2026-08-28");
        match narrator {
            CostEstimate::Known {
                usd_low, usd_high, ..
            } => {
                assert!((usd_low - 3.0).abs() < 1e-9);
                assert!(
                    (usd_high - 3.0).abs() < 1e-9,
                    "narrator is a point, not a range"
                );
            }
            other => panic!("expected Known, got {other:?}"),
        }
        // Teacher mode widens the high bound by the contract ratio.
        let teacher = estimate_tts("openai_tts", "tts-1-hd", 200_000, true, "2026-08-28");
        match teacher {
            CostEstimate::Known {
                usd_low,
                usd_high,
                basis,
                ..
            } => {
                assert!((usd_low - 6.0).abs() < 1e-9);
                assert!((usd_high - 9.0).abs() < 1e-9);
                assert!(basis.contains("Teacher"));
            }
            other => panic!("expected Known, got {other:?}"),
        }
    }

    #[test]
    fn output_token_and_credit_priced_tts_stays_honestly_unknown() {
        for (provider, model) in [
            ("openai_tts", "gpt-4o-mini-tts"),
            ("cartesia_tts", "sonic-3.6"),
        ] {
            assert!(matches!(
                estimate_tts(provider, model, 50_000, false, "2026-08-28"),
                CostEstimate::Unknown { .. }
            ));
        }
    }

    #[test]
    fn an_unknown_model_says_exactly_the_unavailable_line_never_zero() {
        assert_eq!(
            estimate_script("anthropic", "some-unlisted-model", 1000, "2026-08-28"),
            CostEstimate::Unknown {
                message: "Cost unavailable from this app - check your provider".to_string()
            }
        );
    }

    #[test]
    fn unix_seconds_produce_civil_dates() {
        assert_eq!(iso_date_from_unix(0), "1970-01-01");
        assert_eq!(iso_date_from_unix(1_787_875_200), "2026-08-28");
    }

    #[test]
    fn an_old_catalog_reads_stale_not_fresh() {
        assert!(matches!(
            estimate_script("anthropic", "claude-sonnet-5", 4_000, "2027-06-01"),
            CostEstimate::Stale { .. }
        ));
    }
}
