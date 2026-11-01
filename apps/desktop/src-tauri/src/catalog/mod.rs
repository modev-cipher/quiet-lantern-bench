pub mod gguf;

use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

pub const BUNDLED_CATALOG_JSON: &str = include_str!("../../../../../catalog/catalog.json");

static BUNDLED_CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    load_catalog(BUNDLED_CATALOG_JSON).expect("bundled catalog.json must be valid")
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub catalog_version: String,
    pub models: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub task: ModelTask,
    pub display_name: String,
    pub family: String,
    #[serde(default)]
    pub quant: Option<String>,
    pub format: ModelFormat,
    pub disk_bytes: u64,
    #[serde(default)]
    pub full_gpu_vram_mb: Option<u64>,
    pub min_free_ram_mb: u64,
    pub hybrid_capable: bool,
    #[serde(default)]
    pub ctx_max: Option<u64>,
    pub quality_score: u8,
    pub speed_score: u8,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    pub license: License,
    pub download: Download,
    pub source: ModelSource,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub perf: Option<Perf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Perf {
    #[serde(default)]
    pub tok_s_gpu: Option<f64>,
    #[serde(default)]
    pub tok_s_cpu: Option<f64>,
    #[serde(default)]
    pub rtf_gpu: Option<f64>,
    #[serde(default)]
    pub rtf_cpu: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTask {
    Llm,
    Tts,
    Align,
    Layout,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFormat {
    Gguf,
    Pytorch,
    Onnx,
    Ct2,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    Curated,
    Custom,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    pub spdx: String,
    pub commercial_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub files: Vec<DownloadFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFile {
    pub url: String,
    pub sha256: String,
    pub dest_path: String,
    pub size_bytes: u64,
}

#[derive(Debug)]
pub enum CatalogError {
    Parse(serde_json::Error),
    Invalid(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Parse(e) => write!(f, "catalog JSON is malformed: {e}"),
            CatalogError::Invalid(msg) => write!(f, "catalog is invalid: {msg}"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<serde_json::Error> for CatalogError {
    fn from(e: serde_json::Error) -> Self {
        CatalogError::Parse(e)
    }
}

pub fn load_catalog(json: &str) -> Result<Catalog, CatalogError> {
    let catalog: Catalog = serde_json::from_str(json)?;
    validate(&catalog)?;
    Ok(catalog)
}

pub fn bundled_catalog() -> &'static Catalog {
    &BUNDLED_CATALOG
}

fn validate(catalog: &Catalog) -> Result<(), CatalogError> {
    if catalog.catalog_version.trim().is_empty() {
        return Err(CatalogError::Invalid(
            "catalog_version must not be empty".into(),
        ));
    }
    if catalog.models.is_empty() {
        return Err(CatalogError::Invalid(
            "catalog must contain at least one model".into(),
        ));
    }
    let mut seen = HashSet::new();
    for entry in &catalog.models {
        if entry.id.trim().is_empty() {
            return Err(CatalogError::Invalid("model id must not be empty".into()));
        }
        if !is_safe_path_segment(&entry.id) {
            return Err(CatalogError::Invalid(format!(
                "model id '{}' must be a safe path segment (alphanumeric, '-', '_', '.')",
                entry.id
            )));
        }
        if !seen.insert(entry.id.as_str()) {
            return Err(CatalogError::Invalid(format!(
                "duplicate model id '{}'",
                entry.id
            )));
        }
        if entry.display_name.trim().is_empty() {
            return Err(CatalogError::Invalid(format!(
                "model '{}': display_name must not be empty",
                entry.id
            )));
        }
        if entry.quality_score > 100 {
            return Err(CatalogError::Invalid(format!(
                "model '{}': quality_score must be 0-100, got {}",
                entry.id, entry.quality_score
            )));
        }
        if entry.speed_score > 100 {
            return Err(CatalogError::Invalid(format!(
                "model '{}': speed_score must be 0-100, got {}",
                entry.id, entry.speed_score
            )));
        }
        if entry.license.spdx.trim().is_empty() {
            return Err(CatalogError::Invalid(format!(
                "model '{}': license.spdx must not be empty",
                entry.id
            )));
        }
        if let Some(ctx_max) = entry.ctx_max {
            if ctx_max == 0 {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': ctx_max must be greater than 0",
                    entry.id
                )));
            }
        }
        if entry.download.files.is_empty() {
            return Err(CatalogError::Invalid(format!(
                "model '{}': download.files must not be empty",
                entry.id
            )));
        }
        let mut dest_paths = HashSet::new();
        for file in &entry.download.files {
            if !file.url.starts_with("https://") {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': download url '{}' must start with https://",
                    entry.id, file.url
                )));
            }
            if !is_sha256_hex(&file.sha256) {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': sha256 for '{}' must be 64 lowercase hex chars",
                    entry.id, file.dest_path
                )));
            }
            if file.dest_path.trim().is_empty() {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': dest_path must not be empty",
                    entry.id
                )));
            }
            let path = std::path::Path::new(&file.dest_path);
            let all_normal = path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
            if !all_normal || file.dest_path.contains('\\') || file.dest_path.contains(':') {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': dest_path '{}' must be a relative path with only normal components",
                    entry.id, file.dest_path
                )));
            }
            if !dest_paths.insert(file.dest_path.as_str()) {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': duplicate dest_path '{}'",
                    entry.id, file.dest_path
                )));
            }
            if file.size_bytes == 0 {
                return Err(CatalogError::Invalid(format!(
                    "model '{}': size_bytes for '{}' must be > 0",
                    entry.id, file.dest_path
                )));
            }
        }
        if let Some(perf) = &entry.perf {
            for (field, value) in [
                ("tok_s_gpu", perf.tok_s_gpu),
                ("tok_s_cpu", perf.tok_s_cpu),
                ("rtf_gpu", perf.rtf_gpu),
                ("rtf_cpu", perf.rtf_cpu),
            ] {
                if let Some(v) = value {
                    if !v.is_finite() || v <= 0.0 {
                        return Err(CatalogError::Invalid(format!(
                            "model '{}': perf.{} must be a finite positive number, got {}",
                            entry.id, field, v
                        )));
                    }
                }
            }
        }
        let total: u64 = entry.download.files.iter().map(|f| f.size_bytes).sum();
        if entry.disk_bytes != total {
            return Err(CatalogError::Invalid(format!(
                "model '{}': disk_bytes {} must equal sum of file sizes {}",
                entry.id, entry.disk_bytes, total
            )));
        }
    }
    Ok(())
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_safe_path_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SHA: &str = "a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3";

    fn entry_json(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "task": "llm",
                "display_name": "Test Model",
                "family": "test",
                "quant": "Q4_K_M",
                "format": "gguf",
                "disk_bytes": 1000,
                "full_gpu_vram_mb": 2000,
                "min_free_ram_mb": 4096,
                "hybrid_capable": true,
                "ctx_max": 8192,
                "quality_score": 80,
                "speed_score": 70,
                "capabilities": ["teacher_mode"],
                "languages": ["en"],
                "license": {{ "spdx": "Apache-2.0", "commercial_ok": true }},
                "download": {{ "files": [{{ "url": "https://example.com/m.gguf", "sha256": "{TEST_SHA}", "dest_path": "m.gguf", "size_bytes": 1000 }}] }},
                "source": "curated",
                "evidence": ["https://example.com"]
            }}"#
        )
    }

    fn catalog_json(entries: &[String]) -> String {
        format!(
            r#"{{ "catalog_version": "2026.07.0", "models": [{}] }}"#,
            entries.join(",")
        )
    }

    #[test]
    fn valid_catalog_parses() {
        let catalog = load_catalog(&catalog_json(&[entry_json("m1")])).unwrap();
        assert_eq!(catalog.catalog_version, "2026.07.0");
        assert_eq!(catalog.models.len(), 1);
        let m = &catalog.models[0];
        assert_eq!(m.id, "m1");
        assert_eq!(m.task, ModelTask::Llm);
        assert_eq!(m.format, ModelFormat::Gguf);
        assert_eq!(m.source, ModelSource::Curated);
        assert!(!m.deprecated);
        assert!(m.replacement.is_none());
    }

    #[test]
    fn malformed_json_gives_clear_error() {
        let err = load_catalog("{ not json").unwrap_err();
        assert!(matches!(err, CatalogError::Parse(_)));
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let entry = entry_json("m1").replacen('{', r#"{ "future_field": 42,"#, 1);
        let json = catalog_json(&[entry]).replacen('{', r#"{ "top_level_future": true,"#, 1);
        let catalog = load_catalog(&json).unwrap();
        assert_eq!(catalog.models[0].id, "m1");
    }

    #[test]
    fn unknown_enum_values_tolerated() {
        let entry = entry_json("m1")
            .replace(r#""task": "llm""#, r#""task": "audio_super_res""#)
            .replace(r#""format": "gguf""#, r#""format": "safetensors""#)
            .replace(r#""source": "curated""#, r#""source": "mirror""#);
        let catalog = load_catalog(&catalog_json(&[entry])).unwrap();
        let m = &catalog.models[0];
        assert_eq!(m.task, ModelTask::Unknown);
        assert_eq!(m.format, ModelFormat::Unknown);
        assert_eq!(m.source, ModelSource::Unknown);
    }

    #[test]
    fn missing_required_field_rejected() {
        let entry = entry_json("m1").replace(r#""display_name": "Test Model","#, "");
        let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
        assert!(matches!(err, CatalogError::Parse(_)));
        assert!(err.to_string().contains("display_name"));
    }

    #[test]
    fn duplicate_ids_rejected() {
        let err = load_catalog(&catalog_json(&[entry_json("m1"), entry_json("m1")])).unwrap_err();
        assert!(err.to_string().contains("duplicate model id 'm1'"));
    }

    #[test]
    fn empty_models_rejected() {
        let err = load_catalog(r#"{ "catalog_version": "1", "models": [] }"#).unwrap_err();
        assert!(err.to_string().contains("at least one model"));
    }

    #[test]
    fn empty_id_rejected() {
        let err = load_catalog(&catalog_json(&[entry_json("")])).unwrap_err();
        assert!(err.to_string().contains("id must not be empty"));
    }

    #[test]
    fn out_of_range_score_rejected() {
        let entry = entry_json("m1").replace(r#""quality_score": 80"#, r#""quality_score": 101"#);
        let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
        assert!(err.to_string().contains("quality_score must be 0-100"));
    }

    #[test]
    fn non_https_url_rejected() {
        let entry =
            entry_json("m1").replace("https://example.com/m.gguf", "http://example.com/m.gguf");
        let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
        assert!(err.to_string().contains("must start with https://"));
    }

    #[test]
    fn bad_sha256_rejected() {
        let upper = TEST_SHA.to_uppercase();
        let non_hex = format!("{}g", &TEST_SHA[..63]);
        for bad in ["abc", upper.as_str(), non_hex.as_str()] {
            let entry = entry_json("m1").replace(TEST_SHA, bad);
            let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
            assert!(
                err.to_string().contains("64 lowercase hex"),
                "accepted: {bad}"
            );
        }
    }

    #[test]
    fn empty_files_rejected() {
        let entry = entry_json("m1");
        let start = entry.find(r#""files": ["#).unwrap() + r#""files": ["#.len();
        let end = entry[start..].find("}]").unwrap() + start + 1;
        let entry = format!("{}{}", &entry[..start], &entry[end..]);
        let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
        assert!(err.to_string().contains("download.files must not be empty"));
    }

    #[test]
    fn unsafe_model_id_rejected() {
        for bad in ["..", "a/b", "a\\b", "../evil", "a b", "a:b"] {
            let entry = entry_json(bad);
            let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
            assert!(
                err.to_string().contains("safe path segment"),
                "accepted: {bad}"
            );
        }
    }

    #[test]
    fn safe_path_segment_accepts_catalog_style_ids() {
        for good in ["phi4-mini-q4km", "qwen3.5-9b", "kokoro_82m", "m1"] {
            assert!(is_safe_path_segment(good), "rejected: {good}");
        }
        for bad in ["", ".", "..", "a/b", "a\\b", "a b"] {
            assert!(!is_safe_path_segment(bad), "accepted: {bad}");
        }
    }

    #[test]
    fn invalid_dest_path_rejected() {
        for (bad, expected) in [
            ("", "dest_path must not be empty"),
            ("../evil.gguf", "relative path"),
            ("/abs/path.gguf", "relative path"),
            ("sub\\win.gguf", "relative path"),
            ("C:/evil.gguf", "relative path"),
            ("C:evil.gguf", "relative path"),
        ] {
            let entry = entry_json("m1").replace(
                r#""dest_path": "m.gguf""#,
                &format!(r#""dest_path": "{}""#, bad.replace('\\', "\\\\")),
            );
            let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
            assert!(err.to_string().contains(expected), "accepted: {bad}");
        }
    }

    #[test]
    fn disk_bytes_mismatch_rejected() {
        for bad in ["999", "1001"] {
            let entry = entry_json("m1").replace(
                r#""disk_bytes": 1000,"#,
                &format!(r#""disk_bytes": {bad},"#),
            );
            let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
            assert!(
                err.to_string().contains("must equal sum of file sizes"),
                "accepted disk_bytes {bad}"
            );
        }
    }

    #[test]
    fn duplicate_dest_path_rejected() {
        let dup = r#"{ "url": "https://example.com/m.gguf", "sha256": "SHA", "dest_path": "m.gguf", "size_bytes": 100 }"#
            .replace("SHA", TEST_SHA);
        let entry = entry_json("m1").replace(
            r#""size_bytes": 1000 }]"#,
            &format!(r#""size_bytes": 100 }}, {dup}]"#),
        );
        let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
        assert!(err.to_string().contains("duplicate dest_path"));
    }

    #[test]
    fn deprecated_entry_with_replacement_parses() {
        let entry =
            entry_json("m1").replacen('{', r#"{ "deprecated": true, "replacement": "m2","#, 1);
        let catalog = load_catalog(&catalog_json(&[entry, entry_json("m2")])).unwrap();
        assert!(catalog.models[0].deprecated);
        assert_eq!(catalog.models[0].replacement.as_deref(), Some("m2"));
    }

    #[test]
    fn missing_perf_defaults_to_none() {
        let catalog = load_catalog(&catalog_json(&[entry_json("m1")])).unwrap();
        assert!(catalog.models[0].perf.is_none());
    }

    #[test]
    fn perf_field_parses() {
        let entry = entry_json("m1").replacen(
            '{',
            r#"{ "perf": { "tok_s_gpu": 45.0, "tok_s_cpu": 8.5 },"#,
            1,
        );
        let catalog = load_catalog(&catalog_json(&[entry])).unwrap();
        let perf = catalog.models[0].perf.expect("perf");
        assert_eq!(perf.tok_s_gpu, Some(45.0));
        assert_eq!(perf.tok_s_cpu, Some(8.5));
        assert!(perf.rtf_gpu.is_none());
        assert!(perf.rtf_cpu.is_none());
    }

    #[test]
    fn invalid_perf_values_rejected() {
        for bad in ["0.0", "-1.0"] {
            let entry = entry_json("m1").replacen(
                '{',
                &format!(r#"{{ "perf": {{ "rtf_cpu": {bad} }},"#),
                1,
            );
            let err = load_catalog(&catalog_json(&[entry])).unwrap_err();
            assert!(
                err.to_string().contains("perf.rtf_cpu"),
                "accepted: {bad}, got: {err}"
            );
        }
    }

    #[test]
    fn bundled_catalog_is_valid() {
        let catalog = bundled_catalog();
        assert!(!catalog.models.is_empty());
        for task in [
            ModelTask::Llm,
            ModelTask::Tts,
            ModelTask::Align,
            ModelTask::Layout,
        ] {
            assert!(
                catalog.models.iter().any(|m| m.task == task),
                "bundled catalog missing task {task:?}"
            );
        }
    }

    fn hf_url_pins_commit(url: &str) -> bool {
        let Some(rest) = url.strip_prefix("https://huggingface.co/") else {
            return true; // not a Hub URL; other hosts are out of this pin's scope
        };
        let Some((_, rev_and_path)) = rest.split_once("/resolve/") else {
            return false;
        };
        let Some((rev, _)) = rev_and_path.split_once('/') else {
            return false;
        };
        rev.len() == 40
            && rev
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn hf_url_pin_helper_accepts_only_full_commit_shas() {
        let sha = "0314792d7f1f7e229411f620751375812bb9faf2";
        assert!(hf_url_pins_commit(&format!(
            "https://huggingface.co/o/r/resolve/{sha}/a/b.gguf"
        )));
        assert!(!hf_url_pins_commit(
            "https://huggingface.co/o/r/resolve/main/b.gguf"
        ));
        assert!(!hf_url_pins_commit(&format!(
            "https://huggingface.co/o/r/resolve/{}/b.gguf",
            &sha[..39]
        )));
        assert!(!hf_url_pins_commit(&format!(
            "https://huggingface.co/o/r/resolve/{}/b.gguf",
            sha.to_uppercase()
        )));
        assert!(hf_url_pins_commit(
            "https://www.modelscope.cn/models/x/resolve/v3.9.0/f.pth"
        ));
    }

    #[test]
    fn bundled_replacements_point_at_live_entries() {
        let catalog = bundled_catalog();
        let live = |id: &str| catalog.models.iter().any(|m| m.id == id && !m.deprecated);
        let offenders: Vec<String> = catalog
            .models
            .iter()
            .filter(|m| m.deprecated)
            .filter(|m| !m.replacement.as_deref().is_some_and(live))
            .map(|m| format!("{}: replacement {:?}", m.id, m.replacement))
            .collect();
        assert!(
            offenders.is_empty(),
            "every deprecated entry needs a replacement that exists and is not itself deprecated:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn deprecated_qwen35_32b_class_and_its_replacement_share_the_same_weights_file() {
        let catalog = bundled_catalog();
        let entry = |id: &str| catalog.models.iter().find(|m| m.id == id).expect(id);
        let old = entry("qwen3.5-32b-class-q4km");
        let new = entry("qwen3.5-27b-q4km");
        assert!(old.deprecated);
        assert_eq!(old.replacement.as_deref(), Some("qwen3.5-27b-q4km"));
        assert!(!new.deprecated);
        let weights = |e: &CatalogEntry| {
            e.download
                .files
                .iter()
                .find(|f| !f.dest_path.contains("mmproj"))
                .map(|f| (f.url.clone(), f.sha256.clone(), f.size_bytes))
                .expect("weights file")
        };
        assert_eq!(weights(old), weights(new));
        assert!(new
            .download
            .files
            .iter()
            .any(|f| f.dest_path.contains("mmproj")));
    }

    #[test]
    fn bundled_catalog_hf_urls_pin_a_commit_sha() {
        let catalog = bundled_catalog();
        let offenders: Vec<String> = catalog
            .models
            .iter()
            .flat_map(|m| m.download.files.iter().map(move |f| (m, f)))
            .filter(|(_, f)| !hf_url_pins_commit(&f.url))
            .map(|(m, f)| format!("{}: {}", m.id, f.url))
            .collect();
        assert!(
            offenders.is_empty(),
            "huggingface.co URLs must use resolve/<40-hex commit>/, not a branch:\n{}",
            offenders.join("\n")
        );
    }

    const PERMITTED_WEIGHT_LICENCES: [&str; 7] = [
        "MIT",
        "Apache-2.0",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "ISC",
        "0BSD",
        "CDLA-Permissive-2.0",
    ];

    #[test]
    fn bundled_catalog_ships_only_permissive_commercial_weights() {
        let offenders: Vec<String> = bundled_catalog()
            .models
            .iter()
            .filter(|m| {
                !m.license.commercial_ok
                    || !m
                        .license
                        .spdx
                        .split(" AND ")
                        .all(|term| PERMITTED_WEIGHT_LICENCES.contains(&term.trim()))
            })
            .map(|m| format!("{}: {}", m.id, m.license.spdx))
            .collect();
        assert!(
            offenders.is_empty(),
            "owner licence rule 2026-10-20: weights must be permissive and commercial:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn bundled_catalog_lists_no_retired_model() {
        let listed: Vec<&str> = bundled_catalog()
            .models
            .iter()
            .map(|m| m.id.as_str())
            .filter(|id| crate::model_manager::RETIRED_MODEL_IDS.contains(id))
            .collect();
        assert!(listed.is_empty(), "retired models still listed: {listed:?}");
    }

    #[test]
    fn bundled_catalog_contains_wav2vec2_align_entry() {
        let catalog = bundled_catalog();
        let entry = catalog
            .models
            .iter()
            .find(|m| m.id == "wav2vec2-base-960h")
            .expect("wav2vec2-base-960h entry in bundled catalog");
        assert_eq!(entry.task, ModelTask::Align);
        assert_eq!(entry.family, "wav2vec2");
        assert_eq!(entry.format, ModelFormat::Pytorch);
        assert_eq!(entry.license.spdx, "Apache-2.0");
        assert!(entry.license.commercial_ok);
        assert_eq!(entry.download.files.len(), 7);
        let expected_files = [
            "config.json",
            "feature_extractor_config.json",
            "model.safetensors",
            "preprocessor_config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
            "vocab.json",
        ];
        for f in expected_files {
            assert!(
                entry.download.files.iter().any(|file| file.dest_path == f),
                "missing download file {f}"
            );
        }
        let total_size: u64 = entry.download.files.iter().map(|f| f.size_bytes).sum();
        assert_eq!(entry.disk_bytes, total_size);
        assert_eq!(entry.disk_bytes, 377_610_353);
    }
}
