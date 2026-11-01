//! PF-PH1 — the owner-directed total pool-health composition (Hermes S-5
//! pre-brief, binding). Two deliberately different notions live here:
//! the HEALTH POPULATION (every configured non-Disabled slot — Invalid,
//! RateLimited, Unreachable, and MissingBytes stay included so a summary
//! cannot hide unavailable capacity) and DIAL ELIGIBILITY NOW (Working and
//! loadable Unverified only). The pool label and the provider-card
//! aggregate are TOTAL first-match functions over a closed enum: the
//! backend computes them and exposes typed counts; React never infers
//! health from labels.
//!
//! The atomic pool is provider custody identity + canonical base URL.

use serde::Serialize;

use crate::voice::keys::probe::KeyState;

/// The closed slot-state enum (pre-brief PF-PH1). `Invalid` is VA3's
/// dead/provider-rejected state; `Disabled` is an explicit operator choice
/// that keeps the slot configured but out of the operational population;
/// `Unverified` is absence of conclusive evidence; `Unreachable` is a
/// COMPLETED attempt that failed in typed transport — evidence, never an
/// alias for unknown, and never `Invalid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotHealth {
    Working,
    Checking,
    Unverified,
    Invalid,
    RateLimited,
    Unreachable,
    MissingBytes,
    Disabled,
}

/// The typed facts one slot projects from. VA3's four-state `KeyState`
/// stays authoritative for what it knows; the extra evidence (operator
/// disable, in-flight check, typed transport failure, missing bytes,
/// elapsed retry deadline) arrives as separate typed fields so nothing is
/// inferred from labels.
#[derive(Debug, Clone, Copy)]
pub struct SlotFacts {
    pub state: KeyState,
    pub key_present: bool,
    pub disabled: bool,
    pub checking: bool,
    /// A completed probe attempt ended in typed timeout/DNS/TLS/connect/
    /// transport failure and no conclusive result has replaced it since.
    pub transport_failed: bool,
    /// The provider-given rate-limit retry deadline has elapsed. Time
    /// passage alone never makes a slot Working — it re-enters Unverified.
    pub retry_after_elapsed: bool,
}

pub fn slot_health(facts: &SlotFacts) -> SlotHealth {
    if facts.disabled {
        return SlotHealth::Disabled;
    }
    if facts.checking {
        return SlotHealth::Checking;
    }
    if !facts.key_present {
        return SlotHealth::MissingBytes;
    }
    match facts.state {
        KeyState::Active => SlotHealth::Working,
        KeyState::Invalid => SlotHealth::Invalid,
        KeyState::OutOfQuota => {
            if facts.retry_after_elapsed {
                SlotHealth::Unverified
            } else {
                SlotHealth::RateLimited
            }
        }
        KeyState::Unchecked => {
            if facts.transport_failed {
                SlotHealth::Unreachable
            } else {
                SlotHealth::Unverified
            }
        }
    }
}

/// Dial eligibility NOW (pre-brief): Working and loadable Unverified only.
/// Checking, Invalid, RateLimited (deadline not elapsed), Unreachable,
/// MissingBytes, and Disabled are not dial-eligible. An explicit Retry may
/// start a new attempt from Unreachable; it does not relabel the slot.
pub fn dial_eligible(health: SlotHealth) -> bool {
    matches!(health, SlotHealth::Working | SlotHealth::Unverified)
}

/// Health-population membership: every configured non-Disabled slot.
pub fn in_health_population(health: SlotHealth) -> bool {
    health != SlotHealth::Disabled
}

/// The 14-rule pool label, in binding priority order. Total by
/// construction: the closed enum plus the ordered predicates leave rule 14
/// exhaustive (all-Invalid), not a vacuous fallback.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PoolLabel {
    /// 1. No enabled connection references the pool.
    ConnectionPoolDisabled,
    /// 2. Every enabled referencing connection is lawful keyless loopback.
    NoKeyRequired,
    /// 3. Vault locked / custody backend unavailable.
    PoolLocked,
    /// 4. An explicit pool test is running (completed / total).
    Checking { completed: u32, total: u32 },
    /// 5. Zero configured slots where a credential is required.
    NoKeysConfigured,
    /// 6. Slots exist but every one is operator-Disabled.
    KeysDisabled,
    /// 7. Every operational slot is Working — the only Healthy.
    Healthy,
    /// 8. At least one operational slot Working, others need attention.
    Degraded { need_attention: u32 },
    /// 9/10. No Working; Checking and/or Unverified present.
    Unverified,
    /// 11. No Working/Checking/Unverified; some slot has no bytes.
    KeysNeeded,
    /// 12. Remaining usable-someday capacity is rate limited.
    RateLimited { earliest_retry_unix: Option<i64> },
    /// 13. Typed transport failure and nothing better.
    Unreachable,
    /// 14. Every operational credential was provider-rejected/dead.
    NotWorking,
}

