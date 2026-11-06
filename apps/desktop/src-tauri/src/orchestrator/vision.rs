use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::BoxFuture;
use serde_json::json;

use super::layout::scoped_pages;
use super::overload::{connection_label, friendly_provider_error};
use super::provider_error_policy::{FaultClass, PatienceBudget, ProviderErrorPolicy};
use super::residency_check;
use super::scripting::{
    engine_capability, is_gpu_device, plan_json_from_fit, request_run_id, ConnectionWait,
    SidecarProvider,
};
use super::stage::{Stage, StageContext, StageOutcome};
use super::state::{JobState, OrchestratorError, Scope};
use super::stream::{post_sse_cancellable, SidecarTarget, StreamOutcome};
use super::vision_model::{
    cast_vision_model, mmproj_file, text_gguf_file, vision_route, VisionRoute,
};
use crate::catalog::bundled_catalog;
use crate::hardware;
use crate::providers::binding::PhaseBinding;
use crate::providers::registry::{ProviderKind, ScriptError, ScriptErrorKind};
use crate::providers::ConnectionStore;
use crate::sidecar::client::SseEvent;
use crate::storage::{book, job, models, SqlitePool};
use crate::voice::keys::{ApiKey, KeyManager};

pub const REGION_TIMEOUT_S: f64 = 40.0;
pub const FLAG_SKIPPED_NO_MODEL: &str = "vision_skipped_no_model";

pub struct VisionStage {
    client: reqwest::Client,
    sidecar: Arc<dyn SidecarProvider>,
    models_root: PathBuf,
    cache_dir: PathBuf,
    keys: Option<Arc<KeyManager>>,
}

struct LocalVisionPrep {
    model_id: String,
    plan: serde_json::Value,
    model_path: PathBuf,
    mmproj_path: PathBuf,
    family: String,
    entry: crate::catalog::CatalogEntry,
    mmproj_bytes: u64,
}

