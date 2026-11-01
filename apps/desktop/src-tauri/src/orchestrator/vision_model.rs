use std::path::Path;

use serde_json::Value;

use super::llm::resolve_llm_id;
use crate::catalog::{Catalog, CatalogEntry, DownloadFile, ModelTask};
use crate::model_manager::missing_files;
use crate::providers::binding::PhaseBinding;
use crate::storage::models::ModelInstall;

pub const FIGURE_EXPLANATION: &str = "figure_explanation";
pub const EXPLAIN_FIGURES_KEY: &str = "explain_figures";

pub fn mmproj_file(entry: &CatalogEntry) -> Option<&DownloadFile> {
    entry
        .download
        .files
        .iter()
        .find(|f| f.dest_path.contains("mmproj"))
}

pub fn text_gguf_file(entry: &CatalogEntry) -> Option<&DownloadFile> {
    entry
        .download
        .files
        .iter()
        .find(|f| f.dest_path.ends_with(".gguf") && !f.dest_path.contains("mmproj"))
}

pub fn entry_vision_capable(entry: &CatalogEntry) -> bool {
    entry.task == ModelTask::Llm
        && entry.capabilities.iter().any(|c| c == FIGURE_EXPLANATION)
        && mmproj_file(entry).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionDecline {
    NoCastModel,
    NotInCatalog,
    NotInstalled,
    NotVisionCapable,
    WeightsMissing,
    ProjectorMissing,
}

impl std::fmt::Display for VisionDecline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoCastModel => "no local model is installed for this cast",
            Self::NotInCatalog => "not in catalog",
            Self::NotInstalled => "not installed",
            Self::NotVisionCapable => "cannot explain figures",
            Self::WeightsMissing => "weights not downloaded",
            Self::ProjectorMissing => "projector not downloaded",
        })
    }
}