/// Pool-level facts the label composes over.
#[derive(Debug, Clone)]
pub struct PoolFacts {
    /// Count of ENABLED connections referencing this pool.
    pub enabled_connections: u32,
    /// Every enabled referencing connection is lawful credential-free
    /// loopback. A keyed alias makes this false.
    pub all_enabled_loopback_keyless: bool,
    pub vault_locked: bool,
    /// `Some((completed, total))` while an explicit pool test runs.
    pub explicit_test: Option<(u32, u32)>,
    pub slots: Vec<SlotHealth>,
    /// Earliest trustworthy retry deadline among rate-limited slots.
    pub earliest_retry_unix: Option<i64>,
}

pub fn pool_label(facts: &PoolFacts) -> PoolLabel {
    // 1
    if facts.enabled_connections == 0 {
        return PoolLabel::ConnectionPoolDisabled;
    }
    // 2
    if facts.all_enabled_loopback_keyless {
        return PoolLabel::NoKeyRequired;
    }
    // 3
    if facts.vault_locked {
        return PoolLabel::PoolLocked;
    }
    // 4
    if let Some((completed, total)) = facts.explicit_test {
        return PoolLabel::Checking { completed, total };
    }
    // 5
    if facts.slots.is_empty() {
        return PoolLabel::NoKeysConfigured;
    }
    let operational: Vec<SlotHealth> = facts
        .slots
        .iter()
        .copied()
        .filter(|health| in_health_population(*health))
        .collect();
    // 6
    if operational.is_empty() {
        return PoolLabel::KeysDisabled;
    }
    let count = |target: SlotHealth| operational.iter().filter(|h| **h == target).count() as u32;
    let working = count(SlotHealth::Working);
    let checking = count(SlotHealth::Checking);
    let unverified = count(SlotHealth::Unverified);
    let missing = count(SlotHealth::MissingBytes);
    let rate_limited = count(SlotHealth::RateLimited);
    let unreachable = count(SlotHealth::Unreachable);
    let total = operational.len() as u32;
    // 7
    if working == total {
        return PoolLabel::Healthy;
    }
    // 8
    if working > 0 {
        return PoolLabel::Degraded {
            need_attention: total - working,
        };
    }
    // 9 — all Checking (rule 9's own text; rendered like rule 4 upstream).
    if checking == total {
        return PoolLabel::Checking {
            completed: 0,
            total,
        };
    }
    // 10 — success unproven: Checking and/or never-probed Unverified.
    if checking + unverified > 0 {
        return PoolLabel::Unverified;
    }
    // 11
    if missing > 0 {
        return PoolLabel::KeysNeeded;
    }
    // 12
    if rate_limited > 0 {
        return PoolLabel::RateLimited {
            earliest_retry_unix: facts.earliest_retry_unix,
        };
    }
    // 13
    if unreachable > 0 {
        return PoolLabel::Unreachable;
    }
    // 14 — the closed enum leaves only Invalid.
    PoolLabel::NotWorking
}

/// Whether a pool label counts as "currently usable" for the provider-card
/// aggregation (Healthy, No key required, or Degraded).
fn pool_usable(label: &PoolLabel) -> bool {
    matches!(
        label,
        PoolLabel::Healthy | PoolLabel::NoKeyRequired | PoolLabel::Degraded { .. }
    )
}

fn pool_checking(label: &PoolLabel) -> bool {
    matches!(label, PoolLabel::Checking { .. })
}

/// The provider-card aggregate, first-match and total. Disabled
/// connections, `ConnectionPoolDisabled`, and `KeysDisabled` pools are
/// excluded from operational membership BEFORE composition; their counts
/// surface separately on the card.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderCardLabel {
    NotConfigured,
    Disabled,
    NeedsUnlock,
    CheckingAll { completed: u32, total: u32 },
    DegradedChecking { checking_pools: u32 },
    CheckingSome { checking: u32, total: u32 },
    Healthy,
    Degraded,
    Uniform { label: PoolLabel },
    Unavailable { pools_need_attention: u32 },
}