impl VisionStage {
    pub fn new(
        sidecar: Arc<dyn SidecarProvider>,
        models_root: PathBuf,
        cache_dir: PathBuf,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(super::scripting::SIDECAR_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        VisionStage {
            client,
            sidecar,
            models_root,
            cache_dir,
            keys: None,
        }
    }

    pub fn with_key_manager(mut self, key_manager: impl Into<Option<Arc<KeyManager>>>) -> Self {
        self.keys = key_manager.into();
        self
    }

    async fn healthy_connection(
        &self,
        ctx: &StageContext,
    ) -> Result<ConnectionWait, OrchestratorError> {
        if ctx.checkpoint.recovery_failures > 0 {
            tracing::info!(
                job_id = %ctx.job_id,
                recovery_failures = ctx.checkpoint.recovery_failures,
                "recovery revision detected in vision stage; respawning sidecar"
            );
            self.sidecar.respawn();
        }
        let deadline = tokio::time::Instant::now() + super::scripting::SIDECAR_READY_TIMEOUT;
        loop {
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(ConnectionWait::Interrupted(outcome));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(OrchestratorError::SidecarUnavailable);
            }
            if let Some((port, token)) = self.sidecar.connection() {
                let url = format!("http://127.0.0.1:{port}/health");
                let is_healthy = self
                    .client
                    .get(&url)
                    .bearer_auth(&token)
                    .timeout(std::time::Duration::from_secs(2))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);

                if is_healthy {
                    return Ok(ConnectionWait::Ready(port, token));
                } else {
                    tracing::warn!(
                        port = port,
                        "sidecar connection reported port {port}, but /health probe failed; requesting respawn"
                    );
                    self.sidecar.respawn();
                }
            }
            tokio::time::sleep(super::scripting::SIDECAR_READY_POLL).await;
        }
    }

    async fn select_vision_alternative(
        &self,
        ctx: &StageContext,
        port: u16,
        token: &str,
        prep: &LocalVisionPrep,
    ) -> Result<serde_json::Value, OrchestratorError> {
        if !is_gpu_device(&prep.plan) {
            return Ok(prep.plan.clone());
        }
        if engine_capability(&self.client, port, token, "gpu_offload", true).await {
            return Ok(prep.plan.clone());
        }
        tracing::info!("engine runtime lacks gpu_offload; recomputing the vision cpu alternative");
        let gguf_info = crate::catalog::gguf::read_model_info(&prep.model_path).ok();
        let profile = hardware::detect(
            &self.models_root,
            hardware::DirOrigin::Default,
            &self.models_root,
        )
        .await;
        let plan = plan_json_from_fit(
            hardware::plan::vision_cpu_fit(
                &prep.entry,
                &profile,
                gguf_info.as_ref(),
                prep.mmproj_bytes,
            ),
            &prep.model_id,
            gguf_info.as_ref(),
            &profile.ram,
        );
        job::set_job_vision_plan(&ctx.pool, &ctx.job_id, &plan.to_string()).await?;
        residency_check::log_stage_plan(&ctx.job_id, JobState::Vision, &plan).await;
        Ok(plan)
    }

    async fn prepare_local_vision(
        &self,
        ctx: &StageContext,
        record: &job::RenderJob,
        recorded_model: Option<&str>,
    ) -> Result<Option<LocalVisionPrep>, OrchestratorError> {
        let installs = models::list_model_installs(&ctx.pool).await?;
        let catalog = bundled_catalog();
        let resolved = cast_vision_model(
            recorded_model,
            record.model_overrides_json.as_deref(),
            catalog,
            &installs,
            &self.models_root,
        );
        if let Some((model_id, reason)) = &resolved.declined {
            tracing::info!("cast model '{model_id}' cannot explain figures: {reason}");
        }
        let Some(model_id) = resolved.model_id else {
            tracing::info!("no figure model for this cast; figures will use generic cues");
            return Ok(None);
        };
        let Some(install) = installs.iter().find(|i| i.model_id == model_id) else {
            tracing::info!("vision model '{model_id}' not installed; using generic cues");
            return Ok(None);
        };
        let Some(entry) = catalog.models.iter().find(|e| e.id == model_id) else {
            tracing::info!("vision model '{model_id}' not in catalog; using generic cues");
            return Ok(None);
        };
        let install_dir = self.models_root.join(&install.path);
        let Some(gguf_file) = text_gguf_file(entry) else {
            tracing::info!("vision model '{model_id}' has no text weights; using generic cues");
            return Ok(None);
        };
        let Some(mmproj) = mmproj_file(entry) else {
            tracing::info!("vision model '{model_id}' has no projector; using generic cues");
            return Ok(None);
        };
        let model_path = install_dir.join(&gguf_file.dest_path);
        let mmproj_path = install_dir.join(&mmproj.dest_path);
        if !model_path.is_file() || !mmproj_path.is_file() {
            tracing::info!("vision model '{model_id}' files missing; using generic cues");
            return Ok(None);
        }
        let mmproj_bytes = std::fs::metadata(&mmproj_path)
            .map(|m| m.len())
            .unwrap_or(0)
            .max(mmproj.size_bytes);
        let stored = super::scripting::stored_plan_for(
            record.vision_plan_json.as_deref(),
            &model_id,
            &ctx.job_id,
            "vision",
        )?;
        let plan = match stored {
            Some(stored) => {
                if stored.upcast {
                    let rewritten = job::rewrite_plan_if_unchanged(
                        &ctx.pool,
                        &ctx.job_id,
                        job::PlanColumn::Vision,
                        record.vision_plan_json.as_deref().unwrap_or_default(),
                        &stored.plan.to_string(),
                    )
                    .await?;
                    if !rewritten {
                        tracing::info!(
                            "job {} kept a concurrent vision plan write instead of its upcast",
                            ctx.job_id
                        );
                    }
                }
                stored.plan
            }
            None => {
                let gguf_info = crate::catalog::gguf::read_model_info(&model_path).ok();
                let profile = hardware::detect(
                    &self.models_root,
                    hardware::DirOrigin::Default,
                    &self.models_root,
                )
                .await;
                let plan = plan_json_from_fit(
                    hardware::plan::vision_fit(entry, &profile, gguf_info.as_ref(), mmproj_bytes),
                    &model_id,
                    gguf_info.as_ref(),
                    &profile.ram,
                );
                job::set_job_vision_plan(&ctx.pool, &ctx.job_id, &plan.to_string()).await?;
                residency_check::log_stage_plan(&ctx.job_id, JobState::Vision, &plan).await;
                plan
            }
        };
        Ok(Some(LocalVisionPrep {
            model_id,
            plan,
            model_path,
            mmproj_path,
            family: entry.family.clone(),
            entry: entry.clone(),
            mmproj_bytes,
        }))
    }

    async fn call_cloud_vision(
        &self,
        connection: &crate::providers::ConnectionRecord,
        model: &str,
        key: Option<&ApiKey>,
        b64: &str,
    ) -> Result<(String, String), ScriptError> {
        const VISION_PROMPT: &str = "Describe this figure or chart from a book concisely for an audiobook listener. Provide your response as JSON with two fields: 'caption' (a brief 1-2 sentence title/summary) and 'explanation' (a detailed explanation of what the image shows, data points, or meaning).";

        let (url, req) = match connection.provider {
            ProviderKind::Gemini => {
                let endpoint = format!(
                    "{}/v1beta/models/{}:generateContent",
                    connection.key_base_url().trim_end_matches('/'),
                    model
                );
                let body = json!({
                    "contents": [{
                        "role": "user",
                        "parts": [
                            { "text": VISION_PROMPT },
                            {
                                "inline_data": {
                                    "mime_type": "image/png",
                                    "data": b64
                                }
                            }
                        ]
                    }],
                    "generationConfig": {
                        "responseMimeType": "application/json"
                    }
                });
                let mut req = self.client.post(&endpoint).json(&body);
                if let Some(key) = key {
                    req = req.header("x-goog-api-key", key.expose());
                }
                (endpoint, req)
            }
            ProviderKind::OpenaiCompatible => {
                let endpoint = format!(
                    "{}/chat/completions",
                    connection.key_base_url().trim_end_matches('/')
                );
                let body = json!({
                    "model": model,
                    "messages": [
                        {
                            "role": "user",
                            "content": [
                                {
                                    "type": "text",
                                    "text": VISION_PROMPT
                                },
                                {
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:image/png;base64,{b64}")
                                    }
                                }
                            ]
                        }
                    ]
                });
                let mut req = self.client.post(&endpoint).json(&body);
                if let Some(key) = key {
                    req = req.header("Authorization", format!("Bearer {}", key.expose()));
                }
                (endpoint, req)
            }
            ProviderKind::Anthropic => {
                let endpoint = format!(
                    "{}/v1/messages",
                    connection.key_base_url().trim_end_matches('/')
                );
                let body = json!({
                    "model": model,
                    "max_tokens": 1024,
                    "messages": [
                        {
                            "role": "user",
                            "content": [
                                {
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": "image/png",
                                        "data": b64
                                    }
                                },
                                {
                                    "type": "text",
                                    "text": VISION_PROMPT
                                }
                            ]
                        }
                    ]
                });
                let mut req = self
                    .client
                    .post(&endpoint)
                    .json(&body)
                    .header("anthropic-version", "2023-06-01");
                if let Some(key) = key {
                    req = req.header("x-api-key", key.expose());
                }
                (endpoint, req)
            }
            other => {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: false,
                    message: format!("provider '{}' cannot serve Vision MLLM", other.id()),
                    retry_after_secs: None,
                    status: None,
                });
            }
        };

        let resp = req.send().await.map_err(|err| ScriptError {
            kind: ScriptErrorKind::Transport,
            retryable: true,
            message: format!("HTTP send error to {url}: {err}"),
            retry_after_secs: None,
            status: None,
        })?;

        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let text = resp.text().await.unwrap_or_default();

        if status == 429 {
            return Err(ScriptError {
                kind: ScriptErrorKind::Quota,
                retryable: true,
                message: format!("HTTP 429 quota exhausted: {text}"),
                retry_after_secs: retry_after,
                status: Some(status),
            });
        }

        if status == 401 || (status == 400 && text.contains("API_KEY_INVALID")) {
            return Err(ScriptError {
                kind: ScriptErrorKind::Auth,
                retryable: false,
                message: format!("HTTP {status} auth failed: {text}"),
                retry_after_secs: None,
                status: Some(status),
            });
        }

        if status >= 400 {
            return Err(ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: status >= 500,
                message: format!("HTTP {status} from provider: {text}"),
                retry_after_secs: retry_after,
                status: Some(status),
            });
        }

        let raw_output = match connection.provider {
            ProviderKind::Gemini => {
                let payload: serde_json::Value =
                    serde_json::from_str(&text).map_err(|err| ScriptError {
                        kind: ScriptErrorKind::Malformed,
                        retryable: false,
                        message: format!("malformed Gemini response: {err}"),
                        retry_after_secs: None,
                        status: Some(status),
                    })?;
                let parts = payload["candidates"][0]["content"]["parts"]
                    .as_array()
                    .ok_or_else(|| ScriptError {
                        kind: ScriptErrorKind::Malformed,
                        retryable: false,
                        message: "no candidate parts in Gemini response".to_string(),
                        retry_after_secs: None,
                        status: Some(status),
                    })?;
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            }
            ProviderKind::OpenaiCompatible => {
                let payload: serde_json::Value =
                    serde_json::from_str(&text).map_err(|err| ScriptError {
                        kind: ScriptErrorKind::Malformed,
                        retryable: false,
                        message: format!("malformed OpenAI response: {err}"),
                        retry_after_secs: None,
                        status: Some(status),
                    })?;
                payload["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            }
            ProviderKind::Anthropic => {
                let payload: serde_json::Value =
                    serde_json::from_str(&text).map_err(|err| ScriptError {
                        kind: ScriptErrorKind::Malformed,
                        retryable: false,
                        message: format!("malformed Anthropic response: {err}"),
                        retry_after_secs: None,
                        status: Some(status),
                    })?;
                let blocks = payload["content"].as_array().ok_or_else(|| ScriptError {
                    kind: ScriptErrorKind::Malformed,
                    retryable: false,
                    message: "no content blocks in Anthropic response".to_string(),
                    retry_after_secs: None,
                    status: Some(status),
                })?;
                blocks
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            }
            _ => String::new(),
        };

        if raw_output.trim().is_empty() {
            return Err(ScriptError {
                kind: ScriptErrorKind::Malformed,
                retryable: false,
                message: "provider returned empty vision text".to_string(),
                retry_after_secs: None,
                status: Some(status),
            });
        }

        Ok(parse_figure_output(&raw_output))
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_cloud_vision(
        &self,
        ctx: &mut StageContext,
        record: &job::RenderJob,
        connection_id: &str,
        requested_model: &str,
        figures: Vec<book::FigureRegion>,
        already_explained: usize,
        started: Instant,
    ) -> Result<StageOutcome, OrchestratorError> {
        let connection = ConnectionStore::get(&ctx.pool, connection_id)
            .await
            .map_err(|err| OrchestratorError::Engine {
                status: None,
                code: "ERR_PROVIDER_CONNECTION".to_string(),
                message: format!("the recorded vision connection cannot be loaded: {err}"),
            })?;
        if !connection.enabled {
            return Err(OrchestratorError::Engine {
                status: None,
                code: "ERR_PROVIDER_DISABLED".to_string(),
                message: format!("connection '{}' is disabled", connection.name),
            });
        }
        if !connection
            .provider
            .servable_phases()
            .contains(&crate::providers::PhaseCapability::VisionMllm)
        {
            return Err(OrchestratorError::Engine {
                status: None,
                code: "ERR_PROVIDER_UNSUPPORTED".to_string(),
                message: format!(
                    "provider '{}' cannot serve Vision MLLM",
                    connection.provider.id()
                ),
            });
        }

        let effective_model = if !requested_model.trim().is_empty() {
            requested_model.to_string()
        } else {
            connection.default_model.clone()
        };
        ctx.emit_progress(
            0.0,
            None,
            &match figures.len() {
                1 => format!("explaining 1 figure with {}", connection_label(&connection)),
                n => format!(
                    "explaining {n} figures with {}",
                    connection_label(&connection)
                ),
            },
        );

        let provider_id = connection.provider.vault_provider().id();
        let base_url = connection.key_base_url();
        let mut current_account: Option<String> = None;
        let mut current_key: Option<ApiKey> = None;

        if !connection.credential_not_required {
            let manager = self
                .keys
                .as_ref()
                .ok_or_else(|| OrchestratorError::Engine {
                    status: None,
                    code: "ERR_PROVIDER_KEY".to_string(),
                    message: "key custody unavailable for keyed cloud vision".to_string(),
                })?;
            let candidate = manager
                .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                .await
                .map_err(|err| OrchestratorError::Engine {
                    status: None,
                    code: "ERR_PROVIDER_KEY".to_string(),
                    message: format!("no usable key for connection '{}': {err}", connection.name),
                })?;
            current_account = Some(candidate.account);
            current_key = Some(candidate.key);
        }

        let total_figures = figures.len();
        let mut explained_in_this_run = 0;

        let is_overnight = record.overnight;
        let budget = PatienceBudget::for_mode(is_overnight);

        for (idx, figure) in figures.into_iter().enumerate() {
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            let hash = figure.crop_hash.as_deref().unwrap_or_default();
            let crop_path = self.crop_path(hash);
            if !crop_path.is_file() {
                tracing::warn!("figure crop file missing: {:?}", crop_path);
                continue;
            }
            let img_bytes =
                tokio::fs::read(&crop_path)
                    .await
                    .map_err(|e| OrchestratorError::Engine {
                        status: None,
                        code: "ERR_READ_CROP".to_string(),
                        message: format!("failed to read crop {hash}: {e}"),
                    })?;
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&img_bytes);

            let mut rotation_count = 0;
            let mut patience_retries = 0;

            let (caption, explanation) = loop {
                match self
                    .call_cloud_vision(&connection, &effective_model, current_key.as_ref(), &b64)
                    .await
                {
                    Ok((cap, exp)) => {
                        if let (Some(manager), Some(ref account)) =
                            (self.keys.as_ref(), &current_account)
                        {
                            manager.record_pool_outcome(account, Ok(()));
                        }
                        break (cap, exp);
                    }
                    Err(error) => {
                        let fault = ProviderErrorPolicy::classify_script_error(&error);
                        match fault {
                            FaultClass::ClusterOverload { .. } | FaultClass::TransientTransport => {
                                let retry_after = match fault {
                                    FaultClass::ClusterOverload { retry_after_secs } => {
                                        retry_after_secs
                                    }
                                    _ => None,
                                };
                                patience_retries += 1;
                                if patience_retries <= budget.max_cluster_attempts {
                                    let delay =
                                        budget.calculate_delay(patience_retries, retry_after);
                                    let delay_secs = delay.as_secs_f64();
                                    tracing::warn!(
                                        figure = %figure.id,
                                        attempt = patience_retries,
                                        max_attempts = budget.max_cluster_attempts,
                                        delay_secs,
                                        is_overnight,
                                        "cloud vision provider overloaded / transient transport; retrying on same key with jittered backoff"
                                    );
                                    let wait_label = if delay_secs >= 60.0 {
                                        format!("{:.0}m", (delay_secs / 60.0).ceil())
                                    } else {
                                        format!("{:.0}s", delay_secs)
                                    };
                                    let provider_label = connection_label(&connection);
                                    let status_msg = if is_overnight {
                                        format!(
                                            "figure {}/{} ({} busy; retrying overnight in {}... attempt {}/{})",
                                            idx + 1,
                                            total_figures,
                                            provider_label,
                                            wait_label,
                                            patience_retries,
                                            budget.max_cluster_attempts
                                        )
                                    } else {
                                        format!(
                                            "figure {}/{} ({} busy; retrying in {}... attempt {}/{})",
                                            idx + 1,
                                            total_figures,
                                            provider_label,
                                            wait_label,
                                            patience_retries,
                                            budget.max_cluster_attempts
                                        )
                                    };
                                    ctx.emit_progress(
                                        idx as f64 / total_figures as f64,
                                        None,
                                        &status_msg,
                                    );

                                    tokio::select! {
                                        outcome = ctx.control.interrupted() => {
                                            return Ok(outcome);
                                        }
                                        _ = tokio::time::sleep(delay) => {}
                                    }
                                    continue;
                                }
                            }
                            FaultClass::KeyAuthInvalid => {
                                if let (Some(manager), Some(ref account)) =
                                    (self.keys.as_ref(), &current_account)
                                {
                                    manager.record_dial_auth_failure(account, None);
                                    manager.advance_pool_cursor(account);
                                    if let Ok(candidate) = manager
                                        .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                        .await
                                    {
                                        if Some(&candidate.account) != current_account.as_ref() {
                                            rotation_count += 1;
                                            tracing::info!(
                                                old_account = ?current_account,
                                                new_account = %candidate.account,
                                                figure = %figure.id,
                                                rotation = rotation_count,
                                                "rotated key for cloud vision after auth rejection; continuing without sleep"
                                            );
                                            current_account = Some(candidate.account);
                                            current_key = Some(candidate.key);
                                            continue;
                                        }
                                    }
                                }
                            }
                            FaultClass::KeyRateLimited { retry_after_secs } => {
                                if let (Some(manager), Some(ref account)) =
                                    (self.keys.as_ref(), &current_account)
                                {
                                    manager.record_dial_quota(account, None, retry_after_secs);
                                    manager.advance_pool_cursor(account);
                                    if let Ok(candidate) = manager
                                        .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                        .await
                                    {
                                        if Some(&candidate.account) != current_account.as_ref() {
                                            rotation_count += 1;
                                            tracing::info!(
                                                old_account = ?current_account,
                                                new_account = %candidate.account,
                                                figure = %figure.id,
                                                rotation = rotation_count,
                                                "rotated key for cloud vision after 429 rate limit; continuing without sleep"
                                            );
                                            current_account = Some(candidate.account);
                                            current_key = Some(candidate.key);
                                            continue;
                                        }
                                    }
                                }

                                patience_retries += 1;
                                if patience_retries <= budget.max_cluster_attempts {
                                    let wait_secs = retry_after_secs
                                        .unwrap_or(60)
                                        .min(if is_overnight { 300 } else { 60 });
                                    tracing::warn!(
                                        figure = %figure.id,
                                        attempt = patience_retries,
                                        wait_secs,
                                        is_overnight,
                                        "cloud vision rate-limited; cooling down before retry"
                                    );
                                    let provider_label = connection_label(&connection);
                                    let wait_label = if wait_secs >= 60 {
                                        format!("{}m", wait_secs.div_ceil(60))
                                    } else {
                                        format!("{}s", wait_secs)
                                    };
                                    let status_msg = format!(
                                        "figure {}/{} ({} rate-limited; retrying in {}...)",
                                        idx + 1,
                                        total_figures,
                                        provider_label,
                                        wait_label
                                    );
                                    ctx.emit_progress(
                                        idx as f64 / total_figures as f64,
                                        None,
                                        &status_msg,
                                    );
                                    tokio::select! {
                                        outcome = ctx.control.interrupted() => {
                                            return Ok(outcome);
                                        }
                                        _ = tokio::time::sleep(std::time::Duration::from_secs(wait_secs)) => {}
                                    }

                                    if let Some(manager) = self.keys.as_ref() {
                                        if let Ok(candidate) = manager
                                            .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                            .await
                                        {
                                            current_account = Some(candidate.account);
                                            current_key = Some(candidate.key);
                                        }
                                    }
                                    continue;
                                }
                            }
                            FaultClass::Terminal => {}
                        }

                        // Terminal failure
                        let friendly =
                            friendly_provider_error(&connection.name, error.status, &error.message);
                        let failure = crate::providers::registry::ProviderFailure {
                            class: match error.kind {
                                ScriptErrorKind::Auth => "auth",
                                ScriptErrorKind::Quota => "quota",
                                ScriptErrorKind::Transport => "transport",
                                ScriptErrorKind::Timeout => "timeout",
                                _ => "provider",
                            }
                            .to_string(),
                            retryable: error.retryable,
                            retry_after_secs: error.retry_after_secs,
                            detail: friendly.clone(),
                            status: error.status,
                        };
                        if let Ok(json) = serde_json::to_value(&failure) {
                            let _ = ctx.merge_report_value("provider_failure", json).await;
                        }

                        return Err(OrchestratorError::Engine {
                            status: error.status,
                            code: "ERR_CLOUD_VISION".to_string(),
                            message: friendly,
                        });
                    }
                }
            };

            book::set_figure_vision_output(&ctx.pool, &figure.id, &caption, &explanation, 0.95)
                .await?;
            explained_in_this_run += 1;
            ctx.emit_progress(
                (idx + 1) as f64 / total_figures as f64,
                None,
                &format!("figure {}/{}", idx + 1, total_figures),
            );
            ctx.checkpoint.done_figures = already_explained as i64 + (idx + 1) as i64;
            ctx.save_checkpoint().await?;
        }

        merge_report(
            ctx,
            record,
            &[],
            already_explained + explained_in_this_run,
            started.elapsed().as_secs(),
            None,
        )
        .await?;
        let total_done = already_explained + explained_in_this_run;
        let message = match total_done {
            0 => "no figures to explain".to_string(),
            1 => "1 figure explained".to_string(),
            n => format!("{n} figures explained"),
        };
        ctx.emit_progress(1.0, Some(0), &message);
        Ok(StageOutcome::Completed)
    }

    fn crop_path(&self, crop_hash: &str) -> PathBuf {
        compose_crop_path(&self.cache_dir, crop_hash)
    }
}

