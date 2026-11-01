pub mod align;
pub mod contention;
pub mod layout;
pub mod llm;
pub mod overload;
pub mod provider_error_policy;
pub mod render;
pub mod residency_check;
pub mod scripting;
pub mod scripting_cloud;
pub mod sleep_inhibit;
pub mod stage;
pub mod state;
pub mod stream;
pub mod synthesis;
pub mod synthesis_cloud;
pub mod verify;
pub mod vision;
pub mod vision_model;
pub mod worker;

pub use align::AligningStage;
pub use contention::{
    classify_failure, record_contention, BusyRetryPolicy, ContentionDelta, StageFailure,
    BUSY_NOTICE, BUSY_PAUSE_NOTICE, CONTENTION_KEY, MAX_CONTENTION_PAUSES,
};
pub use layout::LayoutStage;
pub use overload::{
    classify_provider_error, friendly_provider_error, friendly_provider_name, is_overload,
    is_provider_failure_overload, is_script_error_overload, PatienceConfig, ProviderErrorClass,
    PAUSED_KIND_RATE_LIMITED, PAUSED_KIND_SERVICE_BUSY,
};
pub use provider_error_policy::{FaultClass, PatienceBudget, ProviderErrorPolicy};
pub use render::RenderingStage;
pub use scripting::{engine_capability, engine_capability_known, ScriptingStage, SidecarProvider};
pub use stage::{
    control_channel, Control, ControlHandle, ControlReceiver, ProgressEvent, ProgressSink, Stage,
    StageContext, StageOutcome,
};
pub use state::{
    can_transition, terminal_states_csv, Checkpoint, JobState, OrchestratorError, PausedReason,
    RateLimitClass, RecoveryEntry, Scope, ALL_STATES, STAGE_ORDER,
};
pub use synthesis::SynthesizingStage;
pub use vision::VisionStage;
pub use worker::{
    seed_default_voice_profile, CreateJobRequest, JobOrchestrator, JobUpdate, SegmentEdit,
    StageRegistry, StartOptions, UpdateSink, DEFAULT_GRACE, DEFAULT_VOICE_PROFILE_ID,
    RECOVERY_NOTICE,
};
