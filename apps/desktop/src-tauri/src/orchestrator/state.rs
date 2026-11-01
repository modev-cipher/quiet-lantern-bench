use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobState {
    Created,
    Queued,
    Ingesting,
    Layout,
    Vision,
    Scripting,
    Synthesizing,
    Aligning,
    Rendering,
    Done,
    Paused,
    Cancelled,
    Failed,
}

pub const ALL_STATES: [JobState; 13] = [
    JobState::Created,
    JobState::Queued,
    JobState::Ingesting,
    JobState::Layout,
    JobState::Vision,
    JobState::Scripting,
    JobState::Synthesizing,
    JobState::Aligning,
    JobState::Rendering,
    JobState::Done,
    JobState::Paused,
    JobState::Cancelled,
    JobState::Failed,
];

pub const STAGE_ORDER: [JobState; 7] = [
    JobState::Ingesting,
    JobState::Layout,
    JobState::Vision,
    JobState::Scripting,
    JobState::Synthesizing,
    JobState::Aligning,
    JobState::Rendering,
];

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Created => "CREATED",
            JobState::Queued => "QUEUED",
            JobState::Ingesting => "INGESTING",
            JobState::Layout => "LAYOUT",
            JobState::Vision => "VISION",
            JobState::Scripting => "SCRIPTING",
            JobState::Synthesizing => "SYNTHESIZING",
            JobState::Aligning => "ALIGNING",
            JobState::Rendering => "RENDERING",
            JobState::Done => "DONE",
            JobState::Paused => "PAUSED",
            JobState::Cancelled => "CANCELLED",
            JobState::Failed => "FAILED",
        }
    }

    pub fn start_label(&self) -> &'static str {
        match self {
            JobState::Ingesting => "reading the book",
            JobState::Layout => "starting layout",
            JobState::Vision => "preparing figures",
            JobState::Scripting => "preparing the script",
            JobState::Synthesizing => "preparing voices",
            JobState::Aligning => "loading the alignment model",
            JobState::Rendering => "starting render",
            _ => "starting",
        }
    }

    pub fn parse(value: &str) -> Result<Self, OrchestratorError> {
        match value {
            "CREATED" => Ok(JobState::Created),
            "QUEUED" => Ok(JobState::Queued),
            "INGESTING" => Ok(JobState::Ingesting),
            "LAYOUT" => Ok(JobState::Layout),
            "VISION" => Ok(JobState::Vision),
            "SCRIPTING" => Ok(JobState::Scripting),
            "SYNTHESIZING" => Ok(JobState::Synthesizing),
            "ALIGNING" => Ok(JobState::Aligning),
            "RENDERING" => Ok(JobState::Rendering),
            "DONE" => Ok(JobState::Done),
            "PAUSED" => Ok(JobState::Paused),
            "CANCELLED" => Ok(JobState::Cancelled),
            "FAILED" => Ok(JobState::Failed),
            other => Err(OrchestratorError::UnknownState(other.to_string())),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Done | JobState::Cancelled | JobState::Failed
        )
    }

    pub fn is_stage(&self) -> bool {
        STAGE_ORDER.contains(self)
    }

    pub fn next_stage(&self) -> Option<JobState> {
        match self {
            JobState::Queued => Some(STAGE_ORDER[0]),
            _ => {
                let idx = STAGE_ORDER.iter().position(|s| s == self)?;
                Some(STAGE_ORDER.get(idx + 1).copied().unwrap_or(JobState::Done))
            }
        }
    }
}

pub fn terminal_states_csv() -> String {
    let mut csv = String::from(",");
    for state in ALL_STATES.iter().filter(|state| state.is_terminal()) {
        csv.push_str(state.as_str());
        csv.push(',');
    }
    csv
}

pub fn can_transition(from: JobState, to: JobState) -> bool {
    // The ONE exception to terminality (charter pre-brief item 9, Hermes R2
    // fold-in 7): a FAILED job may be explicitly re-queued by user recovery
    // — bindings and checkpoints preserved — and by nothing else.
    if from == JobState::Failed && to == JobState::Queued {
        return true;
    }
    if from.is_terminal() {
        return false;
    }
    match to {
        JobState::Created => false,
        JobState::Paused | JobState::Cancelled | JobState::Failed => from != JobState::Created,
        JobState::Queued => matches!(from, JobState::Created | JobState::Paused),
        JobState::Done => matches!(from, JobState::Rendering | JobState::Paused),
        stage if stage.is_stage() => match from {
            JobState::Paused => true,
            _ => from.next_stage() == Some(stage),
        },
        _ => false,
    }
}