fn parse_figure_output(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    let cleaned = if let Some(stripped) = trimmed.strip_prefix("```json") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else if let Some(stripped) = trimmed.strip_prefix("```") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else {
        trimmed
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(cleaned) {
        let cap = v
            .get("caption")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();
        let exp = v
            .get("explanation")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .trim();
        if !cap.is_empty() || !exp.is_empty() {
            let final_cap = if cap.is_empty() {
                exp.to_string()
            } else {
                cap.to_string()
            };
            let final_exp = if exp.is_empty() {
                cap.to_string()
            } else {
                exp.to_string()
            };
            return (final_cap, final_exp);
        }
    }
    if let Some((first, rest)) = trimmed.split_once('.') {
        (format!("{}.", first.trim()), rest.trim().to_string())
    } else {
        (trimmed.to_string(), trimmed.to_string())
    }
}

fn compose_crop_path(cache_dir: &std::path::Path, crop_hash: &str) -> PathBuf {
    if crop_hash.len() < 2 {
        tracing::warn!(
            hash_len = crop_hash.len(),
            "vision crop hash is shorter than a cache shard; composing a degraded path — the hash arrives from the sidecar, so this means a corrupt or missing crop_hash on a figure region"
        );
    }
    crate::cache_path::cache_path(cache_dir, crop_hash)
}

async fn figures_in_scope(
    pool: &SqlitePool,
    book_id: &str,
    scope: &Scope,
) -> Result<Vec<book::FigureRegion>, OrchestratorError> {
    let book = book::get_book(pool, book_id)
        .await?
        .ok_or_else(|| OrchestratorError::BookNotFound(book_id.to_string()))?;
    let chapters = book::list_chapters(pool, book_id).await?;
    let pages: HashSet<i64> = scoped_pages(scope, &chapters, book.page_count)
        .into_iter()
        .collect();
    Ok(book::list_figures_for_book(pool, book_id)
        .await?
        .into_iter()
        .filter(|f| pages.contains(&f.page_no))
        .collect())
}

fn awaits_explanation(figure: &book::FigureRegion) -> bool {
    figure.crop_hash.is_some() && figure.caption.is_none()
}

pub async fn explains_figures_locally(
    pool: &SqlitePool,
    models_root: &Path,
    book_id: &str,
    scope: &Scope,
    model_overrides_json: Option<&str>,
) -> Result<bool, OrchestratorError> {
    let VisionRoute::Local { recorded_model } =
        vision_route(model_overrides_json).map_err(OrchestratorError::InvalidRequest)?
    else {
        return Ok(false);
    };
    let figures = figures_in_scope(pool, book_id, scope).await?;
    if !figures.iter().any(awaits_explanation) {
        return Ok(false);
    }
    let installs = models::list_model_installs(pool).await?;
    let resolved = cast_vision_model(
        recorded_model.as_deref(),
        model_overrides_json,
        bundled_catalog(),
        &installs,
        models_root,
    );
    Ok(resolved.model_id.is_some())
}

fn region_timeout(plan: &serde_json::Value) -> Option<f64> {
    match plan.get("badge").and_then(|v| v.as_str()) {
        Some("orange") | Some("red") => Some(REGION_TIMEOUT_S),
        _ => None,
    }
}

async fn merge_report(
    ctx: &StageContext,
    _record: &job::RenderJob,
    flags: &[String],
    explained: usize,
    elapsed_s: u64,
    badge: Option<&str>,
) -> Result<(), OrchestratorError> {
    let fresh_record = job::get_job(&ctx.pool, &ctx.job_id)
        .await?
        .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
    let mut report: serde_json::Value = fresh_record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !report["durations_s"].is_object() {
        report["durations_s"] = json!({});
    }
    report["durations_s"]["vision"] = json!(elapsed_s);
    report["vision_flags"] = json!(flags);
    if !report["figures"].is_object() {
        report["figures"] = json!({});
    }
    report["figures"]["explained"] = json!(explained);
    if let Some(badge) = badge {
        report["figures"]["badge"] = json!(badge);
    }
    job::set_job_report_only(&ctx.pool, &ctx.job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

impl Stage for VisionStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            let started = Instant::now();

            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let scope: Scope = serde_json::from_str(&record.scope_json)?;
            let scoped = figures_in_scope(&ctx.pool, &record.book_id, &scope).await?;
            let already_explained = scoped.iter().filter(|f| f.caption.is_some()).count();
            let figures: Vec<book::FigureRegion> =
                scoped.into_iter().filter(awaits_explanation).collect();
            if figures.is_empty() {
                merge_report(
                    ctx,
                    &record,
                    &[],
                    already_explained,
                    started.elapsed().as_secs(),
                    None,
                )
                .await?;
                let message = match already_explained {
                    0 => "no figures to explain".to_string(),
                    1 => "1 figure already explained".to_string(),
                    n => format!("{n} figures already explained"),
                };
                tracing::info!(
                    "vision skipped for job {}: {message} (no uncaptioned crops in scope)",
                    ctx.job_id
                );
                ctx.emit_progress(1.0, Some(0), &message);
                return Ok(StageOutcome::Completed);
            }

            let route = vision_route(record.model_overrides_json.as_deref())
                .map_err(OrchestratorError::InvalidRequest)?;
            let prep = match route {
                VisionRoute::Cloud(PhaseBinding::Connection {
                    connection_id,
                    model: requested_model,
                    provider,
                    endpoint,
                    adapter_rev,
                    ..
                }) => {
                    let connection = super::scripting_cloud::resolve_connection(
                        &ctx.pool,
                        &connection_id,
                        crate::providers::PhaseCapability::VisionMllm,
                    )
                    .await?;
                    super::scripting_cloud::enforce_snapshot(
                        &connection,
                        &provider,
                        &endpoint,
                        &adapter_rev,
                    )?;
                    return self
                        .run_cloud_vision(
                            ctx,
                            &record,
                            &connection_id,
                            &requested_model,
                            figures,
                            already_explained,
                            started,
                        )
                        .await;
                }
                VisionRoute::Local { recorded_model } => {
                    ctx.emit_progress(0.0, None, "loading the figure model");
                    self.prepare_local_vision(ctx, &record, recorded_model.as_deref())
                        .await?
                }
                VisionRoute::Cloud(PhaseBinding::Local { .. })
                | VisionRoute::Skip
                | VisionRoute::CloudCastWithoutVision => None,
            };
            let Some(prep) = prep else {
                merge_report(
                    ctx,
                    &record,
                    &[FLAG_SKIPPED_NO_MODEL.to_string()],
                    0,
                    started.elapsed().as_secs(),
                    None,
                )
                .await?;
                ctx.emit_progress(1.0, Some(0), "no figure model; generic cues");
                return Ok(StageOutcome::Completed);
            };

            let (port, token) = match self.healthy_connection(ctx).await? {
                ConnectionWait::Ready(port, token) => (port, token),
                ConnectionWait::Interrupted(outcome) => return Ok(outcome),
            };
            let plan = self
                .select_vision_alternative(ctx, port, &token, &prep)
                .await?;
            let vision_badge = plan
                .get("badge")
                .and_then(|b| b.as_str())
                .map(str::to_string);

            let requested_ids: HashSet<&str> = figures.iter().map(|f| f.id.as_str()).collect();
            let regions: Vec<serde_json::Value> = figures
                .iter()
                .map(|f| {
                    let hash = f.crop_hash.as_deref().unwrap_or_default();
                    json!({
                        "region_id": f.id,
                        "crop_path": self.crop_path(hash).to_string_lossy(),
                        "region_type": f.kind,
                    })
                })
                .collect();
            let body = json!({
                "regions": regions,
                "model_id": prep.model_id,
                "model_path": prep.model_path.to_string_lossy(),
                "mmproj_path": prep.mmproj_path.to_string_lossy(),
                "model_family": prep.family,
                "resource_plan": plan,
                "region_timeout_s": region_timeout(&plan),
            });

            let mut captions: Vec<(String, String, String, f64)> = Vec::new();
            let mut flags: Vec<String> = Vec::new();
            let mut cancelled = false;
            let mut sidecar_error: Option<OrchestratorError> = None;
            let mut done_figures: i64 = 0;
            let run_id = request_run_id(&ctx.job_id);
            let mut control = ctx.control.clone();
            // The engine loads the model inside this one SSE call and unloads
            // it when the run's lease is released, so a sample taken after the
            // stream would measure an empty engine. The first event is the
            // earliest moment the model is provably resident; a task parked on
            // the notify takes the after-load sample there, and the result is
            // read back only after the stream completes so `report_json` keeps
            // its single writer (SINGLE_WORKER_ASSUMPTIONS S1).
            let ram_probe = residency_check::begin_ram_plan(&plan).await;
            let model_loaded = std::sync::Arc::new(tokio::sync::Notify::new());
            let after_load_sample = ram_probe.map(|_| {
                let loaded = model_loaded.clone();
                tokio::spawn(async move {
                    loaded.notified().await;
                    hardware::probe_ram_free_mb().await
                })
            });
            let mut any_event_seen = false;
            let stream_result = post_sse_cancellable(
                SidecarTarget::new(&self.client, port, &token),
                "/vision",
                &run_id,
                body,
                &mut control,
                |event: SseEvent| {
                    if !any_event_seen {
                        any_event_seen = true;
                        model_loaded.notify_one();
                    }
                    match event.event.as_str() {
                        "caption" => {
                            let region_id = event.data.get("region_id").and_then(|v| v.as_str());
                            match region_id {
                                Some(id) if requested_ids.contains(id) => {
                                    if let (Some(caption), Some(explanation), Some(conf)) = (
                                        event.data.get("caption").and_then(|v| v.as_str()),
                                        event.data.get("explanation").and_then(|v| v.as_str()),
                                        event.data.get("confidence").and_then(|v| v.as_f64()),
                                    ) {
                                        captions.push((
                                            id.to_string(),
                                            caption.to_string(),
                                            explanation.to_string(),
                                            conf,
                                        ));
                                    }
                                }
                                Some(id) => {
                                    sidecar_error = Some(OrchestratorError::Engine {
                                        status: None,
                                        code: "ERR_INVALID_REGION".to_string(),
                                        message: format!("unrequested vision region '{id}'"),
                                    });
                                }
                                None => {
                                    sidecar_error = Some(OrchestratorError::Engine {
                                        status: None,
                                        code: "ERR_INVALID_REGION".to_string(),
                                        message: "caption event without region_id".to_string(),
                                    });
                                }
                            }
                        }
                        "progress" => {
                            if let (Some(figure), Some(total)) = (
                                event.data.get("figure").and_then(|v| v.as_i64()),
                                event.data.get("total").and_then(|v| v.as_i64()),
                            ) {
                                done_figures = figure;
                                if total > 0 {
                                    ctx.emit_progress(
                                        figure as f64 / total as f64,
                                        None,
                                        &format!("figure {figure}/{total}"),
                                    );
                                }
                            }
                        }
                        "result" => {
                            if let Some(value) = event.data.get("flags") {
                                flags = serde_json::from_value::<Vec<String>>(value.clone())
                                    .unwrap_or_default();
                            }
                        }
                        "cancelled" => cancelled = true,
                        "error" => {
                            sidecar_error = Some(OrchestratorError::from_engine_event(
                                &event.data,
                                "vision failed",
                            ));
                        }
                        _ => {}
                    }
                },
            )
            .await;

            if let (Some(probe), Some(sample)) = (ram_probe, after_load_sample) {
                if any_event_seen {
                    if let Ok(after_load_free) = sample.await {
                        if let Err(error) = residency_check::record_stage_ram_plan(
                            &ctx.pool,
                            &ctx.job_id,
                            JobState::Vision,
                            probe,
                            after_load_free,
                        )
                        .await
                        {
                            tracing::warn!(
                                "ram residency telemetry failed for job {} stage VISION: {error}",
                                ctx.job_id
                            );
                        }
                    }
                } else {
                    sample.abort();
                }
            }

            let explained = already_explained + captions.len();
            tracing::info!(
                job = %ctx.job_id,
                "vision summary: captioned={} already_explained={already_explained}",
                captions.len()
            );
            for (region_id, caption, explanation, confidence) in &captions {
                book::set_figure_vision_output(
                    &ctx.pool,
                    region_id,
                    caption,
                    explanation,
                    *confidence,
                )
                .await?;
            }
            ctx.checkpoint.done_figures = already_explained as i64 + done_figures;
            ctx.save_checkpoint().await?;
            let stream_outcome = stream_result.map_err(OrchestratorError::from)?;
            if let StreamOutcome::Interrupted(outcome) = stream_outcome {
                return Ok(outcome);
            }

            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            if cancelled {
                return Ok(StageOutcome::Cancelled);
            }
            if let Some(error) = sidecar_error {
                return Err(error);
            }

            merge_report(
                ctx,
                &record,
                &flags,
                explained,
                started.elapsed().as_secs(),
                vision_badge.as_deref(),
            )
            .await?;
            ctx.emit_progress(1.0, Some(0), &format!("{explained} figures explained"));
            Ok(StageOutcome::Completed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::degrade::cpu_fallback;
    use serde_json::json;

    #[test]
    fn region_timeout_set_for_orange_and_red_badges() {
        assert_eq!(
            region_timeout(&json!({"badge": "orange"})),
            Some(REGION_TIMEOUT_S)
        );
        assert_eq!(
            region_timeout(&json!({"badge": "red"})),
            Some(REGION_TIMEOUT_S)
        );
    }

    #[test]
    fn region_timeout_absent_for_gpu_badges() {
        assert_eq!(region_timeout(&json!({"badge": "green"})), None);
        assert_eq!(region_timeout(&json!({"badge": "yellow"})), None);
        assert_eq!(region_timeout(&json!({})), None);
    }

    #[test]
    fn region_timeout_applies_after_runtime_clamp() {
        let plan = json!({
            "device": "cuda",
            "n_gpu_layers": 12,
            "badge": "green",
            "tok_s_est": 55.0
        });
        let clamped = cpu_fallback(&plan);
        assert_eq!(region_timeout(&clamped), Some(REGION_TIMEOUT_S));
    }
}

#[cfg(test)]
mod crop_path_tests {
    use super::compose_crop_path;
    use std::path::Path;

    #[test]
    fn a_full_hash_is_sharded_by_its_first_two_characters() {
        assert_eq!(
            compose_crop_path(Path::new("/data/cache"), "abcdef"),
            Path::new("/data/cache/ab/abcdef")
        );
    }

    #[test]
    fn a_hash_shorter_than_a_shard_degrades_instead_of_panicking() {
        assert_eq!(
            compose_crop_path(Path::new("/data/cache"), "a"),
            Path::new("/data/cache/a/a"),
            "the crop hash arrives from the sidecar over HTTP; a corrupt one-char hash must not crash the vision stage"
        );
    }

    #[test]
    fn an_empty_hash_from_a_missing_crop_degrades_instead_of_panicking() {
        assert_eq!(
            compose_crop_path(Path::new("/data/cache"), ""),
            Path::new("/data/cache")
        );
    }
}

#[cfg(test)]
mod local_figure_model_tests {
    use super::*;
    use crate::model_manager::install_rel_dir;
    use crate::storage::models::ModelInstall;

    const MODEL: &str = "gemma4-e2b-q4km";

    struct Library {
        _dir: tempfile::TempDir,
        pool: SqlitePool,
        models_root: PathBuf,
    }

    async fn library(caption: Option<&str>, model_ready: bool) -> Library {
        let dir = tempfile::tempdir().expect("dir");
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .expect("pool");
        sqlx::query(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at, \
             page_count) VALUES ('b1', 'Book', '/b.pdf', 'pdf', 'h', 0, 10)",
        )
        .execute(&pool)
        .await
        .expect("book");
        book::insert_figure_region_ignore_conflict(
            &pool,
            &book::FigureRegion {
                id: "f1".into(),
                book_id: "b1".into(),
                page_no: 4,
                bbox_json: "[0,0,1,1]".into(),
                kind: "figure".into(),
                confidence: 0.9,
                crop_hash: Some("ab12".into()),
                caption: caption.map(str::to_owned),
                explanation: None,
                caption_confidence: None,
            },
        )
        .await
        .expect("figure");
        let models_root = dir.path().join("models");
        if model_ready {
            let entry = bundled_catalog()
                .models
                .iter()
                .find(|e| e.id == MODEL)
                .expect("entry");
            let rel = install_rel_dir(entry);
            std::fs::create_dir_all(models_root.join(&rel)).expect("model dir");
            for file in &entry.download.files {
                std::fs::write(models_root.join(&rel).join(&file.dest_path), b"stub")
                    .expect("model file");
            }
            models::insert_model_install(
                &pool,
                &ModelInstall {
                    model_id: MODEL.into(),
                    path: rel.clone(),
                    sha256: "deadbeef".into(),
                    disk_bytes: 1,
                    source: "curated".into(),
                    installed_at: 0,
                },
            )
            .await
            .expect("install");
        }
        Library {
            _dir: dir,
            pool,
            models_root,
        }
    }

    fn pages(page_start: i64, page_end: i64) -> Scope {
        Scope::Pages {
            page_start,
            page_end,
        }
    }

    async fn local(lib: &Library, scope: Scope, overrides: Option<&str>) -> bool {
        explains_figures_locally(&lib.pool, &lib.models_root, "b1", &scope, overrides)
            .await
            .expect("route")
    }

    #[tokio::test]
    async fn a_waiting_figure_and_an_installed_figure_model_run_locally() {
        let lib = library(None, true).await;
        assert!(local(&lib, pages(1, 10), None).await);
        assert!(local(&lib, pages(1, 10), Some(r#"{"explain_figures": true}"#)).await);
    }

    #[tokio::test]
    async fn without_a_figure_model_figures_take_generic_cues() {
        let lib = library(None, false).await;
        assert!(!local(&lib, pages(1, 10), None).await);
    }

    #[tokio::test]
    async fn figures_already_explained_or_outside_the_scope_need_no_model() {
        let explained = library(Some("A chart"), true).await;
        assert!(!local(&explained, pages(1, 10), None).await);
        let elsewhere = library(None, true).await;
        assert!(!local(&elsewhere, pages(5, 10), None).await);
    }

    #[tokio::test]
    async fn figure_explanation_off_or_on_a_provider_needs_no_local_model() {
        let lib = library(None, true).await;
        assert!(!local(&lib, pages(1, 10), Some(r#"{"explain_figures": false}"#)).await);
        let cloud = serde_json::json!({
            "vision_binding": {
                "kind": "connection",
                "connection_id": "c1",
                "model": "m",
                "provider": "openai",
                "endpoint": "https://api.openai.com/v1",
                "adapter_rev": "1",
            }
        });
        assert!(!local(&lib, pages(1, 10), Some(&cloud.to_string())).await);
    }
}
