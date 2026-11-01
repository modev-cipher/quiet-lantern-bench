use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::watch;

use super::state::{Checkpoint, JobState, OrchestratorError};
use crate::storage::{job, now_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Run,
    Pause,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct ControlHandle {
    sender: Arc<watch::Sender<Control>>,
}

#[derive(Debug, Clone)]
pub struct ControlReceiver {
    receiver: watch::Receiver<Control>,
}

pub fn control_channel() -> (ControlHandle, ControlReceiver) {
    let (sender, receiver) = watch::channel(Control::Run);
    (
        ControlHandle {
            sender: Arc::new(sender),
        },
        ControlReceiver { receiver },
    )
}

impl ControlHandle {
    pub fn pause(&self) {
        let _ = self.sender.send(Control::Pause);
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(Control::Cancel);
    }

    pub fn current(&self) -> Control {
        *self.sender.borrow()
    }
}

impl ControlReceiver {
    pub fn current(&self) -> Control {
        *self.receiver.borrow()
    }

    pub fn interruption(&self) -> Option<StageOutcome> {
        match self.current() {
            Control::Run => None,
            Control::Pause => Some(StageOutcome::Paused),
            Control::Cancel => Some(StageOutcome::Cancelled),
        }
    }

    pub async fn interrupted(&mut self) -> StageOutcome {
        loop {
            if let Some(outcome) = self.interruption() {
                return outcome;
            }
            if self.receiver.changed().await.is_err() {
                return StageOutcome::Cancelled;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    Completed,
    Paused,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProgressEvent {
    pub job_id: String,
    pub stage: JobState,
    pub pct: f64,
    pub eta_seconds: Option<i64>,
    pub current_item: String,
}

pub type ProgressSink = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

pub struct StageContext {
    pub pool: SqlitePool,
    pub job_id: String,
    pub stage: JobState,
    pub checkpoint: Checkpoint,
    pub control: ControlReceiver,
    progress: ProgressSink,
}

impl StageContext {
    pub fn new(
        pool: SqlitePool,
        job_id: String,
        stage: JobState,
        checkpoint: Checkpoint,
        control: ControlReceiver,
        progress: ProgressSink,
    ) -> Self {
        StageContext {
            pool,
            job_id,
            stage,
            checkpoint,
            control,
            progress,
        }
    }

    pub async fn save_checkpoint(&mut self) -> Result<(), OrchestratorError> {
        self.checkpoint.stage = self.stage;
        self.checkpoint.updated_at = now_ms();
        let json = serde_json::to_string(&self.checkpoint)?;
        job::update_job_checkpoint(&self.pool, &self.job_id, &json).await?;
        tracing::debug!(
            job = %self.job_id,
            "checkpoint saved: stage={} done_pages={} done_figures={} done_chunks={} \
             done_segments={}/{} rev={}",
            self.stage.as_str(),
            self.checkpoint.done_pages,
            self.checkpoint.done_figures,
            self.checkpoint.done_chunks,
            self.checkpoint.done_segments,
            self.checkpoint.total_segments,
            self.checkpoint.resource_plan_rev
        );
        Ok(())
    }

    pub async fn merge_report_duration(
        &self,
        stage_key: &str,
        seconds: u64,
    ) -> Result<(), OrchestratorError> {
        let Some(record) = job::get_job(&self.pool, &self.job_id).await? else {
            return Ok(());
        };
        let mut report: serde_json::Value = record
            .report_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        if !report["durations_s"].is_object() {
            report["durations_s"] = serde_json::json!({});
        }
        report["durations_s"][stage_key] = serde_json::json!(seconds);
        job::set_job_report_only(&self.pool, &self.job_id, &serde_json::to_string(&report)?)
            .await?;
        Ok(())
    }

    pub async fn merge_report_count(&self, key: &str, value: u64) -> Result<(), OrchestratorError> {
        if value == 0 {
            return Ok(());
        }
        let Some(record) = job::get_job(&self.pool, &self.job_id).await? else {
            return Ok(());
        };
        let mut report: serde_json::Value = record
            .report_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        report[key] = serde_json::json!(value);
        job::set_job_report_only(&self.pool, &self.job_id, &serde_json::to_string(&report)?)
            .await?;
        Ok(())
    }

    pub async fn merge_report_flags(
        &self,
        key: &str,
        flags: &[String],
    ) -> Result<(), OrchestratorError> {
        if flags.is_empty() {
            return Ok(());
        }
        let Some(record) = job::get_job(&self.pool, &self.job_id).await? else {
            return Ok(());
        };
        let mut report: serde_json::Value = record
            .report_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        report[key] = serde_json::json!(flags);
        job::set_job_report_only(&self.pool, &self.job_id, &serde_json::to_string(&report)?)
            .await?;
        Ok(())
    }

    pub async fn merge_report_value(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), OrchestratorError> {
        let Some(record) = job::get_job(&self.pool, &self.job_id).await? else {
            return Ok(());
        };
        let mut report: serde_json::Value = record
            .report_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        report[key] = value;
        job::set_job_report_only(&self.pool, &self.job_id, &serde_json::to_string(&report)?)
            .await?;
        Ok(())
    }

    pub fn emit_progress(&self, pct: f64, eta_seconds: Option<i64>, current_item: &str) {
        (self.progress)(ProgressEvent {
            job_id: self.job_id.clone(),
            stage: self.stage,
            pct,
            eta_seconds,
            current_item: current_item.to_string(),
        });
    }
}

pub trait Stage: Send + Sync {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>>;
}

pub struct PassthroughStage;

impl Stage for PassthroughStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            ctx.emit_progress(1.0, Some(0), "skipped");
            Ok(StageOutcome::Completed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_channel_signals_pause_and_cancel() {
        let (handle, receiver) = control_channel();
        assert_eq!(receiver.current(), Control::Run);
        assert!(receiver.interruption().is_none());
        handle.pause();
        assert_eq!(receiver.interruption(), Some(StageOutcome::Paused));
        handle.cancel();
        assert_eq!(receiver.interruption(), Some(StageOutcome::Cancelled));
        assert_eq!(handle.current(), Control::Cancel);
    }

    #[tokio::test]
    async fn interrupted_wakes_on_signal() {
        let (handle, mut receiver) = control_channel();
        let waiter = tokio::spawn(async move { receiver.interrupted().await });
        handle.pause();
        assert_eq!(waiter.await.unwrap(), StageOutcome::Paused);
    }

    #[tokio::test]
    async fn interrupted_resolves_cancelled_when_handle_dropped() {
        let (handle, mut receiver) = control_channel();
        drop(handle);
        assert_eq!(receiver.interrupted().await, StageOutcome::Cancelled);
    }
}
