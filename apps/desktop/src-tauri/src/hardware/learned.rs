use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MAX_SAMPLES: u32 = 10;

pub const KEY_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub value: f64,
    pub samples: u32,
}

impl Sample {
    pub fn new(value: f64) -> Self {
        Sample { value, samples: 1 }
    }

    pub fn merge(self, observation: f64) -> Self {
        if !observation.is_finite() || observation <= 0.0 {
            return self;
        }
        let n = self.samples.min(MAX_SAMPLES);
        let weight = f64::from(n);
        let value = (self.value * weight + observation) / (weight + 1.0);
        Sample {
            value,
            samples: (self.samples + 1).min(MAX_SAMPLES),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LearnedCoefficients {
    #[serde(default)]
    pub tts: BTreeMap<String, Sample>,
    #[serde(default)]
    pub llm: BTreeMap<String, Sample>,
    #[serde(default)]
    pub key_revision: u32,
}

pub fn tts_key(model_id: &str, device: &str) -> String {
    format!("{model_id}:{device}")
}

pub fn llm_key(model_id: &str, device: &str) -> String {
    format!("{model_id}:{device}")
}

impl LearnedCoefficients {
    pub fn from_json(raw: Option<&str>) -> Self {
        match raw.and_then(|s| serde_json::from_str::<Self>(s).ok()) {
            Some(stored) => stored,
            None => LearnedCoefficients {
                key_revision: KEY_REVISION,
                ..Default::default()
            },
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn record_tts_rtf(&mut self, model_id: &str, device: &str, observed: f64) {
        record(&mut self.tts, &tts_key(model_id, device), observed);
    }

    pub fn record_llm_tok_s(&mut self, model_id: &str, device: &str, observed: f64) {
        record(&mut self.llm, &llm_key(model_id, device), observed);
    }

    pub fn tts_rtf(&self, model_id: &str, device: &str) -> Option<f64> {
        self.tts.get(&tts_key(model_id, device)).map(|s| s.value)
    }

    pub fn llm_tok_s(&self, model_id: &str, device: &str) -> Option<f64> {
        self.llm.get(&llm_key(model_id, device)).map(|s| s.value)
    }
}

fn is_legacy_gpu_label(device: &str) -> bool {
    let head = device.split(':').next().unwrap_or(device);
    matches!(head, "cuda" | "rocm" | "vulkan" | "metal")
}

fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(':')
}

impl LearnedCoefficients {
    pub fn migrate_legacy_gpu_keys(&mut self, cuda_identity: Option<&str>) -> bool {
        if self.key_revision >= KEY_REVISION {
            return false;
        }
        self.key_revision = KEY_REVISION;
        self.llm.retain(|key, _| {
            !split_key(key).is_some_and(|(_, device)| is_legacy_gpu_label(device))
        });

        let legacy: Vec<String> = self
            .tts
            .keys()
            .filter(|key| split_key(key).is_some_and(|(_, device)| is_legacy_gpu_label(device)))
            .cloned()
            .collect();
        for key in legacy {
            let Some(sample) = self.tts.remove(&key) else {
                continue;
            };
            let (Some(identity), Some((model_id, _))) = (cuda_identity, split_key(&key)) else {
                continue;
            };
            self.tts
                .entry(tts_key(model_id, identity))
                .or_insert(sample);
        }
        true
    }
}

fn record(map: &mut BTreeMap<String, Sample>, key: &str, observed: f64) {
    if !observed.is_finite() || observed <= 0.0 {
        return;
    }
    match map.get(key).copied() {
        Some(existing) => {
            map.insert(key.to_string(), existing.merge(observed));
        }
        None => {
            map.insert(key.to_string(), Sample::new(observed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_model_ids_carry_no_colon() {
        for model in &crate::catalog::bundled_catalog().models {
            assert!(
                !model.id.contains(':'),
                "'{}' would make its learned key ambiguous against the device half",
                model.id
            );
        }
    }

    #[test]
    fn a_bus_id_key_survives_being_split() {
        let key = tts_key("kokoro-82m", "pci:0000:01:00.0");
        assert_eq!(
            split_key(&key),
            Some(("kokoro-82m", "pci:0000:01:00.0")),
            "splitting from the right would cut the bus ID and mistake the remainder for a \
             device label"
        );
    }

    #[test]
    fn a_table_that_did_not_exist_is_born_migrated() {
        let fresh = LearnedCoefficients::from_json(None);
        assert_eq!(fresh.key_revision, KEY_REVISION);
        assert!(
            !fresh.clone().migrate_legacy_gpu_keys(None),
            "on a first launch the migration task and a resumed job race; a table with no \
             history has nothing to migrate and must not eat what the job just recorded"
        );
    }

    #[test]
    fn an_existing_pre_revision_table_still_migrates() {
        let legacy = r#"{"tts":{"kokoro-82m:cuda":{"value":0.05,"samples":1}},"llm":{}}"#;
        let mut stored = LearnedCoefficients::from_json(Some(legacy));
        assert_eq!(stored.key_revision, 0);
        assert!(stored.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0")));
        assert_eq!(stored.tts_rtf("kokoro-82m", "pci:0000:01:00.0"), Some(0.05));
    }

    #[test]
    fn a_table_already_at_the_current_revision_is_left_alone() {
        let mut c = LearnedCoefficients {
            key_revision: KEY_REVISION,
            ..Default::default()
        };
        c.record_tts_rtf("kokoro-82m", "cuda", 0.05);
        assert!(!c.migrate_legacy_gpu_keys(None));
        assert_eq!(c.tts_rtf("kokoro-82m", "cuda"), Some(0.05));
    }

    #[test]
    fn the_revision_survives_a_json_round_trip() {
        let mut c = LearnedCoefficients::default();
        c.migrate_legacy_gpu_keys(None);
        let reloaded = LearnedCoefficients::from_json(Some(&c.to_json().expect("json")));
        assert_eq!(reloaded.key_revision, KEY_REVISION);
        assert!(
            !reloaded.clone().migrate_legacy_gpu_keys(None),
            "a stamp that does not survive the round trip is not a stamp"
        );
    }

    #[test]
    fn a_sample_recorded_today_is_not_eaten_by_the_legacy_migration() {
        let plan = crate::hardware::device::DevicePlan::gpu(
            crate::hardware::device::DeviceBackend::Vulkan,
            0,
        );
        let key = plan.learned_key();
        assert_eq!(
            key, "vulkan:0",
            "a plan whose bus id was unknown still records under a backend-shaped key"
        );

        let mut c = LearnedCoefficients {
            key_revision: KEY_REVISION,
            ..Default::default()
        };
        c.record_tts_rtf("kokoro-82m", &key, 0.05);
        c.migrate_legacy_gpu_keys(None);
        assert_eq!(
            c.tts_rtf("kokoro-82m", &key),
            Some(0.05),
            "the migration classifies by key shape, so it cannot tell a pre-identity sample \
             from one written this morning, and eats the new one on every launch forever"
        );
    }

    #[test]
    fn a_legacy_tts_sample_moves_onto_a_cuda_card() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro-82m", "cuda", 0.05);
        assert!(c.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0")));
        assert_eq!(c.tts_rtf("kokoro-82m", "pci:0000:01:00.0"), Some(0.05));
        assert_eq!(c.tts_rtf("kokoro-82m", "cuda"), None);
    }

    #[test]
    fn a_legacy_tts_sample_is_dropped_when_the_device_is_not_cuda() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro-82m", "cuda", 0.05);
        assert!(c.migrate_legacy_gpu_keys(None));
        assert!(
            c.tts.is_empty(),
            "kokoro is torch-CUDA-only, so on a Vulkan machine that sample was measured on \
             the CPU and must not be filed against a GPU identity"
        );
    }

    #[test]
    fn legacy_llm_samples_are_always_dropped() {
        let mut c = LearnedCoefficients::default();
        c.record_llm_tok_s("phi", "cuda", 40.0);
        c.record_llm_tok_s("phi", "vulkan:1", 12.0);
        assert!(c.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0")));
        assert!(
            c.llm.is_empty(),
            "the old label was emitted for any GPU fit, so these were measured on whatever \
             backend llama.cpp was built with and cannot be attributed to a card"
        );
    }

    #[test]
    fn cpu_and_identity_keyed_samples_survive_the_migration() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro-82m", "cpu", 0.9);
        c.record_tts_rtf("kokoro-82m", "pci:0000:01:00.0", 0.02);
        c.record_llm_tok_s("phi", "cpu", 12.0);
        assert!(c.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0")));
        assert_eq!(c.tts_rtf("kokoro-82m", "cpu"), Some(0.9));
        assert_eq!(c.tts_rtf("kokoro-82m", "pci:0000:01:00.0"), Some(0.02));
        assert_eq!(c.llm_tok_s("phi", "cpu"), Some(12.0));
    }

    #[test]
    fn a_remap_never_overwrites_a_sample_measured_under_the_identity() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro-82m", "pci:0000:01:00.0", 0.02);
        c.record_tts_rtf("kokoro-82m", "cuda", 0.50);
        c.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0"));
        assert_eq!(
            c.tts_rtf("kokoro-82m", "pci:0000:01:00.0"),
            Some(0.02),
            "a measured-under-identity sample outranks a relabelled legacy one"
        );
    }

    #[test]
    fn migrating_twice_changes_nothing_the_second_time() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro-82m", "cuda", 0.05);
        c.record_llm_tok_s("phi", "cuda", 40.0);
        assert!(c.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0")));
        let after_first = c.clone();
        assert!(!c.migrate_legacy_gpu_keys(Some("pci:0000:01:00.0")));
        assert_eq!(c, after_first);
    }

    #[test]
    fn first_observation_seeds_value() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro-82m", "cuda", 0.05);
        assert_eq!(c.tts_rtf("kokoro-82m", "cuda"), Some(0.05));
        assert_eq!(c.tts.get("kokoro-82m:cuda").unwrap().samples, 1);
    }

    #[test]
    fn rolling_average_converges_toward_observations() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("m", "cpu", 1.0);
        c.record_tts_rtf("m", "cpu", 0.5);
        let after_two = c.tts_rtf("m", "cpu").unwrap();
        assert!((after_two - 0.75).abs() < 1e-9, "got {after_two}");
        c.record_tts_rtf("m", "cpu", 0.5);
        let after_three = c.tts_rtf("m", "cpu").unwrap();
        assert!(after_three < after_two && after_three > 0.5);
    }

    #[test]
    fn samples_saturate_at_max() {
        let mut c = LearnedCoefficients::default();
        for _ in 0..50 {
            c.record_llm_tok_s("phi", "cuda", 40.0);
        }
        assert_eq!(c.llm.get("phi:cuda").unwrap().samples, MAX_SAMPLES);
        assert!((c.llm_tok_s("phi", "cuda").unwrap() - 40.0).abs() < 1e-6);
    }

    #[test]
    fn non_positive_or_nan_observations_ignored() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("m", "cpu", 0.0);
        c.record_tts_rtf("m", "cpu", -1.0);
        c.record_tts_rtf("m", "cpu", f64::NAN);
        assert_eq!(c.tts_rtf("m", "cpu"), None);
        c.record_tts_rtf("m", "cpu", 0.4);
        c.record_tts_rtf("m", "cpu", f64::INFINITY);
        assert_eq!(c.tts_rtf("m", "cpu"), Some(0.4));
    }

    #[test]
    fn json_roundtrip_preserves_samples() {
        let mut c = LearnedCoefficients::default();
        c.record_tts_rtf("kokoro", "cuda", 0.03);
        c.record_llm_tok_s("phi", "cpu", 12.0);
        let json = c.to_json().unwrap();
        let back = LearnedCoefficients::from_json(Some(&json));
        assert_eq!(c, back);
    }

    #[test]
    fn missing_keys_return_none() {
        let c = LearnedCoefficients::from_json(None);
        assert_eq!(c.tts_rtf("x", "cpu"), None);
        assert_eq!(c.llm_tok_s("x", "cuda"), None);
    }

    #[test]
    fn malformed_json_falls_back_to_an_empty_table_born_migrated() {
        let c = LearnedCoefficients::from_json(Some("not json"));
        assert!(c.tts.is_empty() && c.llm.is_empty());
        assert_eq!(
            c.key_revision, KEY_REVISION,
            "the samples are already lost to corruption; a table with no history has \
             nothing to migrate and must not enter the legacy path on the next launch"
        );
    }

    #[test]
    fn a_stored_table_without_a_revision_field_is_pre_revision() {
        let c = LearnedCoefficients::from_json(Some(
            r#"{"tts":{"kokoro-82m:cpu":{"value":0.9,"samples":1}},"llm":{}}"#,
        ));
        assert_eq!(
            c.key_revision, 0,
            "a real row written before the stamp existed must still migrate exactly once"
        );
    }
}