pub fn provider_card_label(configured: bool, pool_labels: &[PoolLabel]) -> ProviderCardLabel {
    if !configured {
        return ProviderCardLabel::NotConfigured;
    }
    let operational: Vec<&PoolLabel> = pool_labels
        .iter()
        .filter(|label| {
            !matches!(
                label,
                PoolLabel::ConnectionPoolDisabled | PoolLabel::KeysDisabled
            )
        })
        .collect();
    if operational.is_empty() {
        return ProviderCardLabel::Disabled;
    }
    if operational
        .iter()
        .any(|label| matches!(label, PoolLabel::PoolLocked))
    {
        return ProviderCardLabel::NeedsUnlock;
    }
    let total = operational.len() as u32;
    let checking = operational
        .iter()
        .filter(|label| pool_checking(label))
        .count() as u32;
    if checking == total {
        return ProviderCardLabel::CheckingAll {
            completed: 0,
            total,
        };
    }
    if checking > 0 {
        if operational
            .iter()
            .any(|label| !pool_checking(label) && pool_usable(label))
        {
            return ProviderCardLabel::DegradedChecking {
                checking_pools: checking,
            };
        }
        return ProviderCardLabel::CheckingSome { checking, total };
    }
    if operational
        .iter()
        .all(|label| matches!(label, PoolLabel::Healthy | PoolLabel::NoKeyRequired))
    {
        return ProviderCardLabel::Healthy;
    }
    if operational.iter().any(|label| pool_usable(label)) {
        return ProviderCardLabel::Degraded;
    }
    if operational
        .windows(2)
        .all(|pair| std::mem::discriminant(pair[0]) == std::mem::discriminant(pair[1]))
    {
        return ProviderCardLabel::Uniform {
            label: (*operational[0]).clone(),
        };
    }
    ProviderCardLabel::Unavailable {
        pools_need_attention: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(slots: Vec<SlotHealth>) -> PoolFacts {
        PoolFacts {
            enabled_connections: 1,
            all_enabled_loopback_keyless: false,
            vault_locked: false,
            explicit_test: None,
            slots,
            earliest_retry_unix: None,
        }
    }

    fn slot(state: KeyState) -> SlotFacts {
        SlotFacts {
            state,
            key_present: true,
            disabled: false,
            checking: false,
            transport_failed: false,
            retry_after_elapsed: false,
        }
    }

    // ------ slot projection ------

    #[test]
    fn slot_projection_is_total_and_typed() {
        use SlotHealth::*;
        assert_eq!(slot_health(&slot(KeyState::Active)), Working);
        assert_eq!(slot_health(&slot(KeyState::Invalid)), Invalid);
        assert_eq!(slot_health(&slot(KeyState::OutOfQuota)), RateLimited);
        assert_eq!(slot_health(&slot(KeyState::Unchecked)), Unverified);
        // Typed transport failure is EVIDENCE (Unreachable), never Invalid
        // and never the absence-of-evidence Unverified.
        let unreachable = SlotFacts {
            transport_failed: true,
            ..slot(KeyState::Unchecked)
        };
        assert_eq!(slot_health(&unreachable), Unreachable);
        // Elapsed retry-after re-enters Unverified — never Working.
        let elapsed = SlotFacts {
            retry_after_elapsed: true,
            ..slot(KeyState::OutOfQuota)
        };
        assert_eq!(slot_health(&elapsed), Unverified);
        // Missing bytes is its own state, not provider-invalid.
        let missing = SlotFacts {
            key_present: false,
            ..slot(KeyState::Active)
        };
        assert_eq!(slot_health(&missing), MissingBytes);
        // Operator disable wins over everything.
        let disabled = SlotFacts {
            disabled: true,
            checking: true,
            ..slot(KeyState::Invalid)
        };
        assert_eq!(slot_health(&disabled), Disabled);
        // In-flight check wins over stored state (but not over disable).
        let checking = SlotFacts {
            checking: true,
            ..slot(KeyState::Invalid)
        };
        assert_eq!(slot_health(&checking), Checking);
    }

    #[test]
    fn dial_eligibility_and_health_population_are_separate_pins() {
        use SlotHealth::*;
        for health in [Working, Unverified] {
            assert!(dial_eligible(health), "{health:?} dials");
        }
        for health in [
            Checking,
            Invalid,
            RateLimited,
            Unreachable,
            MissingBytes,
            Disabled,
        ] {
            assert!(!dial_eligible(health), "{health:?} must not dial");
        }
        for health in [
            Working,
            Checking,
            Unverified,
            Invalid,
            RateLimited,
            Unreachable,
            MissingBytes,
        ] {
            assert!(in_health_population(health), "{health:?} counts");
        }
        assert!(!in_health_population(Disabled));
    }

    // ------ the 14-rule truth table, in priority order ------

    #[test]
    fn rule_01_no_enabled_connection_wins_over_everything() {
        let mut f = facts(vec![SlotHealth::Working]);
        f.enabled_connections = 0;
        f.vault_locked = true;
        f.explicit_test = Some((1, 2));
        assert_eq!(pool_label(&f), PoolLabel::ConnectionPoolDisabled);
    }

    #[test]
    fn rule_02_lawful_loopback_needs_no_key_even_with_zero_slots() {
        let mut f = facts(Vec::new());
        f.all_enabled_loopback_keyless = true;
        assert_eq!(pool_label(&f), PoolLabel::NoKeyRequired);
    }

    #[test]
    fn rule_03_locked_vault_beats_slot_composition() {
        let mut f = facts(vec![SlotHealth::Working, SlotHealth::Invalid]);
        f.vault_locked = true;
        assert_eq!(pool_label(&f), PoolLabel::PoolLocked);
    }

    #[test]
    fn rule_04_explicit_test_shows_progress() {
        let mut f = facts(vec![SlotHealth::Working]);
        f.explicit_test = Some((3, 7));
        assert_eq!(
            pool_label(&f),
            PoolLabel::Checking {
                completed: 3,
                total: 7
            }
        );
    }

    #[test]
    fn rule_05_zero_configured_slots() {
        assert_eq!(pool_label(&facts(Vec::new())), PoolLabel::NoKeysConfigured);
    }

    #[test]
    fn rule_06_all_disabled_is_keys_disabled_not_missing() {
        let f = facts(vec![SlotHealth::Disabled, SlotHealth::Disabled]);
        assert_eq!(pool_label(&f), PoolLabel::KeysDisabled);
    }

    #[test]
    fn rule_07_healthy_requires_every_operational_slot_working() {
        use SlotHealth::*;
        assert_eq!(
            pool_label(&facts(vec![Working, Working])),
            PoolLabel::Healthy
        );
        // Disabled slots are excluded from the operational population, so
        // Working + Disabled is still Healthy.
        assert_eq!(
            pool_label(&facts(vec![Working, Disabled])),
            PoolLabel::Healthy
        );
        // Any other operational companion denies Healthy.
        for other in [
            Checking,
            Unverified,
            Invalid,
            RateLimited,
            Unreachable,
            MissingBytes,
        ] {
            assert_ne!(
                pool_label(&facts(vec![Working, other])),
                PoolLabel::Healthy,
                "{other:?} must deny Healthy"
            );
        }
    }

    #[test]
    fn rule_08_one_working_is_degraded_and_counts_every_companion() {
        use SlotHealth::*;
        let f = facts(vec![
            Working,
            Checking,
            Unverified,
            Invalid,
            RateLimited,
            Unreachable,
            MissingBytes,
        ]);
        assert_eq!(pool_label(&f), PoolLabel::Degraded { need_attention: 6 });
        // Disabled is reported separately, never as unhealthy capacity.
        let f = facts(vec![Working, Invalid, Disabled]);
        assert_eq!(pool_label(&f), PoolLabel::Degraded { need_attention: 1 });
    }

    #[test]
    fn rule_09_all_operational_checking() {
        use SlotHealth::*;
        assert_eq!(
            pool_label(&facts(vec![Checking, Checking])),
            PoolLabel::Checking {
                completed: 0,
                total: 2
            }
        );
        // One Checking plus Working is Degraded (clarification 4).
        assert_eq!(
            pool_label(&facts(vec![Checking, Working])),
            PoolLabel::Degraded { need_attention: 1 }
        );
    }

    #[test]
    fn rule_10_unverified_absorbs_checking_mixes_without_working() {
        use SlotHealth::*;
        assert_eq!(
            pool_label(&facts(vec![Checking, Unverified])),
            PoolLabel::Unverified
        );
        assert_eq!(
            pool_label(&facts(vec![Unverified, Invalid, Unreachable])),
            PoolLabel::Unverified
        );
    }

    #[test]
    fn rule_11_missing_bytes_says_keys_needed_not_invalid() {
        use SlotHealth::*;
        assert_eq!(
            pool_label(&facts(vec![MissingBytes, Invalid])),
            PoolLabel::KeysNeeded
        );
    }

    #[test]
    fn rule_12_rate_limited_with_retry_time() {
        use SlotHealth::*;
        let mut f = facts(vec![RateLimited, Invalid, Unreachable]);
        f.earliest_retry_unix = Some(1_756_000_000);
        assert_eq!(
            pool_label(&f),
            PoolLabel::RateLimited {
                earliest_retry_unix: Some(1_756_000_000)
            }
        );
    }

    #[test]
    fn rule_13_unreachable_proves_nothing_about_key_validity() {
        use SlotHealth::*;
        assert_eq!(
            pool_label(&facts(vec![Unreachable, Invalid])),
            PoolLabel::Unreachable
        );
    }

    #[test]
    fn rule_14_all_invalid_is_not_working_and_the_table_is_total() {
        use SlotHealth::*;
        assert_eq!(
            pool_label(&facts(vec![Invalid, Invalid])),
            PoolLabel::NotWorking
        );
        // Totality sweep: every single-slot pool composes to SOME label
        // without panicking, and Healthy only for Working.
        for health in [
            Working,
            Checking,
            Unverified,
            Invalid,
            RateLimited,
            Unreachable,
            MissingBytes,
            Disabled,
        ] {
            let label = pool_label(&facts(vec![health]));
            assert_eq!(
                label == PoolLabel::Healthy,
                health == Working,
                "{health:?} -> {label:?}"
            );
        }
    }

    #[test]
    fn retry_after_expiry_reenters_unverified_composition() {
        // Clarification 3: an elapsed retry-after slot projects Unverified,
        // so a pool of them is Pool Unverified — not Rate limited, and
        // never silently Working.
        let elapsed = SlotFacts {
            retry_after_elapsed: true,
            ..SlotFacts {
                state: KeyState::OutOfQuota,
                key_present: true,
                disabled: false,
                checking: false,
                transport_failed: false,
                retry_after_elapsed: true,
            }
        };
        let f = facts(vec![slot_health(&elapsed)]);
        assert_eq!(pool_label(&f), PoolLabel::Unverified);
    }

    // ------ provider-card aggregation ------

    #[test]
    fn card_not_configured_then_disabled_then_unlock() {
        assert_eq!(
            provider_card_label(false, &[]),
            ProviderCardLabel::NotConfigured
        );
        assert_eq!(
            provider_card_label(true, &[PoolLabel::ConnectionPoolDisabled]),
            ProviderCardLabel::Disabled
        );
        assert_eq!(
            provider_card_label(true, &[PoolLabel::KeysDisabled]),
            ProviderCardLabel::Disabled
        );
        assert_eq!(
            provider_card_label(true, &[PoolLabel::PoolLocked, PoolLabel::Healthy]),
            ProviderCardLabel::NeedsUnlock
        );
    }

    #[test]
    fn card_checking_tiers() {
        let checking = PoolLabel::Checking {
            completed: 0,
            total: 3,
        };
        assert_eq!(
            provider_card_label(true, &[checking.clone(), checking.clone()]),
            ProviderCardLabel::CheckingAll {
                completed: 0,
                total: 2
            }
        );
        assert_eq!(
            provider_card_label(true, &[checking.clone(), PoolLabel::Healthy]),
            ProviderCardLabel::DegradedChecking { checking_pools: 1 }
        );
        assert_eq!(
            provider_card_label(true, &[checking, PoolLabel::NotWorking]),
            ProviderCardLabel::CheckingSome {
                checking: 1,
                total: 2
            }
        );
    }

    #[test]
    fn card_healthy_degraded_uniform_unavailable() {
        assert_eq!(
            provider_card_label(true, &[PoolLabel::Healthy, PoolLabel::NoKeyRequired]),
            ProviderCardLabel::Healthy
        );
        assert_eq!(
            provider_card_label(
                true,
                &[
                    PoolLabel::Degraded { need_attention: 2 },
                    PoolLabel::NotWorking
                ]
            ),
            ProviderCardLabel::Degraded
        );
        assert_eq!(
            provider_card_label(true, &[PoolLabel::NotWorking, PoolLabel::NotWorking]),
            ProviderCardLabel::Uniform {
                label: PoolLabel::NotWorking
            }
        );
        assert_eq!(
            provider_card_label(true, &[PoolLabel::NotWorking, PoolLabel::Unreachable]),
            ProviderCardLabel::Unavailable {
                pools_need_attention: 2
            }
        );
        // A KeysDisabled pool is excluded BEFORE composition: it neither
        // degrades nor hides the healthy pool.
        assert_eq!(
            provider_card_label(true, &[PoolLabel::Healthy, PoolLabel::KeysDisabled]),
            ProviderCardLabel::Healthy
        );
    }
}
