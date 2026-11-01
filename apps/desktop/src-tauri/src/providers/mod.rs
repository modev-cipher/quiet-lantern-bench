//! Phase-scoped provider registry for batch casts (provider-flexibility
//! charter §3, S-0). Generalizes the Voice Tutor selection pattern: a
//! registry chooses one object-safe implementation at the binding point,
//! orchestration degrades from capability/outcome facts (never provider
//! identity), keys resolve only inside Rust adapters through the shared VA3
//! custody (`crate::voice::keys`), and wire quirks stay inside adapter
//! modules. Batch traits are deliberately narrower than `SessionChannel` —
//! no streaming, tools, turns, or live-session state (charter §3.3).
//!
//! S-0 ships types, the connection store, the binding schema, and local
//! markers; nothing here is wired into the cast pipeline yet, so a tree with
//! zero configured connections behaves byte-for-byte as before (charter §6.5).

pub mod adapters;
pub mod binding;
pub mod connection;
pub mod directory;
pub mod discovery;
mod hygiene;
pub mod overview;
pub mod pool_health;
pub mod registry;
pub mod script;
pub mod setup;

pub mod cast_plan;
pub mod consent;
pub mod cost;
pub mod model_fetch;
pub mod model_pick;
pub mod tts_words;

pub use binding::{
    cloud_profile_connection_id, tts_binding_from_cloud_profile, validate_cloud_profile,
    PhaseBinding, TtsBinding, ValidCloudVoice, CLOUD_VOICE_ENGINE,
};
pub use connection::{ConnectionRecord, ConnectionStore, ConnectionView, DeleteImpact};
pub use registry::{PhaseCapability, ProviderKind};