#[derive(Debug)]
pub enum OrchestratorError {
    UnknownState(String),
    IllegalTransition {
        from: JobState,
        to: JobState,
    },
    JobNotFound(String),
    BookNotFound(String),
    VoiceProfileNotFound(String),
    InvalidRequest(String),
    Sidecar(String),
    Engine {
        status: Option<u16>,
        code: String,
        message: String,
    },
    SidecarUnavailable,
    EngineBusy {
        stage: JobState,
        retries: u32,
        waited_s: u64,
        cause: String,
    },
    ModelNotInstalled(String),
    FfmpegNotFound,
    Render(String),
    MissingAudioArtifact {
        stage: JobState,
        count: usize,
        message: String,
    },
    ArtifactRecoveryExhausted {
        stage: JobState,
        attempts: u32,
    },
    Storage(sqlx::Error),
    Serde(serde_json::Error),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrchestratorError::UnknownState(s) => write!(f, "unknown job state '{s}'"),
            OrchestratorError::IllegalTransition { from, to } => {
                write!(f, "illegal transition {} -> {}", from.as_str(), to.as_str())
            }
            OrchestratorError::JobNotFound(id) => write!(f, "job '{id}' not found"),
            OrchestratorError::BookNotFound(id) => write!(f, "book '{id}' not found"),
            OrchestratorError::VoiceProfileNotFound(id) => {
                write!(f, "voice profile '{id}' not found")
            }
            OrchestratorError::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
            OrchestratorError::Sidecar(msg) => write!(f, "sidecar error: {msg}"),
            OrchestratorError::Engine {
                status: Some(status),
                code,
                message,
            } => write!(f, "engine refused with {status} ({code}): {message}"),
            OrchestratorError::Engine {
                status: None,
                code,
                message,
            } => write!(f, "engine reported {code}: {message}"),
            OrchestratorError::SidecarUnavailable => write!(f, "sidecar unavailable"),
            OrchestratorError::EngineBusy {
                stage,
                retries,
                waited_s,
                cause,
            } => write!(
                f,
                "engine stayed busy at {}: gave up after {retries} retries over {waited_s}s; last error: {cause}",
                stage.as_str()
            ),
            OrchestratorError::ModelNotInstalled(id) => {
                write!(f, "model '{id}' is not installed")
            }
            OrchestratorError::FfmpegNotFound => {
                write!(
                    f,
                    "ffmpeg could not be started: no bundled copy and none on PATH \
                     (set TXTCASTER_FFMPEG to override)"
                )
            }
            OrchestratorError::Render(msg) => write!(f, "render error: {msg}"),
            OrchestratorError::MissingAudioArtifact {
                stage,
                count,
                message,
            } => write!(
                f,
                "missing {count} audio artifact(s) at {}: {message}",
                stage.as_str()
            ),
            OrchestratorError::ArtifactRecoveryExhausted { stage, attempts } => write!(
                f,
                "artifact recovery exhausted at {}: failed to restore audio artifacts after {attempts} attempts",
                stage.as_str()
            ),
            OrchestratorError::Storage(e) => write!(f, "storage error: {e}"),
            OrchestratorError::Serde(e) => write!(f, "serialization error: {e}"),
        }
    }
}

impl std::error::Error for OrchestratorError {}

pub const MISSING_EVENT_CODE: &str = "ERR_UNCODED";