pub fn vision_readiness(
    entry: &CatalogEntry,
    install: &ModelInstall,
    models_root: &Path,
) -> Result<(), VisionDecline> {
    if !entry_vision_capable(entry)
        || text_gguf_file(entry).is_none()
        || mmproj_file(entry).is_none()
    {
        return Err(VisionDecline::NotVisionCapable);
    }
    let missing = missing_files(entry, &models_root.join(&install.path));
    if missing.is_empty() {
        Ok(())
    } else if missing.iter().all(|path| path.contains("mmproj")) {
        Err(VisionDecline::ProjectorMissing)
    } else {
        Err(VisionDecline::WeightsMissing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisionRoute {
    Skip,
    Cloud(PhaseBinding),
    CloudCastWithoutVision,
    Local { recorded_model: Option<String> },
}

pub fn explain_figures(overrides: &Value) -> bool {
    overrides.get(EXPLAIN_FIGURES_KEY).and_then(Value::as_bool) != Some(false)
}

pub fn vision_route(model_overrides_json: Option<&str>) -> Result<VisionRoute, String> {
    let overrides = model_overrides_json
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if !explain_figures(&overrides) {
        return Ok(VisionRoute::Skip);
    }
    if let Some(binding) = overrides.get("vision_binding") {
        if binding.is_null() {
            return Ok(VisionRoute::Skip);
        }
        let binding = serde_json::from_value::<PhaseBinding>(binding.clone()).map_err(|err| {
            format!(
                "this job's recorded vision_binding does not parse ({err}); refusing to \
                 re-resolve a recorded binding — fix or clear the binding explicitly"
            )
        })?;
        if matches!(binding, PhaseBinding::Connection { .. }) {
            return Ok(VisionRoute::Cloud(binding));
        }
    }
    match overrides
        .get("script_binding")
        .and_then(|binding| serde_json::from_value::<PhaseBinding>(binding.clone()).ok())
    {
        Some(PhaseBinding::Connection { .. }) => Ok(VisionRoute::CloudCastWithoutVision),
        Some(PhaseBinding::Local { model_id }) => Ok(VisionRoute::Local {
            recorded_model: Some(model_id),
        }),
        None => Ok(VisionRoute::Local {
            recorded_model: None,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastVisionModel {
    pub model_id: Option<String>,
    pub declined: Option<(String, VisionDecline)>,
}

pub fn cast_vision_model(
    recorded_model: Option<&str>,
    model_overrides_json: Option<&str>,
    catalog: &Catalog,
    installs: &[ModelInstall],
    models_root: &Path,
) -> CastVisionModel {
    let model_id = match recorded_model {
        Some(id) => id.to_string(),
        None => match resolve_llm_id(model_overrides_json, catalog, installs) {
            Ok(id) => id,
            Err(_) => {
                return CastVisionModel {
                    model_id: None,
                    declined: Some((String::new(), VisionDecline::NoCastModel)),
                }
            }
        },
    };
    let readiness = catalog
        .models
        .iter()
        .find(|entry| entry.id == model_id)
        .ok_or(VisionDecline::NotInCatalog)
        .and_then(|entry| {
            let install = installs
                .iter()
                .find(|install| install.model_id == model_id)
                .ok_or(VisionDecline::NotInstalled)?;
            vision_readiness(entry, install, models_root)
        });
    match readiness {
        Ok(()) => CastVisionModel {
            model_id: Some(model_id),
            declined: None,
        },
        Err(reason) => CastVisionModel {
            model_id: None,
            declined: Some((model_id, reason)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_catalog;
    use crate::model_manager::install_rel_dir;

    fn install(model_id: &str) -> ModelInstall {
        ModelInstall {
            model_id: model_id.to_string(),
            path: format!("llm/{model_id}/"),
            sha256: "deadbeef".into(),
            disk_bytes: 1,
            source: "curated".into(),
            installed_at: 0,
        }
    }

    fn catalog_entry(model_id: &str) -> &'static CatalogEntry {
        bundled_catalog()
            .models
            .iter()
            .find(|e| e.id == model_id)
            .expect("entry")
    }

    fn materialize_assets(models_root: &Path, model_id: &str) {
        let entry = catalog_entry(model_id);
        let dir = models_root.join(install_rel_dir(entry));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for file in &entry.download.files {
            std::fs::write(dir.join(&file.dest_path), b"stub").expect("write");
        }
    }

    fn materialize_weights_only(models_root: &Path, model_id: &str) {
        let entry = catalog_entry(model_id);
        let dir = models_root.join(install_rel_dir(entry));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let weights = text_gguf_file(entry).expect("weights");
        std::fs::write(dir.join(&weights.dest_path), b"stub").expect("write");
    }

    fn connection_binding() -> Value {
        serde_json::json!({
            "kind": "connection",
            "connection_id": "c1",
            "model": "m",
            "provider": "openai",
            "endpoint": "https://api.openai.com/v1",
            "adapter_rev": "1",
        })
    }

    #[test]
    fn no_overrides_routes_to_the_locally_resolved_cast_model() {
        assert_eq!(
            vision_route(None),
            Ok(VisionRoute::Local {
                recorded_model: None
            })
        );
    }

    #[test]
    fn explain_figures_false_skips_the_stage() {
        assert_eq!(
            vision_route(Some(r#"{"explain_figures": false}"#)),
            Ok(VisionRoute::Skip)
        );
        assert_eq!(
            vision_route(Some(r#"{"explain_figures": true}"#)),
            Ok(VisionRoute::Local {
                recorded_model: None
            })
        );
    }

    #[test]
    fn a_recorded_null_vision_binding_still_skips() {
        assert_eq!(
            vision_route(Some(r#"{"vision_binding": null}"#)),
            Ok(VisionRoute::Skip)
        );
    }

    #[test]
    fn a_recorded_cloud_vision_binding_replays_verbatim() {
        let overrides = serde_json::json!({ "vision_binding": connection_binding() });
        let route = vision_route(Some(&overrides.to_string())).expect("route");
        let expected: PhaseBinding = serde_json::from_value(connection_binding()).expect("binding");
        assert_eq!(route, VisionRoute::Cloud(expected));
    }

    #[test]
    fn a_cloud_script_without_a_vision_binding_has_no_figure_model() {
        let overrides = serde_json::json!({ "script_binding": connection_binding() });
        assert_eq!(
            vision_route(Some(&overrides.to_string())),
            Ok(VisionRoute::CloudCastWithoutVision)
        );
    }

    #[test]
    fn a_local_script_binding_names_the_figure_model() {
        let route = vision_route(Some(
            r#"{"script_binding": {"kind": "local", "model_id": "gemma4-e2b-q4km"}}"#,
        ));
        assert_eq!(
            route,
            Ok(VisionRoute::Local {
                recorded_model: Some("gemma4-e2b-q4km".into())
            })
        );
    }

    #[test]
    fn an_unparsable_vision_binding_refuses_instead_of_re_resolving() {
        assert!(vision_route(Some(r#"{"vision_binding": {"kind": "bogus"}}"#)).is_err());
    }

    #[test]
    fn the_cast_model_explains_figures_when_its_projector_is_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize_assets(dir.path(), "gemma4-e2b-q4km");
        let resolved = cast_vision_model(
            None,
            None,
            bundled_catalog(),
            &[install("gemma4-e2b-q4km")],
            dir.path(),
        );
        assert_eq!(resolved.model_id.as_deref(), Some("gemma4-e2b-q4km"));
        assert_eq!(resolved.declined, None);
    }

    #[test]
    fn the_llm_override_is_the_figure_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize_assets(dir.path(), "gemma4-e2b-q4km");
        materialize_assets(dir.path(), "qwen3.5-9b-instruct-q4km");
        let resolved = cast_vision_model(
            None,
            Some(r#"{"llm": "gemma4-e2b-q4km"}"#),
            bundled_catalog(),
            &[
                install("gemma4-e2b-q4km"),
                install("qwen3.5-9b-instruct-q4km"),
            ],
            dir.path(),
        );
        assert_eq!(resolved.model_id.as_deref(), Some("gemma4-e2b-q4km"));
    }

    #[test]
    fn a_missing_projector_declines_with_its_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize_weights_only(dir.path(), "qwen3.5-9b-instruct-q4km");
        let resolved = cast_vision_model(
            Some("qwen3.5-9b-instruct-q4km"),
            None,
            bundled_catalog(),
            &[install("qwen3.5-9b-instruct-q4km")],
            dir.path(),
        );
        assert_eq!(resolved.model_id, None);
        assert_eq!(
            resolved.declined,
            Some((
                "qwen3.5-9b-instruct-q4km".into(),
                VisionDecline::ProjectorMissing
            ))
        );
    }

    #[test]
    fn a_text_only_cast_model_declines_rather_than_borrowing_another() {
        let dir = tempfile::tempdir().expect("tempdir");
        materialize_assets(dir.path(), "phi4-mini-q4km");
        materialize_assets(dir.path(), "gemma4-e2b-q4km");
        let resolved = cast_vision_model(
            Some("phi4-mini-q4km"),
            None,
            bundled_catalog(),
            &[install("phi4-mini-q4km"), install("gemma4-e2b-q4km")],
            dir.path(),
        );
        assert_eq!(resolved.model_id, None);
        assert_eq!(
            resolved.declined,
            Some(("phi4-mini-q4km".into(), VisionDecline::NotVisionCapable))
        );
    }

    #[test]
    fn no_installed_llm_declines_with_no_cast_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolved = cast_vision_model(None, None, bundled_catalog(), &[], dir.path());
        assert_eq!(resolved.model_id, None);
        assert_eq!(
            resolved.declined.map(|(_, reason)| reason),
            Some(VisionDecline::NoCastModel)
        );
    }

    #[test]
    fn text_gguf_file_skips_mmproj_regardless_of_file_order() {
        let mut entry = catalog_entry("gemma4-e4b-q4km").clone();
        entry.download.files.reverse();
        assert!(entry.download.files[0].dest_path.contains("mmproj"));

        let selected = text_gguf_file(&entry).expect("text weights");

        assert!(!selected.dest_path.contains("mmproj"));
        assert!(selected.dest_path.ends_with(".gguf"));
    }

    #[test]
    fn entry_vision_capable_matches_catalog_contract() {
        assert!(entry_vision_capable(catalog_entry("gemma4-e2b-q4km")));
        assert!(entry_vision_capable(catalog_entry("gemma4-e2b-q4km")));
        assert!(entry_vision_capable(catalog_entry("gemma4-e4b-q4km")));
        assert!(entry_vision_capable(catalog_entry(
            "qwen3.5-9b-instruct-q4km"
        )));
        assert!(entry_vision_capable(catalog_entry("qwen3.5-27b-q4km")));
        assert!(!entry_vision_capable(catalog_entry(
            "qwen3.5-32b-class-q4km"
        )));
        assert!(!entry_vision_capable(catalog_entry("phi4-mini-q4km")));
        assert!(!entry_vision_capable(catalog_entry("kokoro-82m")));
    }

    #[test]
    fn every_live_catalog_llm_explains_figures() {
        for entry in bundled_catalog()
            .models
            .iter()
            .filter(|e| e.task == ModelTask::Llm && !e.deprecated)
        {
            assert!(
                entry_vision_capable(entry),
                "{} is a live LLM that cannot explain figures",
                entry.id
            );
        }
    }
}
