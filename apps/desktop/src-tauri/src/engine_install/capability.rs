//! What the running engine can do. The `lite` build reads books and prepares scripts for
//! cloud casts; only the `cpu` and `cuda13` builds run models on this computer.

use std::path::Path;

use super::install::Layout;
use super::manifest::LITE_VARIANT;
use crate::sidecar::protocol::EngineStatus;
use crate::sidecar::spawn::{explicit_dev_engine, DEV_ENGINE_ENV};

pub const NOT_INSTALLED: &str = "ERR_ENGINE_NOT_INSTALLED";
pub const LOCAL_AI_REQUIRED: &str = "ERR_ENGINE_LOCAL_AI_REQUIRED";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineNeed {
    Documents,
    LocalAi,
}

pub fn refusal(status: &EngineStatus, lite: bool, need: EngineNeed) -> Option<&'static str> {
    if matches!(
        status,
        EngineStatus::NotInstalled | EngineStatus::NeedsUpdate { .. }
    ) {
        return Some(NOT_INSTALLED);
    }
    (lite && need == EngineNeed::LocalAi).then_some(LOCAL_AI_REQUIRED)
}

/// A cast needs local models unless every model phase runs on a connected provider.
pub fn cast_need(
    cloud_script: bool,
    cloud_voice: bool,
    teacher: bool,
    narrator_llm: bool,
) -> EngineNeed {
    let script_runs_here = !cloud_script && (teacher || narrator_llm);
    if script_runs_here || !cloud_voice {
        EngineNeed::LocalAi
    } else {
        EngineNeed::Documents
    }
}

impl EngineNeed {
    fn wire(self) -> &'static str {
        match self {
            EngineNeed::Documents => "documents",
            EngineNeed::LocalAi => "localAi",
        }
    }
}

/// The form a gate answers with: the UI opens the matching setup and resubmits.
pub fn refusal_message(code: &str, need: EngineNeed) -> String {
    format!("{code}:{}", need.wire())
}

pub fn lite_engine_active(data_dir: &Path) -> bool {
    if explicit_dev_engine(std::env::var_os(DEV_ENGINE_ENV).as_deref()).is_some() {
        return false;
    }
    Layout {
        data_dir: data_dir.to_path_buf(),
    }
    .installed()
    .is_some_and(|engine| engine.variant == LITE_VARIANT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_outdated_engine_is_refused_for_every_need() {
        let outdated = EngineStatus::NeedsUpdate {
            installed_api: Some(0),
            required_api: 1,
        };
        for status in [EngineStatus::NotInstalled, outdated] {
            for need in [EngineNeed::Documents, EngineNeed::LocalAi] {
                assert_eq!(refusal(&status, false, need), Some(NOT_INSTALLED));
            }
        }
    }

    #[test]
    fn the_lite_engine_serves_documents_but_not_local_models() {
        let ready = EngineStatus::Starting;
        assert_eq!(refusal(&ready, true, EngineNeed::Documents), None);
        assert_eq!(
            refusal(&ready, true, EngineNeed::LocalAi),
            Some(LOCAL_AI_REQUIRED)
        );
        assert_eq!(refusal(&ready, false, EngineNeed::LocalAi), None);
    }

    #[test]
    fn a_refusal_names_what_the_setup_must_offer() {
        assert_eq!(
            refusal_message(NOT_INSTALLED, EngineNeed::LocalAi),
            "ERR_ENGINE_NOT_INSTALLED:localAi"
        );
    }

    #[test]
    fn only_a_cast_with_no_local_model_phase_stays_on_documents() {
        assert_eq!(cast_need(true, true, true, false), EngineNeed::Documents);
        assert_eq!(cast_need(false, true, false, false), EngineNeed::Documents);
        assert_eq!(cast_need(false, true, true, false), EngineNeed::LocalAi);
        assert_eq!(cast_need(false, true, false, true), EngineNeed::LocalAi);
        assert_eq!(cast_need(true, false, true, false), EngineNeed::LocalAi);
    }
}
