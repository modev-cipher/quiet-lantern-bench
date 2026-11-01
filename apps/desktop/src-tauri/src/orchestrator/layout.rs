use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::json;

use super::scripting::{
    await_connection, engine_capability, request_run_id, ConnectionWait, SidecarProvider,
};
use super::stage::{Stage, StageContext, StageOutcome};
use super::state::{OrchestratorError, Scope};
use super::stream::{post_sse_cancellable, SidecarTarget, StreamOutcome};
use crate::library::docling_artifacts_dir;
use crate::sidecar::client::SseEvent;
use crate::storage::{book, cache, job};

#[derive(Debug, Deserialize)]
struct RawRegion {
    id: String,
    page: i64,
    bbox: [f64; 4],
    #[serde(rename = "type")]
    kind: String,
    confidence: f64,
    crop_png_path: Option<String>,
}

pub struct LayoutStage {
    client: reqwest::Client,
    sidecar: Arc<dyn SidecarProvider>,
    models_root: PathBuf,
    cache_dir: PathBuf,
}

impl LayoutStage {
    pub fn new(
        sidecar: Arc<dyn SidecarProvider>,
        models_root: PathBuf,
        cache_dir: PathBuf,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(super::scripting::SIDECAR_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        LayoutStage {
            client,
            sidecar,
            models_root,
            cache_dir,
        }
    }
}

pub fn scoped_pages(
    scope: &Scope,
    chapters: &[book::Chapter],
    page_count: Option<i64>,
) -> Vec<i64> {
    let mut pages: Vec<i64> = match scope {
        Scope::Pages {
            page_start,
            page_end,
        } => {
            let end = page_count.map_or(*page_end, |count| (*page_end).min(count));
            (*page_start..=end).collect()
        }
        Scope::Chapters { chapter_ids } => {
            let mut collected: Vec<i64> = chapters
                .iter()
                .filter(|c| chapter_ids.contains(&c.id))
                .flat_map(|c| c.page_start..=c.page_end)
                .collect();
            collected.sort_unstable();
            collected.dedup();
            collected
        }
    };
    pages.retain(|p| *p >= 1);
    pages
}

async fn merge_report(
    ctx: &StageContext,
    record: &job::RenderJob,
    flags: &[String],
    detected: usize,
    elapsed_s: u64,
) -> Result<(), OrchestratorError> {
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !report["durations_s"].is_object() {
        report["durations_s"] = json!({});
    }
    report["durations_s"]["layout"] = json!(elapsed_s);
    report["layout_flags"] = json!(flags);
    if !report["figures"].is_object() {
        report["figures"] = json!({});
    }
    report["figures"]["detected"] = json!(detected);
    job::set_job_report_only(&ctx.pool, &ctx.job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

impl Stage for LayoutStage {
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
            let book = book::get_book(&ctx.pool, &record.book_id)
                .await?
                .ok_or_else(|| OrchestratorError::BookNotFound(record.book_id.clone()))?;
            let chapters = book::list_chapters(&ctx.pool, &record.book_id).await?;
            let scope: Scope = serde_json::from_str(&record.scope_json)?;
            let pages = scoped_pages(&scope, &chapters, book.page_count);
            if pages.is_empty() {
                merge_report(ctx, &record, &[], 0, started.elapsed().as_secs()).await?;
                ctx.emit_progress(1.0, Some(0), "no pages in scope");
                return Ok(StageOutcome::Completed);
            }

            let (port, token) = match await_connection(&*self.sidecar, &ctx.control).await? {
                ConnectionWait::Ready(port, token) => (port, token),
                ConnectionWait::Interrupted(outcome) => return Ok(outcome),
            };

            let forced_cpu = record
                .resource_plan_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|plan| crate::hardware::device::plan_device(&plan).ok())
                .map(|dev| dev.backend == crate::hardware::device::DeviceBackend::Cpu)
                .unwrap_or(false);

            let device = if !forced_cpu
                && engine_capability(&self.client, port, &token, "torch_cuda", false).await
            {
                "cuda"
            } else {
                "cpu"
            };
            let body = json!({
                "book_id": record.book_id,
                "book_path": book.source_path,
                "pages": pages,
                "content_hash": book.content_hash,
                "cache_dir": self.cache_dir.to_string_lossy(),
                "docling_artifacts_dir": docling_artifacts_dir(&self.models_root),
                "device": device,
            });

            let mut raw_regions: Option<Vec<RawRegion>> = None;
            let mut flags: Vec<String> = Vec::new();
            let mut cancelled = false;
            let mut sidecar_error: Option<OrchestratorError> = None;
            let mut done_pages: i64 = 0;
            let run_id = request_run_id(&ctx.job_id);
            let mut control = ctx.control.clone();
            let stream_result = post_sse_cancellable(
                SidecarTarget::new(&self.client, port, &token),
                "/layout",
                &run_id,
                body,
                &mut control,
                |event: SseEvent| match event.event.as_str() {
                    "result" => {
                        if let Some(value) = event.data.get("regions") {
                            raw_regions =
                                serde_json::from_value::<Vec<RawRegion>>(value.clone()).ok();
                        }
                        if let Some(value) = event.data.get("flags") {
                            flags = serde_json::from_value::<Vec<String>>(value.clone())
                                .unwrap_or_default();
                        }
                    }
                    "progress" => {
                        if let (Some(page), Some(total)) = (
                            event.data.get("page").and_then(|v| v.as_i64()),
                            event.data.get("total").and_then(|v| v.as_i64()),
                        ) {
                            done_pages = page;
                            if total > 0 {
                                ctx.emit_progress(
                                    page as f64 / total as f64,
                                    None,
                                    &format!("page {page}/{total}"),
                                );
                            }
                        }
                    }
                    "cancelled" => cancelled = true,
                    "error" => {
                        sidecar_error = Some(OrchestratorError::from_engine_event(
                            &event.data,
                            "layout failed",
                        ));
                    }
                    _ => {}
                },
            )
            .await;
            ctx.checkpoint.done_pages = done_pages;
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

            let regions = raw_regions
                .ok_or_else(|| OrchestratorError::Sidecar("layout produced no result".into()))?;

            for region in &regions {
                let crop_hash = match region.crop_png_path.as_deref() {
                    Some(path) => {
                        let path = std::path::Path::new(path);
                        let hash = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or_else(|| {
                                OrchestratorError::Sidecar(format!(
                                    "invalid crop path '{}'",
                                    path.display()
                                ))
                            })?
                            .to_string();
                        let bytes = tokio::fs::metadata(path)
                            .await
                            .map(|m| m.len() as i64)
                            .unwrap_or(0);
                        cache::record_use(&ctx.pool, &hash, "crop_png", bytes, Some(&ctx.job_id))
                            .await?;
                        Some(hash)
                    }
                    None => None,
                };
                let figure = book::FigureRegion {
                    id: region.id.clone(),
                    book_id: record.book_id.clone(),
                    page_no: region.page,
                    bbox_json: serde_json::to_string(&region.bbox)?,
                    kind: region.kind.clone(),
                    confidence: region.confidence,
                    crop_hash,
                    caption: None,
                    explanation: None,
                    caption_confidence: None,
                };
                book::insert_figure_region_ignore_conflict(&ctx.pool, &figure).await?;
            }

            merge_report(
                ctx,
                &record,
                &flags,
                regions.len(),
                started.elapsed().as_secs(),
            )
            .await?;
            ctx.emit_progress(1.0, Some(0), &format!("{} regions", regions.len()));
            Ok(StageOutcome::Completed)
        })
    }
}
