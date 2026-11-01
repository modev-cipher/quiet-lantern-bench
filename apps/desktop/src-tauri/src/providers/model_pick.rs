//! Provider model listing + the owner's default-model policy (2026-08-28
//! ruling): fetching is EXPLICIT and bounded (T11 — never a background
//! dependency; manual entry stays first-class per T5), and the suggested
//! default per provider is the latest of its workhorse tier — Gemini:
//! latest `-flash`; Anthropic: latest sonnet; OpenAI-compatible: the latest
//! generation, tie-broken toward the cheaper member (rate catalog first,
//! then the provider's own low-cost tier marker in the id).

use crate::providers::registry::ProviderKind;

/// Parse a trailing version from a model id family, e.g.
/// `gemini-3.7-flash` → ("gemini", 3.7, "flash"), `claude-sonnet-5` →
/// 5.0, `luna-5.6-mini` → 5.6. Returns (version, id) when the id matches
/// the wanted shape.
fn version_of(id: &str) -> Option<f64> {
    // The version is the FIRST numeric run after the family words, with
    // dash-joined numeric parts read as one dotted version (Anthropic's
    // `claude-sonnet-4-5` is 4.5, not two numbers), and a numeric prefix
    // accepted before a letter suffix (`gpt-4o` is generation 4).
    let parts: Vec<&str> = id.split(['-', '_']).collect();
    let numeric_prefix = |part: &str| -> Option<String> {
        let digits: String = part
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!digits.is_empty() && digits.parse::<f64>().is_ok()).then_some(digits)
    };
    let start = parts
        .iter()
        .position(|part| numeric_prefix(part).is_some())?;
    let mut joined = numeric_prefix(parts[start])?;
    if !joined.contains('.') {
        if let Some(next) = parts.get(start + 1).and_then(|part| {
            let digits = numeric_prefix(part)?;
            (*part == digits && !digits.contains('.')).then_some(digits)
        }) {
            joined = format!("{joined}.{next}");
        }
    }
    joined.parse().ok()
}

/// Provider low-cost tier markers. "luna" is OpenAI's cost-sensitive
/// GPT-5.6 tier (https://developers.openai.com/api/docs/models, checked
/// 2026-08-29: sol flagship / terra mid / luna "optimized for
/// cost-sensitive workloads") — the owner's default doctrine resolves
/// through this marker when the rate catalog has no price for the family.
fn is_cheap_tier(id: &str) -> bool {
    ["mini", "lite", "flash", "haiku", "nano", "luna"]
        .iter()
        .any(|marker| id.contains(marker))
}

fn catalog_price(provider: &str, model: &str) -> Option<f64> {
    let catalog: serde_json::Value =
        serde_json::from_str(crate::providers::cost::RATE_CATALOG_JSON).ok()?;
    catalog["script_rates"].as_array()?.iter().find_map(|rate| {
        (rate["provider"] == provider && rate["model"] == model)
            .then(|| rate["input_usd_per_mtok"].as_f64())
            .flatten()
    })
}

fn latest_matching(models: &[String], keep: impl Fn(&str) -> bool) -> Vec<(&String, f64)> {
    let mut versioned: Vec<(&String, f64)> = models
        .iter()
        .filter(|id| keep(id))
        .filter_map(|id| version_of(id).map(|version| (id, version)))
        .collect();
    versioned.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    versioned
}