impl OrchestratorError {
    pub fn from_engine_event(data: &serde_json::Value, fallback_message: &str) -> Self {
        let code = data
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(MISSING_EVENT_CODE);
        let message = data
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(fallback_message);
        OrchestratorError::Engine {
            status: None,
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    pub fn engine_code(&self) -> Option<&str> {
        match self {
            OrchestratorError::Engine { code, .. } => Some(code.as_str()),
            _ => None,
        }
    }
}

impl From<crate::sidecar::client::SidecarError> for OrchestratorError {
    fn from(error: crate::sidecar::client::SidecarError) -> Self {
        match error {
            crate::sidecar::client::SidecarError::Transport(message) => {
                OrchestratorError::Sidecar(message)
            }
            crate::sidecar::client::SidecarError::Refused {
                status,
                path,
                code,
                message,
            } => OrchestratorError::Engine {
                status: Some(status),
                code,
                message: format!("{path}: {message}"),
            },
        }
    }
}

impl From<sqlx::Error> for OrchestratorError {
    fn from(e: sqlx::Error) -> Self {
        OrchestratorError::Storage(e)
    }
}

impl From<serde_json::Error> for OrchestratorError {
    fn from(e: serde_json::Error) -> Self {
        OrchestratorError::Serde(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitClass {
    PerMinute,
    Daily,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PausedReason {
    pub kind: String,
    pub class: RateLimitClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_epoch_ms: Option<i64>,
    pub provider: String,
    pub segment_idx: i64,
    pub last_error_message: String,
    #[serde(default)]
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryEntry {
    pub stage: JobState,
    pub rev: i64,
    pub action: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub stage: JobState,
    #[serde(default)]
    pub done_pages: i64,
    #[serde(default)]
    pub done_figures: i64,
    #[serde(default)]
    pub done_chunks: i64,
    #[serde(default)]
    pub done_segments: i64,
    #[serde(default)]
    pub total_segments: i64,
    #[serde(default)]
    pub resource_plan_rev: i64,
    #[serde(default)]
    pub recovery_failures: i64,
    #[serde(default)]
    pub contention_pauses: i64,
    #[serde(default)]
    pub rate_limit_pauses: u32,
    #[serde(default)]
    pub last_pause_done: i64,
    #[serde(default)]
    pub recovery_log: Vec<RecoveryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_reason: Option<PausedReason>,
    pub updated_at: i64,
}

impl Checkpoint {
    pub fn new(stage: JobState, updated_at: i64) -> Self {
        Checkpoint {
            stage,
            done_pages: 0,
            done_figures: 0,
            done_chunks: 0,
            done_segments: 0,
            total_segments: 0,
            resource_plan_rev: 0,
            recovery_failures: 0,
            contention_pauses: 0,
            rate_limit_pauses: 0,
            last_pause_done: 0,
            recovery_log: Vec::new(),
            paused_reason: None,
            updated_at,
        }
    }

    pub fn reset_throttle_progress(&mut self) {
        self.rate_limit_pauses = 0;
        self.last_pause_done = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Chapters { chapter_ids: Vec<String> },
    Pages { page_start: i64, page_end: i64 },
}

impl Scope {
    pub fn validate(&self) -> Result<(), OrchestratorError> {
        match self {
            Scope::Chapters { chapter_ids } => {
                if chapter_ids.is_empty() {
                    return Err(OrchestratorError::InvalidRequest(
                        "scope must include at least one chapter".into(),
                    ));
                }
                Ok(())
            }
            Scope::Pages {
                page_start,
                page_end,
            } => {
                if *page_start < 1 || page_end < page_start {
                    return Err(OrchestratorError::InvalidRequest(format!(
                        "invalid page range {page_start}..{page_end}"
                    )));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_pipeline_stage_has_its_own_start_label() {
        let labels: Vec<&str> = STAGE_ORDER.iter().map(|s| s.start_label()).collect();
        assert_eq!(labels.len(), STAGE_ORDER.len());
        for label in &labels {
            assert_ne!(
                *label, "starting",
                "each pipeline stage needs a specific label"
            );
            assert!(!label.is_empty());
        }
        let unique: std::collections::HashSet<&&str> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "labels must be distinguishable");
    }

    #[test]
    fn stages_that_can_run_in_the_cloud_do_not_claim_a_local_model_load() {
        for stage in [
            JobState::Vision,
            JobState::Scripting,
            JobState::Synthesizing,
        ] {
            assert!(
                !stage.start_label().contains("model"),
                "{stage:?} picks local or cloud after it starts"
            );
        }
    }

    #[test]
    fn non_pipeline_states_fall_back_to_a_generic_label() {
        assert_eq!(JobState::Queued.start_label(), "starting");
        assert_eq!(JobState::Done.start_label(), "starting");
    }

    use super::*;

    #[test]
    fn state_string_round_trip() {
        for state in ALL_STATES {
            assert_eq!(JobState::parse(state.as_str()).unwrap(), state);
        }
        assert!(JobState::parse("BOGUS").is_err());
    }

    #[test]
    fn all_states_is_exhaustive_and_unique() {
        let mut names: Vec<&str> = ALL_STATES.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 13);
        for stage in STAGE_ORDER {
            assert!(ALL_STATES.contains(&stage));
        }
    }

    #[test]
    fn pipeline_walks_in_order() {
        assert_eq!(JobState::Queued.next_stage(), Some(JobState::Ingesting));
        let mut state = JobState::Ingesting;
        let mut walked = vec![state];
        while let Some(next) = state.next_stage() {
            assert!(can_transition(state, next), "{state:?} -> {next:?}");
            walked.push(next);
            if next == JobState::Done {
                break;
            }
            state = next;
        }
        assert_eq!(walked.len(), 8);
        assert_eq!(*walked.last().unwrap(), JobState::Done);
    }

    #[test]
    fn stage_skipping_is_illegal() {
        assert!(!can_transition(JobState::Ingesting, JobState::Vision));
        assert!(!can_transition(JobState::Scripting, JobState::Rendering));
        assert!(!can_transition(JobState::Queued, JobState::Done));
        assert!(!can_transition(JobState::Ingesting, JobState::Created));
    }

    #[test]
    fn any_active_state_can_pause_cancel_fail() {
        for from in [
            JobState::Queued,
            JobState::Ingesting,
            JobState::Synthesizing,
            JobState::Rendering,
        ] {
            assert!(can_transition(from, JobState::Paused));
            assert!(can_transition(from, JobState::Cancelled));
            assert!(can_transition(from, JobState::Failed));
        }
        assert!(!can_transition(JobState::Created, JobState::Paused));
    }

    #[test]
    fn terminal_states_are_final_except_explicit_failure_recovery() {
        for from in [JobState::Done, JobState::Cancelled, JobState::Failed] {
            for to in [
                JobState::Queued,
                JobState::Ingesting,
                JobState::Paused,
                JobState::Cancelled,
                JobState::Failed,
            ] {
                if from == JobState::Failed && to == JobState::Queued {
                    // The ONE user-recovery exception (charter pre-brief
                    // item 9): a failed job may be explicitly re-queued.
                    assert!(can_transition(from, to));
                    continue;
                }
                assert!(!can_transition(from, to), "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn paused_resumes_to_any_stage_or_queue() {
        assert!(can_transition(JobState::Paused, JobState::Queued));
        assert!(can_transition(JobState::Paused, JobState::Synthesizing));
        assert!(can_transition(JobState::Paused, JobState::Done));
        assert!(can_transition(JobState::Paused, JobState::Cancelled));
    }

    #[test]
    fn checkpoint_json_round_trip() {
        let checkpoint = Checkpoint {
            stage: JobState::Synthesizing,
            done_pages: 62,
            done_figures: 14,
            done_chunks: 9,
            done_segments: 214,
            total_segments: 890,
            resource_plan_rev: 2,
            recovery_failures: 1,
            contention_pauses: 0,
            rate_limit_pauses: 3,
            last_pause_done: 9,
            recovery_log: vec![RecoveryEntry {
                stage: JobState::Synthesizing,
                rev: 1,
                action: "tts batch/workers halved".into(),
                at: 1_783_468_700_000,
            }],
            paused_reason: None,
            updated_at: 1_783_468_800_000,
        };
        let json = serde_json::to_string(&checkpoint).unwrap();
        assert!(json.contains("\"SYNTHESIZING\""));
        let parsed: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, checkpoint);
    }

    #[test]
    fn checkpoint_with_paused_reason_round_trip() {
        let checkpoint = Checkpoint {
            stage: JobState::Synthesizing,
            done_pages: 10,
            done_figures: 0,
            done_chunks: 2,
            done_segments: 5,
            total_segments: 20,
            resource_plan_rev: 0,
            recovery_failures: 0,
            contention_pauses: 0,
            rate_limit_pauses: 0,
            last_pause_done: 0,
            recovery_log: Vec::new(),
            paused_reason: Some(PausedReason {
                kind: "rate_limited".into(),
                class: RateLimitClass::PerMinute,
                retry_at_epoch_ms: Some(1_783_468_900_000),
                provider: "gemini".into(),
                segment_idx: 5,
                last_error_message: "Resource has been exhausted (e.g. check quota)".into(),
                attempt: 1,
            }),
            updated_at: 1_783_468_800_000,
        };
        let json = serde_json::to_string(&checkpoint).unwrap();
        assert!(json.contains("\"per_minute\""));
        let parsed: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, checkpoint);
    }

    #[test]
    fn checkpoint_missing_counters_default_to_zero() {
        let parsed: Checkpoint =
            serde_json::from_str(r#"{"stage":"LAYOUT","updated_at":5}"#).unwrap();
        assert_eq!(parsed, Checkpoint::new(JobState::Layout, 5));
    }

    #[test]
    fn checkpoint_without_throttle_counters_defaults_to_zero() {
        let parsed: Checkpoint = serde_json::from_str(
            r#"{"stage":"SCRIPTING","done_chunks":4,"paused_reason":{"kind":"rate_limited","class":"per_minute","provider":"gemini","segment_idx":4,"last_error_message":"quota exceeded","attempt":3},"updated_at":9}"#,
        )
        .unwrap();
        assert_eq!(parsed.rate_limit_pauses, 0);
        assert_eq!(parsed.last_pause_done, 0);
        assert_eq!(parsed.done_chunks, 4);
        assert_eq!(parsed.paused_reason.expect("paused reason").attempt, 3);
    }

    #[test]
    fn checkpoint_throttle_counters_round_trip() {
        let mut checkpoint = Checkpoint::new(JobState::Scripting, 7);
        checkpoint.rate_limit_pauses = 3;
        checkpoint.last_pause_done = 41;
        let json = serde_json::to_string(&checkpoint).unwrap();
        assert!(json.contains("\"rate_limit_pauses\":3"));
        assert!(json.contains("\"last_pause_done\":41"));
        let parsed: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, checkpoint);
    }

    #[test]
    fn reset_throttle_progress_zeroes_both_counters() {
        let mut checkpoint = Checkpoint::new(JobState::Scripting, 7);
        checkpoint.rate_limit_pauses = 5;
        checkpoint.last_pause_done = 41;
        checkpoint.reset_throttle_progress();
        assert_eq!(checkpoint.rate_limit_pauses, 0);
        assert_eq!(checkpoint.last_pause_done, 0);
        checkpoint.reset_throttle_progress();
        assert_eq!(checkpoint.rate_limit_pauses, 0);
        assert_eq!(checkpoint.last_pause_done, 0);
    }

    #[test]
    fn scope_json_matches_spec() {
        let chapters: Scope =
            serde_json::from_str(r#"{"kind":"chapters","chapter_ids":["c1","c2"]}"#).unwrap();
        assert_eq!(
            chapters,
            Scope::Chapters {
                chapter_ids: vec!["c1".into(), "c2".into()]
            }
        );
        let pages: Scope =
            serde_json::from_str(r#"{"kind":"pages","page_start":120,"page_end":184}"#).unwrap();
        assert_eq!(
            pages,
            Scope::Pages {
                page_start: 120,
                page_end: 184
            }
        );
        assert!(chapters.validate().is_ok());
        assert!(pages.validate().is_ok());
    }

    #[test]
    fn scope_validation_rejects_bad_input() {
        assert!(Scope::Chapters {
            chapter_ids: vec![]
        }
        .validate()
        .is_err());
        assert!(Scope::Pages {
            page_start: 0,
            page_end: 4
        }
        .validate()
        .is_err());
        assert!(Scope::Pages {
            page_start: 10,
            page_end: 9
        }
        .validate()
        .is_err());
    }
}

#[cfg(test)]
mod terminal_state_tests {
    use super::*;

    #[test]
    fn terminal_states_match_the_state_machine() {
        let from_enum: Vec<&str> = ALL_STATES
            .iter()
            .filter(|state| state.is_terminal())
            .map(|state| state.as_str())
            .collect();
        assert_eq!(
            from_enum,
            crate::storage::book::TERMINAL_JOB_STATES.to_vec(),
            "the extraction claim excludes jobs by NOT IN (terminals); if a state is added \
             or reclassified, that SQL must be updated in the same change or a live job \
             stops blocking a re-extraction"
        );

        let rendered = format!(
            "({})",
            crate::storage::book::TERMINAL_JOB_STATES
                .iter()
                .map(|state| format!("'{state}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert_eq!(
            rendered,
            crate::storage::book::TERMINAL_STATES_SQL,
            "both admission statements embed this literal; editing the enum without the SQL \
             must fail here rather than in production"
        );
        let book_sql = include_str!("../storage/book.rs");
        let embedded = book_sql
            .lines()
            .filter(|line| {
                line.contains("state NOT IN")
                    && line.contains(crate::storage::book::TERMINAL_STATES_SQL)
            })
            .count();
        assert_eq!(
            embedded, 3,
            "the two admission statements and the delete guard in book.rs embed the literal; \
             a new one must be counted here so it cannot drift from the enum silently"
        );
        let export_sql = include_str!("../storage/export.rs");
        let live_encode = export_sql
            .lines()
            .filter(|line| {
                line.contains("state NOT IN")
                    && line.contains(crate::storage::book::TERMINAL_STATES_SQL)
            })
            .count();
        assert_eq!(
            live_encode, 1,
            "live_encode_exists in export.rs embeds the literal too; it decides whether a \
             delete may take a .partial a running encode owns"
        );

        let job_sql = include_str!("../storage/job.rs");
        assert!(
            job_sql.contains("book.extracting_since IS NOT NULL"),
            "insert_job_unless_extracting is the other half of the exclusion and lives \
             outside book.rs, so the guard has to reach it too"
        );
        assert!(
            job_sql.contains("book.extracting_since >= ?"),
            "and it must honour the staleness bound; without it a crashed pass blocks job \
             creation for that book permanently"
        );
    }
}
