//! BYOK key management (VA3 — VOICE_AGENT §7, ARCHITECTURE §8.1).
//!
//! Keys live per base URL in the OS keychain, with the ranked Linux fallback
//! chain in [`store`] (passphrase-encrypted file, then session-only) and the
//! active level surfaced to the UI. [`secret::ApiKey`] is the only type
//! allowed to hold key bytes: zeroized on drop, redacting `Debug`/`Display`,
//! deliberately unserializable. [`policy`] admits base URLs (HTTPS-only,
//! localhost exempt). [`hygiene`] holds the grep-style tests that keep the
//! persistence layer structurally key-blind.

pub mod import;
pub mod manager;
pub mod policy;
pub mod probe;
pub mod secret;
pub mod store;
pub mod vault;

mod hygiene;

pub use manager::{
    ImportReport, KeyEntryView, KeyError, KeyManager, PoolQuotaAssessment, PoolTestStatus,
    ReverseAnchorConsent, UrlCheck,
};
pub use policy::{canonical_base_url, UrlPolicyError};
pub use probe::{Capability, KeyState, Provider};
pub use secret::ApiKey;
pub use store::{KeyStore, KeyStoreError, StorageLevel, StorageStatus};