/// The owner's default policy over a fetched model list. Returns None when
/// nothing matches the tier shape — the connection's stored default then
/// stands, and manual entry always wins.
pub fn default_model_for(kind: ProviderKind, models: &[String]) -> Option<String> {
    match kind {
        ProviderKind::Gemini => {
            // Latest plain flash: not lite, not preview, not image/tts/live.
            let ranked = latest_matching(models, |id| {
                id.starts_with("gemini-")
                    && id.ends_with("-flash")
                    && !id.contains("lite")
                    && !id.contains("preview")
            });
            ranked.first().map(|(id, _)| (*id).clone())
        }
        ProviderKind::Anthropic => {
            let ranked = latest_matching(models, |id| id.starts_with("claude-sonnet"));
            ranked.first().map(|(id, _)| (*id).clone())
        }
        ProviderKind::OpenaiCompatible | ProviderKind::OpenaiTts => {
            // Latest generation; among equals prefer the cheaper one —
            // catalog price when we have it, else the provider's own
            // low-cost tier marker.
            let ranked = latest_matching(models, |_| true);
            let top = ranked.first()?.1;
            let mut latest: Vec<&String> = ranked
                .iter()
                .filter(|(_, version)| (*version - top).abs() < 1e-9)
                .map(|(id, _)| *id)
                .collect();
            latest.sort_by(|a, b| {
                let price = |id: &str| catalog_price(kind.id(), id);
                match (price(a), price(b)) {
                    (Some(pa), Some(pb)) => {
                        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => is_cheap_tier(b).cmp(&is_cheap_tier(a)),
                }
            });
            latest.first().map(|id| (*id).clone())
        }
        ProviderKind::CartesiaTts => models
            .iter()
            .find(|id| *id == "sonic-latest")
            .or_else(|| {
                latest_matching(models, |id| id.starts_with("sonic"))
                    .first()
                    .map(|(id, _)| *id)
            })
            .cloned(),
    }
}

/// The fixed Cartesia model set — this endpoint enumerates its models in
/// the docs rather than an API (https://docs.cartesia.ai/api-reference/tts/sse,
/// checked 2026-08-28), so "fetching" is the documented list.
pub fn cartesia_static_models() -> Vec<String> {
    ["sonic-3.6", "sonic-3.5", "sonic-3", "sonic-latest"]
        .iter()
        .map(|id| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn gemini_default_is_the_latest_plain_flash() {
        let models = ids(&[
            "gemini-2.5-flash",
            "gemini-3.5-flash",
            "gemini-3.7-flash",
            "gemini-3.5-flash-lite",
            "gemini-3-flash-preview",
            "gemini-3.1-flash-image",
            "gemini-2.5-pro",
        ]);
        assert_eq!(
            default_model_for(ProviderKind::Gemini, &models).as_deref(),
            Some("gemini-3.7-flash")
        );
    }

    #[test]
    fn anthropic_default_is_the_latest_sonnet() {
        let models = ids(&[
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-sonnet-5",
            "claude-opus-5",
        ]);
        assert_eq!(
            default_model_for(ProviderKind::Anthropic, &models).as_deref(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn openai_default_is_latest_then_cheapest_among_equals() {
        // Same latest generation, cheaper tier marker wins when the catalog
        // has no price for either. (Generic ids avoid the real tier
        // markers except the one under test.)
        let models = ids(&["nova-5.6", "nova-5.6-mini", "nova-5.5"]);
        assert_eq!(
            default_model_for(ProviderKind::OpenaiCompatible, &models).as_deref(),
            Some("nova-5.6-mini")
        );
        // Catalog price beats the marker heuristic when present.
        let models = ids(&["gpt-4o-mini", "gpt-4o"]);
        assert_eq!(
            default_model_for(ProviderKind::OpenaiCompatible, &models).as_deref(),
            Some("gpt-4o-mini")
        );
    }

    /// Owner UX doctrine pin (2026-08-29): over OpenAI's live listing as
    /// fetched that day (fixtures/providers/openai_models_2026-08-29.json,
    /// https://developers.openai.com/api/docs/models), the POLICY —
    /// latest generation, cheapest among equals, "luna" recognized as the
    /// low-cost tier marker — must resolve to gpt-5.6-luna. When OpenAI's
    /// lineup moves, this fixture is refreshed deliberately and the
    /// resolution re-ruled; the id is never hardcoded in the policy.
    #[test]
    fn openai_doctrine_pin_resolves_the_dated_live_list_to_luna() {
        let listing: serde_json::Value = serde_json::from_str(include_str!(
            "../../fixtures/providers/openai_models_2026-08-29.json"
        ))
        .unwrap();
        let models: Vec<String> = listing["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            default_model_for(ProviderKind::OpenaiCompatible, &models).as_deref(),
            Some("gpt-5.6-luna")
        );
    }

    #[test]
    fn no_tier_match_means_no_suggestion_and_manual_entry_stands() {
        assert_eq!(
            default_model_for(ProviderKind::Gemini, &ids(&["gemini-2.5-pro"])),
            None
        );
        assert_eq!(default_model_for(ProviderKind::Anthropic, &[]), None);
    }

    #[test]
    fn cartesia_prefers_the_documented_latest_alias() {
        assert_eq!(
            default_model_for(ProviderKind::CartesiaTts, &cartesia_static_models()).as_deref(),
            Some("sonic-latest")
        );
    }
}
