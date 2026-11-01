use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use super::ConnectionRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderGroup {
    Main,
    Others,
    Local,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    Bearer,
    #[serde(rename = "google_api_key")]
    GoogleHeader,
    DeepgramToken,
    OptionalBearer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDefinition {
    pub id: String,
    pub name: String,
    pub group: ProviderGroup,
    pub authentication: Authentication,
    pub custody_id: String,
    pub provider_type: Option<String>,
    pub base_url: Option<String>,
    pub models_path: String,
    pub key_url: Option<String>,
    pub privacy_url: Option<String>,
    pub documentation_url: String,
    pub api_version: Option<String>,
    pub local: bool,
    pub preferred_script: Vec<String>,
    #[serde(default)]
    pub preview_models: Vec<String>,
    #[serde(default)]
    pub retirements: std::collections::BTreeMap<String, String>,
    pub preferred_narration: Vec<String>,
    pub voices: Vec<String>,
    pub reviewed_on: String,
}

pub fn definitions() -> &'static [ProviderDefinition] {
    static DIRECTORY: OnceLock<Vec<ProviderDefinition>> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        serde_json::from_str(include_str!("../../fixtures/providers/directory.json"))
            .expect("the bundled provider directory must conform to its typed fixture")
    })
}

pub fn definition(id: &str) -> Result<&'static ProviderDefinition, String> {
    definitions()
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| "Unknown provider".to_string())
}

pub fn identity_for_url(custody_id: &str, url: &str) -> &'static str {
    if custody_id == "openai_compat"
        && matches!(url, "https://api.openai.com" | "https://api.openai.com/v1")
    {
        return "openai";
    }
    definitions()
        .iter()
        .find(|entry| entry.custody_id == custody_id && entry.base_url.as_deref() == Some(url))
        .map(|entry| entry.id.as_str())
        .unwrap_or("custom")
}

pub fn identity(record: &ConnectionRecord) -> &'static str {
    identity_for_url(
        record.provider.vault_provider().id(),
        &record.key_base_url(),
    )
}

pub fn same_credential_group(provider: &str, left: &str, right: &str) -> bool {
    left == right
        || (provider == "openai_compat"
            && identity_for_url(provider, left) == "openai"
            && identity_for_url(provider, right) == "openai")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_identity_never_follows_model_ownership_or_similar_hosts() {
        assert_eq!(
            identity_for_url("openai_compat", "https://api.groq.com/openai/v1"),
            "groq"
        );
        assert_eq!(
            identity_for_url("openai_compat", "https://api.openai.com/proxy"),
            "custom"
        );
        assert!(!same_credential_group(
            "openai_compat",
            "https://api.openai.com/v1",
            "https://api.openai.com.evil/v1"
        ));
        assert!(!same_credential_group(
            "gemini",
            "https://api.openai.com",
            "https://api.openai.com/v1"
        ));
        assert!(same_credential_group(
            "openai_compat",
            "https://api.openai.com",
            "https://api.openai.com/v1"
        ));
    }

    #[test]
    fn directory_has_one_openai_and_explicit_vendor_groups() {
        let entries = definitions();
        let ids: std::collections::HashSet<_> = entries.iter().map(|entry| &entry.id).collect();
        assert_eq!(entries.len(), ids.len());
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.group == ProviderGroup::Main)
                .count(),
            5
        );
        assert_eq!(
            definition("openai").unwrap().preferred_narration,
            ["gpt-realtime-2.1-mini"]
        );
        assert_eq!(definition("ollama").unwrap().group, ProviderGroup::Local);
        assert!(entries
            .iter()
            .all(|entry| entry.reviewed_on == "2026-10-27"));
    }
}
