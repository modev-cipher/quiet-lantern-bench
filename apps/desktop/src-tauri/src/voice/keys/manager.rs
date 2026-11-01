//! The key-management seam the UI commands talk to (VA3). Owns the trigger
//! discipline of VOICE_AGENT §7: probes run only inside [`KeyManager::probe_all`]
//! (settings entry) and [`KeyManager::refresh_one`] (explicit refresh) — there
//! is no timer, interval, or background task in this module, and `hygiene`
//! greps keep it that way. Probe states are memory-only: they live in this
//! struct and die with the process, never in SQLite.
//!
//! SQLite holds only non-secret metadata — which base URLs are configured, by
//! which provider, and per-URL consent — through the `app_setting` helpers.
//! Key bytes stay behind [`KeyStore`]; this file never calls `expose`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use zeroize::Zeroizing;

use crate::storage::{now_ms, settings};

use super::policy::canonical_base_url;
use super::probe::{classify, probe_request, run_probe, KeyState, Provider};
use super::secret::ApiKey;
use super::store::{KeyStore, KeyStoreError, StorageStatus};

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorKeyReadiness {
    Present,
    Missing,
    Locked,
    Unavailable,
}

const KEY_META_PREFIX: &str = "voice_key.";
/// Slot-qualified store accounts (RCA §9): slot 0 is the bare base URL —
/// exactly what every pre-slot install already has in its keyring/vault — and
/// slot N ≥ 1 is `<base_url>#<N>`. URL policy rejects fragments outright, so
/// `#` can never occur inside a canonical base URL and the split-back is
/// unambiguous.
const SLOT_SEPARATOR: char = '#';

fn slot_account(base_url: &str, slot: u32) -> String {
    if slot == 0 {
        base_url.to_owned()
    } else {
        format!("{base_url}{SLOT_SEPARATOR}{slot}")
    }
}

/// The inverse of [`slot_account`]. A suffix that does not parse as a
/// positive slot number is treated as part of the account itself rather than
/// invented into a slot — canonical URLs cannot carry `#`, so this only
/// happens for rows written outside the app, and refusing to guess keeps
/// them visible as-is instead of silently aliased onto slot 0.
fn split_slot_account(account: &str) -> (&str, u32) {
    match account.rsplit_once(SLOT_SEPARATOR) {
        Some((base_url, digits)) if !base_url.is_empty() => match digits.parse::<u32>() {
            Ok(slot) if slot > 0 => (base_url, slot),
            _ => (account, 0),
        },
        _ => (account, 0),
    }
}
// v2 (F7, 2026-08-30): the consent scope grew — voice sessions may now send
// page images on request, and the copy names it. Grants recorded under v1
// predate that copy, so the key generation bumps and every endpoint
// re-prompts once with the honest scope; v1 rows stay behind, inert.
const CONSENT_PREFIX: &str = "voice_url_consent2.";
/// Per-first-use-per-book consent for the reverse anchor (VOICE_AGENT §11.1,
/// bound by the §5 floor: it sends reading-position information to the
/// provider). Key shape: `voice_reverse_anchor_consent.<book_id>.<canonical>`
/// — book ids are UUIDs (never contain `.`), so the first `.` after the
/// prefix splits unambiguously. Same store, same canonicalization, and the
/// same asked-once discipline as the per-URL consent above; the extra
/// dimension is the book.
const REVERSE_ANCHOR_CONSENT_PREFIX: &str = "voice_reverse_anchor_consent.";

/// Book ids are UUIDs — never empty, never containing `.` — and the key
/// shape depends on both facts: an empty id would make
/// [`reverse_anchor_consent_prefix_for_book`] alias the whole consent
/// prefix (one book's cascade would drop every book's rows), and a dotted
/// id would split at the wrong `.` on the way back out of the store.
/// Anything else coming over the command boundary is refused outright.
fn validated_consent_book_id(book_id: &str) -> Result<(), KeyError> {
    if book_id.is_empty() || book_id.contains('.') {
        return Err(KeyError::InvalidBookId);
    }
    Ok(())
}

fn reverse_anchor_consent_key(book_id: &str, canonical: &str) -> String {
    format!("{REVERSE_ANCHOR_CONSENT_PREFIX}{book_id}.{canonical}")
}

/// The setting-key prefix covering every reverse-anchor consent of one book —
/// the cascade-delete handle (`delete_book` drops the rows through it, so a
/// deleted book's consents never outlive it).
/// app_setting key holding the operator-disabled slot accounts (JSON
/// array). Persisted so an explicit disable survives restart (A28).
const DISABLED_SLOTS_KEY: &str = "voice_key_disabled_slots_v1";

pub fn reverse_anchor_consent_prefix_for_book(book_id: &str) -> String {
    format!("{REVERSE_ANCHOR_CONSENT_PREFIX}{book_id}.")
}

#[derive(Debug)]
pub enum KeyError {
    UnknownProvider,
    EmptyKey,
    OperationConflict,
    BaseUrlFixed,
    BaseUrlRequired,
    Policy(super::policy::UrlPolicyError),
    /// A reverse-anchor consent book id that would corrupt the key shape:
    /// empty (its cascade prefix would alias every book's rows) or dotted
    /// (it would parse back as a different book in the revocation list).
    InvalidBookId,
    ConsentRequired,
    Store(KeyStoreError),
    Db(sqlx::Error),
    NotConfigured,
    /// An import file that could not be read; carries the io::ErrorKind
    /// rendering only (never file content).
    FileUnreadable(String),
    /// Keys exist for the endpoint but every one is currently marked dead
    /// or quota-exhausted — rotation has nothing left to try.
    NoUsableKey {
        stored: usize,
    },
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::UnknownProvider => f.write_str("unknown provider"),
            KeyError::EmptyKey => f.write_str("the key is empty"),
            KeyError::OperationConflict => f.write_str("this setup operation already saved a different key; start a new operation"),
            KeyError::BaseUrlFixed => {
                f.write_str("this provider's endpoint is fixed; only OpenAI-compatible keys take a custom base URL")
            }
            KeyError::BaseUrlRequired => {
                f.write_str("an OpenAI-compatible key needs its base URL")
            }
            KeyError::Policy(err) => write!(f, "{err}"),
            KeyError::InvalidBookId => {
                f.write_str("a consent book id must be non-empty and contain no '.'")
            }
            KeyError::ConsentRequired => {
                f.write_str("this endpoint has not been consented to yet")
            }
            KeyError::Store(err) => write!(f, "{err}"),
            KeyError::Db(_) => f.write_str("the settings database is unavailable"),
            KeyError::NotConfigured => f.write_str("no key is configured for that endpoint"),
            KeyError::FileUnreadable(kind) => {
                write!(f, "the key file could not be read: {kind}")
            }
            KeyError::NoUsableKey { stored } => write!(
                f,
                "all {stored} stored key(s) for this endpoint are currently marked \
                 not working or rate limited"
            ),
        }
    }
}

impl From<KeyStoreError> for KeyError {
    fn from(err: KeyStoreError) -> Self {
        KeyError::Store(err)
    }
}

impl From<sqlx::Error> for KeyError {
    fn from(err: sqlx::Error) -> Self {
        KeyError::Db(err)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolQuotaAssessment {
    pub total: usize,
    pub out_of_quota: usize,
    pub invalid: usize,
    pub earliest_retry_wait_secs: Option<u64>,
    pub earliest_retry_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyMeta {
    provider: String,
    added_at: i64,
    /// Slot-era fields ride the same JSON with defaults so every pre-slot
    /// row keeps parsing exactly as it did (bare row = slot 0, unlabeled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used: Option<i64>,
}

/// One configured key slot as `configured` resolves it from the settings
/// rows: the store `account` (what keyring/vault are keyed by), its split
/// (base URL + slot), and the parsed metadata.
struct ConfiguredKey {
    account: String,
    base_url: String,
    slot: u32,
    meta: KeyMeta,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct KeyEntryView {
    pub base_url: String,
    pub slot: u32,
    /// The slot-qualified store account (`base_url` when slot 0). The UI
    /// treats it as the row's opaque identity and the argument to per-slot
    /// commands — it never reconstructs the `#` shape itself.
    pub account: String,
    pub provider: String,
    pub label: Option<String>,
    pub last4: Option<String>,
    pub state: KeyState,
    pub key_present: bool,
    pub custom: bool,
    pub added_at: i64,
    pub last_used: Option<i64>,
}

/// One dial-ready key the rotation resolver hands a session channel: the
/// slot identity plus the sealed key. Deliberately not `Serialize` — it
/// exists to cross from the resolver to a channel adapter and nowhere else.
/// (`Debug` is safe: the key field's own impl redacts.)
#[derive(Debug, Clone)]
pub struct DialCandidate {
    pub account: String,
    pub base_url: String,
    pub slot: u32,
    pub label: Option<String>,
    pub key: ApiKey,
}

impl DialCandidate {
    /// How a candidate is named in user-facing failure text: its label if it
    /// has one, its slot number otherwise. Never bytes, never last4.
    pub fn display_name(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| format!("slot {}", self.slot))
    }
}

pub struct KeyManager {
    store: Arc<KeyStore>,
    states: Mutex<HashMap<String, KeyState>>,
    /// Rotation state, all memory-only like the probe states: per-URL cursor
    /// (the slot AFTER the last successful dial), per-account last close
    /// code, per-account quota retry deadline (ms epoch), and the account of
    /// the most recent successful dial awaiting its `last_used` write.
    cursors: Mutex<HashMap<String, u32>>,
    close_codes: Mutex<HashMap<String, u16>>,
    quota_retry_until: Mutex<HashMap<String, i64>>,
    /// Typed transport evidence (PF-PH1): accounts whose LAST completed
    /// probe attempt failed in timeout/DNS/TLS/connect/transport. Cleared
    /// by any conclusive result. Memory-only like the probe states.
    transport_failed: Mutex<HashSet<String>>,
    /// Operator-disabled slots (PF-PH1 `Disabled`): persisted under
    /// [`DISABLED_SLOTS_KEY`]; this mirror loads lazily and stays in sync.
    disabled_slots: Mutex<Option<HashSet<String>>>,
    disabled_mutation_lock: tokio::sync::Mutex<()>,
    dial_success: Mutex<Option<String>>,
    /// In-flight explicit pool tests (S-5 blocker 6), keyed by
    /// provider-id + canonical URL: typed progress the overview projects as
    /// PF-PH1 `Checking`, plus a cooperative cancel flag. Memory-only.
    pool_tests: Mutex<HashMap<(String, String), PoolTestStatus>>,
    probes_run: AtomicU32,
    client: OnceLock<reqwest::Client>,
}

/// Typed progress of one in-flight explicit pool test.
#[derive(Debug, Clone, Default)]
pub struct PoolTestStatus {
    pub completed: u32,
    pub total: u32,
    /// The account whose probe is in flight right now.
    pub current_account: Option<String>,
    pub cancelled: bool,
}

impl KeyManager {
    pub fn new(vault_path: std::path::PathBuf) -> Self {
        Self::with_store(KeyStore::new(vault_path))
    }

    /// Session-only manager for integration proofs: keys live in memory,
    /// no OS keychain or vault file is touched. Never on an app path.
    #[doc(hidden)]
    pub fn session_only_for_tests() -> Self {
        Self::with_store(KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            crate::voice::keys::store::Backend::Session(Default::default()),
        ))
    }

    pub(crate) fn with_store(store: KeyStore) -> Self {
        Self {
            store: Arc::new(store),
            states: Mutex::new(HashMap::new()),
            cursors: Mutex::new(HashMap::new()),
            close_codes: Mutex::new(HashMap::new()),
            quota_retry_until: Mutex::new(HashMap::new()),
            transport_failed: Mutex::new(HashSet::new()),
            disabled_slots: Mutex::new(None),
            disabled_mutation_lock: tokio::sync::Mutex::new(()),
            dial_success: Mutex::new(None),
            pool_tests: Mutex::new(HashMap::new()),
            probes_run: AtomicU32::new(0),
            client: OnceLock::new(),
        }
    }

    pub fn probes_run(&self) -> u32 {
        self.probes_run.load(Ordering::SeqCst)
    }

    pub(crate) fn state_of(&self, base_url: &str) -> KeyState {
        self.states
            .lock()
            .expect("state lock poisoned")
            .get(base_url)
            .copied()
            .unwrap_or(KeyState::Unchecked)
    }

    fn set_state(&self, base_url: &str, state: KeyState) {
        self.states
            .lock()
            .expect("state lock poisoned")
            .insert(base_url.to_owned(), state);
    }

    async fn store_blocking<R, F>(&self, op: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&KeyStore) -> R + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || op(&store))
            .await
            .expect("key store operations do not panic")
    }

    pub async fn storage_status(&self) -> StorageStatus {
        self.store_blocking(KeyStore::status).await
    }

    /// Hands a session channel the sealed key for one endpoint at connect time
    /// (VOICE_AGENT §7). The key crosses this surface only as [`ApiKey`] — the
    /// manager never reads the raw bytes, and the caller (a channel adapter)
    /// puts them in an auth header and nowhere else.
    pub async fn session_key(&self, base_url: &str) -> Result<ApiKey, KeyError> {
        let url = base_url.to_owned();
        self.store_blocking(move |store| store.load(&url))
            .await?
            .ok_or(KeyError::NotConfigured)
    }

    /// Whether a slot may be handed to a new session: live (`Active`) and
    /// untested (`Unchecked`) always; quota-exhausted only once its
    /// provider-given retry deadline has passed (without one it stays out
    /// until a probe or restart clears it); dead (`Invalid`) never.
    fn eligible_now(&self, account: &str) -> bool {
        match self.state_of(account) {
            KeyState::Active | KeyState::Unchecked => true,
            KeyState::Invalid => false,
            KeyState::OutOfQuota => self
                .quota_retry_until
                .lock()
                .expect("rotation lock poisoned")
                .get(account)
                .is_some_and(|until| now_ms() >= *until),
        }
    }

    /// Round-robin session-start resolution (RCA §9): up to `limit`
    /// dial-ready candidates for one endpoint, in slot order starting at the
    /// rotation cursor (the slot after the last successful dial), wrapping.
    /// Dead and quota-exhausted slots are skipped by [`Self::eligible_now`];
    /// slots whose bytes cannot be loaded are skipped too, unless every
    /// eligible slot fails to load — then the first store error surfaces
    /// (a locked vault must say so, not masquerade as "no keys").
    pub async fn resolve_rotation(
        &self,
        pool: &SqlitePool,
        base_url: &str,
        limit: usize,
    ) -> Result<Vec<DialCandidate>, KeyError> {
        self.resolve_rotation_scoped(pool, None, base_url, limit)
            .await
    }

    pub async fn resolve_provider_rotation(
        &self,
        pool: &SqlitePool,
        provider_id: &str,
        base_url: &str,
        limit: usize,
    ) -> Result<Vec<DialCandidate>, KeyError> {
        self.resolve_rotation_scoped(pool, Some(provider_id), base_url, limit)
            .await
    }

    async fn resolve_rotation_scoped(
        &self,
        pool: &SqlitePool,
        provider_id: Option<&str>,
        base_url: &str,
        limit: usize,
    ) -> Result<Vec<DialCandidate>, KeyError> {
        let stored: Vec<ConfiguredKey> = self
            .configured(pool)
            .await?
            .into_iter()
            .filter(|entry| {
                crate::providers::directory::same_credential_group(
                    provider_id.unwrap_or(&entry.meta.provider),
                    &entry.base_url,
                    base_url,
                )
            })
            .filter(|entry| provider_id.map_or(true, |id| entry.meta.provider == id))
            .collect();
        if stored.is_empty() {
            return Err(KeyError::NotConfigured);
        }
        let disabled = self.disabled_set(pool).await?;
        let eligible: Vec<&ConfiguredKey> = stored
            .iter()
            .filter(|entry| !disabled.contains(&entry.account))
            .filter(|entry| self.eligible_now(&entry.account))
            .collect();
        if eligible.is_empty() {
            return Err(KeyError::NoUsableKey {
                stored: stored.len(),
            });
        }
        let cursor = self
            .cursors
            .lock()
            .expect("rotation lock poisoned")
            .get(base_url)
            .copied()
            .unwrap_or(0);
        let start = eligible
            .iter()
            .position(|entry| entry.slot >= cursor)
            .unwrap_or(0);

        let mut candidates = Vec::new();
        let mut first_load_error: Option<KeyStoreError> = None;
        for index in 0..eligible.len() {
            if candidates.len() == limit {
                break;
            }
            let entry = eligible[(start + index) % eligible.len()];
            let loaded = {
                let account = entry.account.clone();
                self.store_blocking(move |store| store.load(&account)).await
            };
            match loaded {
                Ok(Some(key)) => candidates.push(DialCandidate {
                    account: entry.account.clone(),
                    base_url: entry.base_url.clone(),
                    slot: entry.slot,
                    label: entry.meta.label.clone(),
                    key,
                }),
                Ok(None) => {}
                Err(err) => first_load_error = first_load_error.or(Some(err)),
            }
        }
        if candidates.is_empty() {
            return Err(match first_load_error {
                Some(err) => KeyError::Store(err),
                None => KeyError::NoUsableKey {
                    stored: stored.len(),
                },
            });
        }
        Ok(candidates)
    }

    /// A dial on this account reached `setupComplete`: the key is live, the
    /// cursor moves past its slot (that IS the round-robin), and the account
    /// is remembered so the session-start path can persist `last_used` while
    /// it still has a database handle in scope.
    pub fn record_dial_success(&self, account: &str) {
        self.record_pool_success(account);
        *self.dial_success.lock().expect("rotation lock poisoned") = Some(account.to_owned());
    }

    /// The batch/model-fetch half of dial success (S-5 blocker 5): the key
    /// worked, so promote it and advance the round-robin cursor past its
    /// slot — WITHOUT touching the `dial_success` cue, which belongs to the
    /// Voice Tutor session-start path's `last_used` bookkeeping alone.
    pub fn record_pool_success(&self, account: &str) {
        let (base_url, slot) = split_slot_account(account);
        self.set_state(account, KeyState::Active);
        self.close_codes
            .lock()
            .expect("rotation lock poisoned")
            .remove(account);
        self.quota_retry_until
            .lock()
            .expect("rotation lock poisoned")
            .remove(account);
        self.cursors
            .lock()
            .expect("rotation lock poisoned")
            .insert(base_url.to_owned(), slot + 1);
    }

    /// Advances the round-robin cursor past this slot without modifying key state.
    /// Used when rotating past a slot after a retryable provider error.
    pub fn advance_pool_cursor(&self, account: &str) {
        let (base_url, slot) = split_slot_account(account);
        self.cursors
            .lock()
            .expect("rotation lock poisoned")
            .insert(base_url.to_owned(), slot + 1);
    }

    /// Feed one settled provider outcome back into slot state (S-5 blocker
    /// 5): success advances the cursor; an auth refusal demotes the slot to
    /// Invalid; a quota refusal demotes it to OutOfQuota (re-entering after
    /// a provider-given retry deadline). Transport/timeout/malformed
    /// outcomes deliberately do NOT demote — they say nothing about the KEY.
    pub fn record_pool_outcome(
        &self,
        account: &str,
        outcome: Result<(), &crate::providers::registry::ScriptError>,
    ) {
        use crate::providers::registry::ScriptErrorKind;
        match outcome {
            Ok(()) => self.record_pool_success(account),
            Err(error) => match error.kind {
                ScriptErrorKind::Auth => self.record_dial_auth_failure(account, None),
                ScriptErrorKind::Quota => {
                    let retry_after = error.retry_after_secs.or(Some(60));
                    self.record_dial_quota(account, None, retry_after)
                }
                _ => {}
            },
        }
    }

    /// Begin an explicit pool test: registers typed progress (projected as
    /// PF-PH1 `Checking` by the overview) and clears any stale cancel flag.
    /// Returns false — and touches NOTHING — when a test is already
    /// running for this pool: a second concurrent run must never overwrite
    /// or cancel the first run's truth (re-verdict item 1).
    #[must_use]
    pub fn begin_pool_test(&self, provider: &str, base_url: &str, total: u32) -> bool {
        let mut tests = self.pool_tests.lock().expect("pool test lock poisoned");
        let key = (provider.to_owned(), base_url.to_owned());
        if tests.contains_key(&key) {
            return false;
        }
        tests.insert(
            key,
            PoolTestStatus {
                completed: 0,
                total,
                current_account: None,
                cancelled: false,
            },
        );
        true
    }

    /// Advance the in-flight pool test. Returns false when the test was
    /// cancelled (the runner stops probing).
    pub fn update_pool_test(
        &self,
        provider: &str,
        base_url: &str,
        completed: u32,
        current_account: Option<&str>,
    ) -> bool {
        let mut tests = self.pool_tests.lock().expect("pool test lock poisoned");
        match tests.get_mut(&(provider.to_owned(), base_url.to_owned())) {
            Some(status) => {
                status.completed = completed;
                status.current_account = current_account.map(str::to_owned);
                !status.cancelled
            }
            None => false,
        }
    }

    /// Cooperatively cancel an in-flight pool test; a no-op when none runs.
    pub fn cancel_pool_test(&self, provider: &str, base_url: &str) {
        if let Some(status) = self
            .pool_tests
            .lock()
            .expect("pool test lock poisoned")
            .get_mut(&(provider.to_owned(), base_url.to_owned()))
        {
            status.cancelled = true;
        }
    }

    pub fn finish_pool_test(&self, provider: &str, base_url: &str) {
        self.pool_tests
            .lock()
            .expect("pool test lock poisoned")
            .remove(&(provider.to_owned(), base_url.to_owned()));
    }

    pub fn pool_test_status(&self, provider: &str, base_url: &str) -> Option<PoolTestStatus> {
        self.pool_tests
            .lock()
            .expect("pool test lock poisoned")
            .get(&(provider.to_owned(), base_url.to_owned()))
            .cloned()
    }

    /// The provider refused this key at dial time (the #218 1007/1008
    /// mapping): dead until a probe or a re-add says otherwise.
    pub fn record_dial_auth_failure(&self, account: &str, close_code: Option<u16>) {
        self.set_state(account, KeyState::Invalid);
        if let Some(code) = close_code {
            self.close_codes
                .lock()
                .expect("rotation lock poisoned")
                .insert(account.to_owned(), code);
        }
    }

    /// The provider reported this key out of quota at dial time. With a
    /// provider-given retry-after the slot re-enters rotation once the
    /// deadline passes; without one it stays out for this process.
    pub fn record_dial_quota(
        &self,
        account: &str,
        close_code: Option<u16>,
        retry_after_secs: Option<u64>,
    ) {
        self.set_state(account, KeyState::OutOfQuota);
        if let Some(code) = close_code {
            self.close_codes
                .lock()
                .expect("rotation lock poisoned")
                .insert(account.to_owned(), code);
        }
        if let Some(secs) = retry_after_secs {
            self.quota_retry_until
                .lock()
                .expect("rotation lock poisoned")
                .insert(account.to_owned(), now_ms() + (secs as i64) * 1000);
        }
    }

    /// A Live-WS probe proved this key working: promote it — but touch
    /// neither the rotation cursor nor the last_used cue, which belong to
    /// real session dials only.
    pub fn record_probe_success(&self, account: &str) {
        self.set_state(account, KeyState::Active);
        self.close_codes
            .lock()
            .expect("rotation lock poisoned")
            .remove(account);
        self.quota_retry_until
            .lock()
            .expect("rotation lock poisoned")
            .remove(account);
    }

    /// Takes (and clears) the account of the most recent successful dial —
    /// the session-start path's cue to write `last_used`.
    /// PF-PH1 slot evidence: whether the last completed probe attempt on
    /// this account was a typed transport failure.
    pub fn transport_failed_for(&self, account: &str) -> bool {
        self.transport_failed
            .lock()
            .expect("transport lock poisoned")
            .contains(account)
    }

    /// PF-PH1: the provider-given rate-limit retry deadline has elapsed
    /// for this account (meaningful only alongside `OutOfQuota`).
    pub fn retry_after_elapsed(&self, account: &str) -> bool {
        self.quota_retry_until
            .lock()
            .expect("rotation lock poisoned")
            .get(account)
            .is_some_and(|until| now_ms() >= *until)
    }

    /// The provider-given retry deadline for this account, if one exists
    /// and has not elapsed (unix ms).
    pub fn retry_after_until(&self, account: &str) -> Option<i64> {
        self.quota_retry_until
            .lock()
            .expect("rotation lock poisoned")
            .get(account)
            .copied()
            .filter(|until| now_ms() < *until)
    }

    /// Returns the minimum number of seconds to wait until the earliest quota deadline
    /// expires among configured keys for this provider and base_url.
    /// Returns None if there are no keys with unelapsed quota deadlines.
    pub async fn earliest_quota_retry_wait_secs(
        &self,
        pool: &SqlitePool,
        provider_id: Option<&str>,
        base_url: &str,
    ) -> Result<Option<u64>, KeyError> {
        let stored = self.configured(pool).await?;
        let deadlines = self
            .quota_retry_until
            .lock()
            .expect("rotation lock poisoned");
        let now = now_ms();
        let min_until = stored
            .into_iter()
            .filter(|entry| {
                crate::providers::directory::same_credential_group(
                    provider_id.unwrap_or(&entry.meta.provider),
                    &entry.base_url,
                    base_url,
                )
            })
            .filter(|entry| provider_id.map_or(true, |id| entry.meta.provider == id))
            .filter_map(|entry| deadlines.get(&entry.account).copied())
            .filter(|until| *until > now)
            .min();

        Ok(min_until.map(|until| {
            let diff_ms = until - now;
            ((diff_ms + 999) / 1000) as u64
        }))
    }

    pub async fn pool_quota_assessment(
        &self,
        pool: &SqlitePool,
        provider_id: Option<&str>,
        base_url: &str,
    ) -> Result<PoolQuotaAssessment, KeyError> {
        let stored = self.configured(pool).await?;
        let disabled = self.disabled_set(pool).await.unwrap_or_default();
        let deadlines = self
            .quota_retry_until
            .lock()
            .expect("rotation lock poisoned");
        let now = now_ms();
        let mut total = 0;
        let mut out_of_quota = 0;
        let mut invalid = 0;
        let mut min_until: Option<i64> = None;

        for entry in stored
            .into_iter()
            .filter(|entry| {
                crate::providers::directory::same_credential_group(
                    provider_id.unwrap_or(&entry.meta.provider),
                    &entry.base_url,
                    base_url,
                )
            })
            .filter(|entry| provider_id.map_or(true, |id| entry.meta.provider == id))
            .filter(|entry| !disabled.contains(&entry.account))
        {
            total += 1;
            match self.state_of(&entry.account) {
                KeyState::OutOfQuota => {
                    out_of_quota += 1;
                    if let Some(until) = deadlines.get(&entry.account).copied() {
                        if until > now {
                            min_until = Some(min_until.map_or(until, |m| m.min(until)));
                        }
                    }
                }
                KeyState::Invalid => {
                    invalid += 1;
                }
                _ => {}
            }
        }

        let earliest_retry_wait_secs = min_until.map(|until| {
            let diff_ms = until - now;
            ((diff_ms + 999) / 1000) as u64
        });

        Ok(PoolQuotaAssessment {
            total,
            out_of_quota,
            invalid,
            earliest_retry_wait_secs,
            earliest_retry_at: min_until,
        })
    }

    async fn disabled_set(&self, pool: &SqlitePool) -> Result<HashSet<String>, KeyError> {
        if let Some(set) = self
            .disabled_slots
            .lock()
            .expect("disabled lock poisoned")
            .clone()
        {
            return Ok(set);
        }
        let set: HashSet<String> = settings::get_setting(pool, DISABLED_SLOTS_KEY)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        *self.disabled_slots.lock().expect("disabled lock poisoned") = Some(set.clone());
        Ok(set)
    }

    /// PF-PH1 `Disabled`: an explicit operator choice. The slot stays
    /// configured (and counted as such) but leaves the operational health
    /// population and rotation until re-enabled.
    pub async fn set_slot_disabled(
        &self,
        pool: &SqlitePool,
        account: &str,
        disabled: bool,
    ) -> Result<(), KeyError> {
        let _guard = self.disabled_mutation_lock.lock().await;
        let exists = self
            .configured(pool)
            .await?
            .iter()
            .any(|entry| entry.account == account);
        if !exists {
            return Err(KeyError::NotConfigured);
        }
        let mut set = self.disabled_set(pool).await?;
        if disabled {
            set.insert(account.to_owned());
        } else {
            set.remove(account);
        }
        let json =
            serde_json::to_string(&set.iter().collect::<Vec<_>>()).expect("string set serializes");
        settings::set_setting(pool, DISABLED_SLOTS_KEY, &json).await?;
        *self.disabled_slots.lock().expect("disabled lock poisoned") = Some(set);
        Ok(())
    }

    pub async fn slot_disabled(&self, pool: &SqlitePool, account: &str) -> bool {
        self.disabled_set(pool)
            .await
            .map(|set| set.contains(account))
            .unwrap_or(false)
    }

    /// Provider-plus-canonical-URL pool custody (PF-PH1, cross-slice
    /// MUST-FIX 2): ONE dial-ready key for this provider's pool at this
    /// endpoint, resolved through the same rotation the Voice Tutor uses.
    /// The provider check makes cross-provider key ambiguity structurally
    /// impossible: a slot stored under another provider id never matches,
    /// however the URLs happen to coincide.
    pub async fn resolve_pool_key(
        &self,
        pool: &SqlitePool,
        provider_id: &str,
        base_url: &str,
    ) -> Result<DialCandidate, KeyError> {
        let mut candidates = self
            .resolve_provider_rotation(pool, provider_id, base_url, 1)
            .await?;
        Ok(candidates.remove(0))
    }

    pub async fn tutor_readiness(&self, pool: &SqlitePool) -> Result<TutorKeyReadiness, KeyError> {
        match self
            .resolve_pool_key(
                pool,
                "gemini",
                Provider::Gemini
                    .default_base_url()
                    .expect("Gemini has a fixed endpoint"),
            )
            .await
        {
            Ok(_) => Ok(TutorKeyReadiness::Present),
            Err(KeyError::NotConfigured) => Ok(TutorKeyReadiness::Missing),
            Err(KeyError::NoUsableKey { .. }) => Ok(TutorKeyReadiness::Unavailable),
            Err(KeyError::Store(KeyStoreError::VaultLocked)) => Ok(TutorKeyReadiness::Locked),
            Err(error) => Err(error),
        }
    }

    pub fn take_dial_success(&self) -> Option<String> {
        self.dial_success
            .lock()
            .expect("rotation lock poisoned")
            .take()
    }

    /// Persists `last_used` on one slot's metadata row. Timestamps only;
    /// a missing row (removed mid-session) is a no-op, not an error.
    pub async fn note_last_used(&self, pool: &SqlitePool, account: &str) -> Result<(), KeyError> {
        let meta_key = format!("{KEY_META_PREFIX}{account}");
        let Some(raw) = settings::get_setting(pool, &meta_key).await? else {
            return Ok(());
        };
        let Ok(mut meta) = serde_json::from_str::<KeyMeta>(&raw) else {
            return Ok(());
        };
        meta.last_used = Some(now_ms());
        let meta_json = serde_json::to_string(&meta).expect("a flat metadata struct serializes");
        settings::set_setting(pool, &meta_key, &meta_json).await?;
        Ok(())
    }

    pub async fn unlock_vault(
        &self,
        passphrase: Zeroizing<String>,
    ) -> Result<StorageStatus, KeyError> {
        self.store_blocking(move |store| store.unlock_file_vault(&passphrase))
            .await?;
        Ok(self.storage_status().await)
    }

    pub async fn use_session_only(&self) -> Result<StorageStatus, KeyError> {
        self.store_blocking(KeyStore::use_session_only).await?;
        Ok(self.storage_status().await)
    }

    pub async fn record_consent(
        &self,
        pool: &SqlitePool,
        raw_url: &str,
    ) -> Result<String, KeyError> {
        let canonical = canonical_base_url(raw_url).map_err(KeyError::Policy)?;
        let consent_key = format!("{CONSENT_PREFIX}{canonical}");
        if settings::get_setting(pool, &consent_key).await?.is_none() {
            settings::set_setting(pool, &consent_key, &now_ms().to_string()).await?;
        }
        Ok(canonical)
    }

    pub async fn check_url(&self, pool: &SqlitePool, raw_url: &str) -> Result<UrlCheck, KeyError> {
        let canonical = canonical_base_url(raw_url).map_err(KeyError::Policy)?;
        let consent_key = format!("{CONSENT_PREFIX}{canonical}");
        let consented = settings::get_setting(pool, &consent_key).await?.is_some();
        Ok(UrlCheck {
            canonical_url: canonical,
            consented,
        })
    }

    /// Records the per-book reverse-anchor consent (VOICE_AGENT §11.1) —
    /// asked once per (book, endpoint), recorded once, mirroring
    /// [`Self::record_consent`].
    pub async fn record_reverse_anchor_consent(
        &self,
        pool: &SqlitePool,
        book_id: &str,
        raw_url: &str,
    ) -> Result<String, KeyError> {
        validated_consent_book_id(book_id)?;
        let canonical = canonical_base_url(raw_url).map_err(KeyError::Policy)?;
        let consent_key = reverse_anchor_consent_key(book_id, &canonical);
        if settings::get_setting(pool, &consent_key).await?.is_none() {
            settings::set_setting(pool, &consent_key, &now_ms().to_string()).await?;
        }
        Ok(canonical)
    }

    /// The gate the session runtime must pass before any tap/selection
    /// leaves the device as a reverse anchor: without recorded consent for
    /// this exact (book, endpoint) pair, taps send nothing (§11.1).
    pub async fn check_reverse_anchor_consent(
        &self,
        pool: &SqlitePool,
        book_id: &str,
        raw_url: &str,
    ) -> Result<UrlCheck, KeyError> {
        validated_consent_book_id(book_id)?;
        let canonical = canonical_base_url(raw_url).map_err(KeyError::Policy)?;
        let consent_key = reverse_anchor_consent_key(book_id, &canonical);
        let consented = settings::get_setting(pool, &consent_key).await?.is_some();
        Ok(UrlCheck {
            canonical_url: canonical,
            consented,
        })
    }

    /// Revocable in settings (§11.1): dropping the row restores the
    /// never-consented state — the next tap asks again instead of sending.
    pub async fn revoke_reverse_anchor_consent(
        &self,
        pool: &SqlitePool,
        book_id: &str,
        raw_url: &str,
    ) -> Result<(), KeyError> {
        validated_consent_book_id(book_id)?;
        let canonical = canonical_base_url(raw_url).map_err(KeyError::Policy)?;
        settings::delete_setting(pool, &reverse_anchor_consent_key(book_id, &canonical)).await?;
        Ok(())
    }

    /// Every recorded reverse-anchor consent, for the settings revocation
    /// surface. Rows whose key shape no longer parses are skipped rather
    /// than invented.
    pub async fn list_reverse_anchor_consents(
        &self,
        pool: &SqlitePool,
    ) -> Result<Vec<ReverseAnchorConsent>, KeyError> {
        let rows = settings::list_settings_with_prefix(pool, REVERSE_ANCHOR_CONSENT_PREFIX).await?;
        Ok(rows
            .into_iter()
            .filter_map(|(key, value)| {
                let rest = key.strip_prefix(REVERSE_ANCHOR_CONSENT_PREFIX)?;
                let (book_id, url) = rest.split_once('.')?;
                Some(ReverseAnchorConsent {
                    book_id: book_id.to_owned(),
                    base_url: url.to_owned(),
                    recorded_at_ms: value.parse().ok()?,
                })
            })
            .collect())
    }

    /// Resolves the endpoint a key lands on: fixed providers own theirs,
    /// custom ones need a canonical, already-consented URL — the same gate
    /// for a single add and a 47-line import.
    async fn canonical_for(
        &self,
        pool: &SqlitePool,
        provider: Provider,
        base_url: Option<&str>,
    ) -> Result<String, KeyError> {
        match (provider.default_base_url(), base_url) {
            (Some(fixed), None) => Ok(fixed.to_owned()),
            (Some(_), Some(_)) => Err(KeyError::BaseUrlFixed),
            (None, Some(raw)) => {
                let canonical = canonical_base_url(raw).map_err(KeyError::Policy)?;
                let consent_key = format!("{CONSENT_PREFIX}{canonical}");
                if settings::get_setting(pool, &consent_key).await?.is_none() {
                    return Err(KeyError::ConsentRequired);
                }
                Ok(canonical)
            }
            (None, None) => Err(KeyError::BaseUrlRequired),
        }
    }

    /// Adds a key as a NEW slot — never displacing one already stored. The
    /// first key of an endpoint takes slot 0 (the pre-slot account, so a
    /// single-key install is bit-identical to before); every later add takes
    /// max(existing)+1, which never reuses a freed low number while higher
    /// slots are still live.
    pub async fn add_key(
        &self,
        pool: &SqlitePool,
        provider_id: &str,
        base_url: Option<&str>,
        key_raw: String,
        label: Option<&str>,
    ) -> Result<KeyEntryView, KeyError> {
        let provider = Provider::from_id(provider_id).ok_or(KeyError::UnknownProvider)?;
        let key = ApiKey::new(key_raw);
        if key.is_empty() {
            return Err(KeyError::EmptyKey);
        }
        let label = label
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned);
        let canonical = self.canonical_for(pool, provider, base_url).await?;
        self.add_slot(pool, provider, &canonical, key, label).await
    }

    pub async fn add_setup_key(
        &self,
        pool: &SqlitePool,
        provider_id: &str,
        base_url: Option<&str>,
        key: ApiKey,
        operation_id: &str,
    ) -> Result<KeyEntryView, KeyError> {
        let provider = Provider::from_id(provider_id).ok_or(KeyError::UnknownProvider)?;
        let canonical = self.canonical_for(pool, provider, base_url).await?;
        let journal = format!("provider_setup_key.{operation_id}");
        if let Some(account) = settings::get_setting(pool, &journal).await? {
            if let Some(entry) = self.list_entries(pool).await?.into_iter().find(|entry| {
                entry.account == account
                    && entry.provider == provider_id
                    && entry.base_url == canonical
            }) {
                if entry.key_present {
                    let account = entry.account.clone();
                    let same_key = self
                        .store_blocking(move |store| {
                            Ok::<bool, KeyStoreError>(
                                store
                                    .load(&account)?
                                    .is_some_and(|stored| stored.matches(&key)),
                            )
                        })
                        .await?;
                    if !same_key {
                        return Err(KeyError::OperationConflict);
                    }
                } else {
                    let account = entry.account.clone();
                    self.store_blocking(move |store| store.store(&account, &key))
                        .await?;
                }
                return self
                    .list_entries(pool)
                    .await?
                    .into_iter()
                    .find(|entry| entry.account == account)
                    .ok_or(KeyError::NotConfigured);
            }
        }
        self.add_slot_with_operation(pool, provider, &canonical, key, None, Some(&journal))
            .await
    }

    async fn add_slot(
        &self,
        pool: &SqlitePool,
        provider: Provider,
        canonical: &str,
        key: ApiKey,
        label: Option<String>,
    ) -> Result<KeyEntryView, KeyError> {
        self.add_slot_with_operation(pool, provider, canonical, key, label, None)
            .await
    }

    async fn add_slot_with_operation(
        &self,
        pool: &SqlitePool,
        provider: Provider,
        canonical: &str,
        key: ApiKey,
        label: Option<String>,
        journal: Option<&str>,
    ) -> Result<KeyEntryView, KeyError> {
        static WRITER: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _writer = WRITER
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let slot = self
            .configured(pool)
            .await?
            .iter()
            .filter(|entry| entry.base_url == canonical)
            .map(|entry| entry.slot)
            .max()
            .map_or(0, |highest| highest + 1);
        let account = slot_account(canonical, slot);

        let last4 = key.last4();
        let meta = KeyMeta {
            provider: provider.id().to_owned(),
            added_at: now_ms(),
            label: label.clone(),
            last_used: None,
        };
        let meta_json = serde_json::to_string(&meta).expect("a flat metadata struct serializes");
        let meta_key = format!("{KEY_META_PREFIX}{account}");
        let mut transaction = pool.begin().await?;
        sqlx::query("INSERT INTO app_setting (key, value, updated_at) VALUES (?, ?, ?)")
            .bind(&meta_key)
            .bind(&meta_json)
            .bind(now_ms())
            .execute(&mut *transaction)
            .await?;
        if let Some(journal) = journal {
            sqlx::query("INSERT INTO app_setting (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(journal).bind(&account).bind(now_ms()).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        let account_clone = account.clone();
        if let Err(error) = self
            .store_blocking(move |store| store.store(&account_clone, &key))
            .await
        {
            if journal.is_none() {
                settings::delete_setting(pool, &meta_key).await?;
            }
            return Err(error.into());
        }
        self.forget_account(&account);
        self.set_state(&account, KeyState::Unchecked);
        Ok(KeyEntryView {
            base_url: canonical.to_owned(),
            slot,
            account,
            provider: provider.id().to_owned(),
            label,
            last4: Some(last4),
            state: KeyState::Unchecked,
            key_present: true,
            custom: provider.default_base_url().is_none(),
            added_at: meta.added_at,
            last_used: None,
        })
    }

    /// Bulk import (RCA §9's 47-key case): every parsed `NAME=VALUE` line
    /// becomes an untested slot labeled with its env-var name, except lines
    /// whose key material is already stored for this endpoint (or appeared
    /// earlier in the same text) — those are reported as duplicates by line
    /// and label, so importing the same pool file twice is a no-op with a
    /// receipt instead of 47 ghost slots. The report renders counts, labels,
    /// and line numbers only; key bytes stay inside [`ApiKey`].
    pub async fn import_keys(
        &self,
        pool: &SqlitePool,
        provider_id: &str,
        base_url: Option<&str>,
        text: String,
    ) -> Result<ImportReport, KeyError> {
        let provider = Provider::from_id(provider_id).ok_or(KeyError::UnknownProvider)?;
        let canonical = self.canonical_for(pool, provider, base_url).await?;
        let text = Zeroizing::new(text);
        let parsed = super::import::parse_env_lines(&text);

        let mut known: Vec<ApiKey> = Vec::new();
        for entry in self.configured(pool).await? {
            if entry.base_url != canonical {
                continue;
            }
            let account = entry.account.clone();
            match self.store_blocking(move |store| store.load(&account)).await {
                Ok(Some(key)) => known.push(key),
                Ok(None) => {}
                Err(err) => return Err(err.into()),
            }
        }

        let mut report = ImportReport {
            imported: Vec::new(),
            rejected: parsed.rejected,
            duplicates: Vec::new(),
        };
        for parsed_key in parsed.keys {
            if known.iter().any(|key| key.matches(&parsed_key.key)) {
                report.duplicates.push(DuplicateLine {
                    line: parsed_key.line,
                    label: parsed_key.label,
                });
                continue;
            }
            let view = match self
                .add_slot(
                    pool,
                    provider,
                    &canonical,
                    parsed_key.key.clone(),
                    Some(parsed_key.label),
                )
                .await
            {
                Ok(view) => view,
                Err(err) => {
                    if report.imported.is_empty() {
                        return Err(err);
                    }
                    return Ok(report);
                }
            };
            known.push(parsed_key.key);
            report.imported.push(view);
        }
        Ok(report)
    }

    /// The file variant: the webview hands over a PATH from the native
    /// picker and the bytes go disk → here → store without ever crossing
    /// IPC or the DOM.
    pub async fn import_keys_from_file(
        &self,
        pool: &SqlitePool,
        provider_id: &str,
        base_url: Option<&str>,
        path: std::path::PathBuf,
    ) -> Result<ImportReport, KeyError> {
        let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
            .await
            .expect("reading a file does not panic")
            .map_err(|err| KeyError::FileUnreadable(err.kind().to_string()))?;
        self.import_keys(pool, provider_id, base_url, text).await
    }

    pub async fn remove_key(
        &self,
        pool: &SqlitePool,
        base_url: &str,
        slot: u32,
    ) -> Result<(), KeyError> {
        let account = slot_account(base_url, slot);
        let meta_key = format!("{KEY_META_PREFIX}{account}");
        if settings::get_setting(pool, &meta_key).await?.is_none() {
            return Err(KeyError::NotConfigured);
        }
        {
            let account = account.clone();
            self.store_blocking(move |store| store.remove(&account))
                .await?;
        }
        settings::delete_setting(pool, &meta_key).await?;
        self.forget_account(&account);
        // The persisted operator-disable is a fact about the REMOVED key,
        // not about whatever fresh key later inherits this slot number —
        // clear it with the slot (re-verdict item 2: no ghost state).
        // Directly on the set: `set_slot_disabled` refuses unconfigured
        // accounts, and this one was just removed.
        let mut set = self.disabled_set(pool).await?;
        if set.remove(&account) {
            let json = serde_json::to_string(&set.iter().collect::<Vec<_>>())
                .expect("string set serializes");
            settings::set_setting(pool, DISABLED_SLOTS_KEY, &json).await?;
            *self.disabled_slots.lock().expect("disabled lock poisoned") = Some(set);
        }
        Ok(())
    }

    /// Drops every memory-only mark for an account. Called on remove and on
    /// add: once all of an endpoint's slots are gone the numbering restarts
    /// at 0, and a stale close code or quota deadline from the dead era must
    /// not taint the fresh key that inherits the number.
    fn forget_account(&self, account: &str) {
        self.states
            .lock()
            .expect("state lock poisoned")
            .remove(account);
        self.close_codes
            .lock()
            .expect("rotation lock poisoned")
            .remove(account);
        self.quota_retry_until
            .lock()
            .expect("rotation lock poisoned")
            .remove(account);
        self.transport_failed
            .lock()
            .expect("transport lock poisoned")
            .remove(account);
    }

    /// Every configured slot, in deterministic (base URL, slot-number) order.
    /// Settings rows come back in whatever order the query gives; sorting
    /// numerically here — not lexically, where `#10` would land before `#2` —
    /// is what makes the rotation order of `session_key_rotating` stable
    /// across process restarts.
    async fn configured(&self, pool: &SqlitePool) -> Result<Vec<ConfiguredKey>, KeyError> {
        let rows = settings::list_settings_with_prefix(pool, KEY_META_PREFIX).await?;
        let mut keys: Vec<ConfiguredKey> = rows
            .into_iter()
            .filter_map(|(key, value)| {
                let account = key[KEY_META_PREFIX.len()..].to_string();
                let meta = serde_json::from_str::<KeyMeta>(&value).ok()?;
                let (base_url, slot) = split_slot_account(&account);
                Some(ConfiguredKey {
                    base_url: base_url.to_owned(),
                    slot,
                    account,
                    meta,
                })
            })
            .collect();
        keys.sort_by(|a, b| (&a.base_url, a.slot).cmp(&(&b.base_url, b.slot)));
        Ok(keys)
    }

    pub async fn configuration_fingerprint(
        pool: &SqlitePool,
        provider: &str,
        base_url: &str,
    ) -> Result<String, KeyError> {
        use sha2::{Digest, Sha256};
        let disabled: HashSet<String> = settings::get_setting(pool, DISABLED_SLOTS_KEY)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let mut entries = Vec::new();
        for (key, value) in settings::list_settings_with_prefix(pool, KEY_META_PREFIX).await? {
            let account = key[KEY_META_PREFIX.len()..].to_string();
            let Ok(meta) = serde_json::from_str::<KeyMeta>(&value) else {
                continue;
            };
            let (url, _) = split_slot_account(&account);
            if meta.provider == provider
                && crate::providers::directory::same_credential_group(provider, url, base_url)
            {
                let is_disabled = disabled.contains(&account);
                entries.push((account, meta.added_at, is_disabled));
            }
        }
        entries.sort();
        Ok(hex::encode(Sha256::digest(
            serde_json::to_vec(&entries).expect("metadata tuples serialize"),
        )))
    }

    async fn view_of(&self, entry: &ConfiguredKey) -> KeyEntryView {
        let provider = Provider::from_id(&entry.meta.provider);
        let loaded = {
            let account = entry.account.clone();
            self.store_blocking(move |store| store.load(&account)).await
        };
        let (last4, key_present) = match loaded {
            Ok(Some(key)) => (Some(key.last4()), true),
            Ok(None) | Err(_) => (None, false),
        };
        KeyEntryView {
            state: self.state_of(&entry.account),
            custom: provider.map_or(true, |p| p.default_base_url().is_none()),
            base_url: entry.base_url.clone(),
            slot: entry.slot,
            account: entry.account.clone(),
            provider: entry.meta.provider.clone(),
            label: entry.meta.label.clone(),
            last4,
            key_present,
            added_at: entry.meta.added_at,
            last_used: entry.meta.last_used,
        }
    }

    pub async fn list_entries(&self, pool: &SqlitePool) -> Result<Vec<KeyEntryView>, KeyError> {
        let mut views = Vec::new();
        for entry in self.configured(pool).await? {
            views.push(self.view_of(&entry).await);
        }
        Ok(views)
    }

    /// Probes one slot: the request dials the slot's *base URL* (the
    /// endpoint), authenticated by the *account*'s key (the slot).
    async fn probe_entry(&self, entry: &ConfiguredKey, provider: Provider) -> KeyState {
        let loaded = {
            let account = entry.account.clone();
            self.store_blocking(move |store| store.load(&account)).await
        };
        let Ok(Some(key)) = loaded else {
            return KeyState::Unchecked;
        };
        let request = probe_request(provider, &entry.base_url, &key);
        let client = self.client.get_or_init(super::probe::probe_client);
        self.probes_run.fetch_add(1, Ordering::SeqCst);
        let state = match run_probe(client, &request).await {
            Ok(outcome) => {
                self.transport_failed
                    .lock()
                    .expect("transport lock poisoned")
                    .remove(&entry.account);
                classify(provider, outcome.status, &outcome.body)
            }
            Err(_) => {
                // A COMPLETED attempt with typed transport failure is
                // evidence (PF-PH1 Unreachable) — recorded beside the
                // unchanged four-state KeyState, never conflated with it.
                self.transport_failed
                    .lock()
                    .expect("transport lock poisoned")
                    .insert(entry.account.clone());
                KeyState::Unchecked
            }
        };
        self.set_state(&entry.account, state);
        state
    }

    pub async fn probe_all(&self, pool: &SqlitePool) -> Result<Vec<KeyEntryView>, KeyError> {
        for entry in self.configured(pool).await? {
            if let Some(provider) = Provider::from_id(&entry.meta.provider) {
                self.probe_entry(&entry, provider).await;
            }
        }
        self.list_entries(pool).await
    }

    pub async fn refresh_one(
        &self,
        pool: &SqlitePool,
        base_url: &str,
    ) -> Result<KeyEntryView, KeyError> {
        let entry = self
            .configured(pool)
            .await?
            .into_iter()
            .find(|entry| entry.account == base_url)
            .ok_or(KeyError::NotConfigured)?;
        if let Some(provider) = Provider::from_id(&entry.meta.provider) {
            self.probe_entry(&entry, provider).await;
        }
        Ok(self.view_of(&entry).await)
    }
}

/// The bulk-import receipt the settings panel renders: which lines became
/// slots (as their views), which were rejected and why (fixed reasons, line
/// numbers), and which were duplicates of keys already stored. No field can
/// carry key bytes — that is what makes it safe to show.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: Vec<KeyEntryView>,
    pub rejected: Vec<super::import::RejectedLine>,
    pub duplicates: Vec<DuplicateLine>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateLine {
    pub line: usize,
    pub label: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UrlCheck {
    pub canonical_url: String,
    pub consented: bool,
}

/// One recorded per-book reverse-anchor consent (VOICE_AGENT §11.1), as the
/// settings revocation surface lists it.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReverseAnchorConsent {
    pub book_id: String,
    pub base_url: String,
    pub recorded_at_ms: i64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CastPoolEntry {
    pub base_url: String,
    pub provider: String,
    pub state: KeyState,
    pub key_present: bool,
}

#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CastKeyPool {
    pub llm: Vec<CastPoolEntry>,
    pub tts: Vec<CastPoolEntry>,
}

impl KeyManager {
    pub async fn cast_key_pool(&self, pool: &SqlitePool) -> Result<CastKeyPool, KeyError> {
        use super::probe::Capability;
        let mut grouped = CastKeyPool::default();
        for entry in self.configured(pool).await? {
            let Some(provider) = Provider::from_id(&entry.meta.provider) else {
                continue;
            };
            let view = self.view_of(&entry).await;
            let entry = CastPoolEntry {
                base_url: view.base_url,
                provider: view.provider,
                state: view.state,
                key_present: view.key_present,
            };
            for capability in provider.capabilities() {
                match capability {
                    Capability::Llm => grouped.llm.push(entry.clone()),
                    Capability::Tts => grouped.tts.push(entry.clone()),
                    Capability::Stt => {}
                }
            }
        }
        Ok(grouped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::keys::store::Backend;

    const KEY: &str = "manager-secret-key-33cc";

    fn session_manager() -> KeyManager {
        KeyManager::with_store(KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        ))
    }

    #[tokio::test]
    async fn a_locked_setup_key_write_recovers_in_place_after_switching_to_session_storage() {
        use crate::voice::keys::vault::FileVault;
        let (dir, pool) = pool().await;
        let path = dir.path().join("keys.vault");
        let manager = KeyManager::with_store(KeyStore::with_forced_backend(
            path.clone(),
            Backend::FileLocked(FileVault::new(path)),
        ));
        let operation = uuid::Uuid::now_v7().to_string();
        assert!(matches!(
            manager
                .add_setup_key(
                    &pool,
                    "gemini",
                    None,
                    ApiKey::new(KEY.to_string()),
                    &operation
                )
                .await,
            Err(KeyError::Store(KeyStoreError::VaultLocked))
        ));
        assert_eq!(manager.list_entries(&pool).await.unwrap().len(), 1);
        manager.use_session_only().await.unwrap();
        let repaired = manager
            .add_setup_key(
                &pool,
                "gemini",
                None,
                ApiKey::new(KEY.to_string()),
                &operation,
            )
            .await
            .unwrap();
        assert!(repaired.key_present);
        assert_eq!(manager.list_entries(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn setup_key_retries_reuse_the_slot_and_repair_interrupted_secret_storage() {
        let manager = session_manager();
        let (_dir, pool) = pool().await;
        let operation = uuid::Uuid::now_v7().to_string();
        let first = manager
            .add_setup_key(
                &pool,
                "gemini",
                None,
                ApiKey::new(KEY.to_string()),
                &operation,
            )
            .await
            .unwrap();
        let account = first.account.clone();
        manager
            .store_blocking(move |store| store.remove(&account))
            .await
            .unwrap();
        assert!(!manager.list_entries(&pool).await.unwrap()[0].key_present);
        for _ in 0..2 {
            let repaired = manager
                .add_setup_key(
                    &pool,
                    "gemini",
                    None,
                    ApiKey::new(KEY.to_string()),
                    &operation,
                )
                .await
                .unwrap();
            assert_eq!(repaired.account, first.account);
            assert!(repaired.key_present);
        }
        assert_eq!(manager.list_entries(&pool).await.unwrap().len(), 1);
        assert!(matches!(
            manager
                .add_setup_key(
                    &pool,
                    "gemini",
                    None,
                    ApiKey::new("different-key".into()),
                    &operation
                )
                .await,
            Err(KeyError::OperationConflict)
        ));
        let rows: Vec<(String,)> = sqlx::query_as("SELECT value FROM app_setting")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(rows.iter().all(|(value,)| !value.contains(KEY)));
    }

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn official_openai_aliases_share_custody_without_crossing_other_providers() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .record_consent(&pool, "https://api.openai.com")
            .await
            .unwrap();
        manager
            .add_key(
                &pool,
                "openai_compat",
                Some("https://api.openai.com"),
                "fixture-openai-key-1234".to_string(),
                None,
            )
            .await
            .unwrap();
        assert!(manager
            .resolve_pool_key(&pool, "openai_compat", "https://api.openai.com/v1")
            .await
            .is_ok());
        assert!(manager
            .resolve_pool_key(&pool, "gemini", "https://api.openai.com/v1")
            .await
            .is_err());
        assert!(manager
            .resolve_pool_key(&pool, "openai_compat", "https://api.openai.com/proxy")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn adding_a_named_provider_key_uses_its_fixed_base_url() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let view = manager
            .add_key(&pool, "deepgram", None, KEY.into(), None)
            .await
            .unwrap();
        assert_eq!(view.base_url, "https://api.deepgram.com");
        assert_eq!(view.state, KeyState::Unchecked);
        assert_eq!(view.last4.as_deref(), Some("33cc"));
        assert!(!view.custom);
        assert!(
            matches!(
                manager
                    .add_key(
                        &pool,
                        "deepgram",
                        Some("https://evil.example"),
                        KEY.into(),
                        None
                    )
                    .await,
                Err(KeyError::BaseUrlFixed)
            ),
            "a custom endpoint must never be able to receive a named provider's key"
        );
    }

    #[tokio::test]
    async fn a_custom_base_url_requires_recorded_consent_before_the_key_is_accepted() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "https://api.example-llm.dev/v1";
        assert!(matches!(
            manager
                .add_key(&pool, "openai_compat", Some(url), KEY.into(), None)
                .await,
            Err(KeyError::ConsentRequired)
        ));
        manager.record_consent(&pool, url).await.unwrap();
        let view = manager
            .add_key(&pool, "openai_compat", Some(url), KEY.into(), None)
            .await
            .unwrap();
        assert!(view.custom);
        assert_eq!(view.base_url, url);
    }

    #[tokio::test]
    async fn consent_is_recorded_once_and_survives_re_checking() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "https://api.example-llm.dev/v1";
        assert!(!manager.check_url(&pool, url).await.unwrap().consented);
        manager.record_consent(&pool, url).await.unwrap();
        let first =
            settings::get_setting(&pool, "voice_url_consent2.https://api.example-llm.dev/v1")
                .await
                .unwrap()
                .unwrap();
        manager.record_consent(&pool, url).await.unwrap();
        let second =
            settings::get_setting(&pool, "voice_url_consent2.https://api.example-llm.dev/v1")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            first, second,
            "consent is per URL, once; re-recording must not refresh the timestamp as if the \
             user consented again"
        );
        assert!(manager.check_url(&pool, url).await.unwrap().consented);
    }

    #[tokio::test]
    async fn nothing_probes_without_settings_entry_or_explicit_refresh() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .add_key(&pool, "gemini", None, KEY.into(), None)
            .await
            .unwrap();
        manager.list_entries(&pool).await.unwrap();
        manager.storage_status().await;
        assert_eq!(
            manager.probes_run(),
            0,
            "adding, listing, and status checks are not the two spec-approved probe \
             triggers; any count here is ambient polling"
        );
    }

    #[tokio::test]
    async fn explicit_refresh_probes_exactly_the_named_key_and_counts_it() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        manager
            .add_key(&pool, "openai_compat", Some(url), KEY.into(), None)
            .await
            .unwrap();
        let view = manager.refresh_one(&pool, url).await.unwrap();
        assert_eq!(manager.probes_run(), 1);
        assert_eq!(
            view.state,
            KeyState::Unchecked,
            "a connection failure proves nothing about the key, so the state must stay \
             unchecked rather than scaring the user with `invalid`"
        );
    }

    #[tokio::test]
    async fn probe_states_are_memory_only_and_reset_with_the_process() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        manager
            .add_key(&pool, "openai_compat", Some(url), KEY.into(), None)
            .await
            .unwrap();
        manager.set_state(url, KeyState::Active);
        for (key, value) in settings::list_settings_with_prefix(&pool, "voice")
            .await
            .unwrap()
        {
            assert!(
                !value.contains("active") && !value.contains("Active"),
                "probe state leaked into SQLite under {key}: VOICE_AGENT §7 pins this state \
                 as memory-only"
            );
        }
        let fresh = KeyManager::with_store(KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        ));
        assert_eq!(fresh.state_of(url), KeyState::Unchecked);
    }

    #[tokio::test]
    async fn removing_a_key_clears_secret_metadata_and_state_but_keeps_consent() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "https://api.example-llm.dev/v1";
        manager.record_consent(&pool, url).await.unwrap();
        manager
            .add_key(&pool, "openai_compat", Some(url), KEY.into(), None)
            .await
            .unwrap();
        manager.remove_key(&pool, url, 0).await.unwrap();
        assert!(manager.list_entries(&pool).await.unwrap().is_empty());
        assert!(
            manager.check_url(&pool, url).await.unwrap().consented,
            "consent is once per URL; re-adding the same endpoint later must not re-prompt"
        );
        assert!(matches!(
            manager.remove_key(&pool, url, 0).await,
            Err(KeyError::NotConfigured)
        ));
    }

    #[tokio::test]
    async fn views_and_metadata_never_carry_key_bytes() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .add_key(&pool, "cartesia", None, KEY.into(), None)
            .await
            .unwrap();
        let views = manager.list_entries(&pool).await.unwrap();
        let serialized = serde_json::to_string(&views).unwrap();
        assert!(
            !serialized.contains("manager-secret"),
            "the view struct is what crosses IPC to the webview; it may carry last4 only"
        );
        for (key, value) in settings::list_settings_with_prefix(&pool, "voice")
            .await
            .unwrap()
        {
            assert!(
                !value.contains("manager-secret") && !key.contains("manager-secret"),
                "SQLite may hold which endpoints are configured, never what unlocks them"
            );
        }
    }

    #[tokio::test]
    async fn the_cast_pool_groups_keys_by_stage_capability_and_runs_zero_probes() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .add_key(&pool, "gemini", None, KEY.into(), None)
            .await
            .unwrap();
        manager
            .add_key(&pool, "cartesia", None, KEY.into(), None)
            .await
            .unwrap();
        manager
            .add_key(&pool, "deepgram", None, KEY.into(), None)
            .await
            .unwrap();
        manager.set_state(
            "https://generativelanguage.googleapis.com",
            KeyState::Active,
        );

        let cast_pool = manager.cast_key_pool(&pool).await.unwrap();
        assert_eq!(
            cast_pool
                .llm
                .iter()
                .map(|e| e.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["gemini"],
        );
        assert_eq!(cast_pool.llm[0].state, KeyState::Active);
        assert_eq!(
            cast_pool
                .tts
                .iter()
                .map(|e| e.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["cartesia"],
        );
        let everywhere = [&cast_pool.llm, &cast_pool.tts];
        assert!(
            everywhere
                .iter()
                .all(|arm| arm.iter().all(|e| e.provider != "deepgram")),
            "STT is a voice-session capability; the batch cast has no STT stage to unlock"
        );
        assert_eq!(
            manager.probes_run(),
            0,
            "the cast form reads cached states; probing from here would be a third trigger \
             outside settings entry and explicit refresh"
        );
    }

    #[tokio::test]
    async fn empty_and_unknown_inputs_are_rejected() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        assert!(matches!(
            manager
                .add_key(&pool, "gemini", None, "  ".into(), None)
                .await,
            Err(KeyError::EmptyKey)
        ));
        assert!(matches!(
            manager
                .add_key(&pool, "playht", None, KEY.into(), None)
                .await,
            Err(KeyError::UnknownProvider)
        ));
        assert!(matches!(
            manager
                .add_key(&pool, "openai_compat", None, KEY.into(), None)
                .await,
            Err(KeyError::BaseUrlRequired)
        ));
    }
    // ---- Key rotation S1: slot addressing (RCA §9) ----

    #[test]
    fn slot_accounts_round_trip_and_refuse_to_invent_slots() {
        let url = "https://generativelanguage.googleapis.com";
        assert_eq!(slot_account(url, 0), url, "slot 0 IS the pre-slot account");
        assert_eq!(slot_account(url, 2), format!("{url}#2"));
        assert_eq!(split_slot_account(url), (url, 0));
        assert_eq!(split_slot_account(&format!("{url}#2")), (url, 2));
        assert_eq!(split_slot_account(&format!("{url}#10")), (url, 10));
        // Suffixes that are not positive slot numbers stay part of the
        // account: guessing would alias a foreign row onto slot 0's key.
        for odd in ["acct#0", "acct#abc", "acct#-1", "#5", "acct#"] {
            assert_eq!(split_slot_account(odd), (odd, 0), "{odd:?} must not split");
        }
    }

    #[tokio::test]
    async fn a_pre_slot_metadata_row_still_parses_and_lists_as_slot_zero() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        // Exactly the JSON every pre-slot install has on disk: two fields.
        settings::set_setting(
            &pool,
            "voice_key.https://generativelanguage.googleapis.com",
            r#"{"provider":"gemini","addedAt":1756000000000}"#,
        )
        .await
        .unwrap();
        let views = manager.list_entries(&pool).await.unwrap();
        assert_eq!(
            views.len(),
            1,
            "the old row must not be dropped as malformed"
        );
        assert_eq!(views[0].slot, 0);
        assert_eq!(views[0].label, None);
        assert_eq!(views[0].last_used, None);
        assert_eq!(views[0].added_at, 1756000000000);
    }

    #[tokio::test]
    async fn slots_list_in_numeric_order_not_lexical() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "https://generativelanguage.googleapis.com";
        // Rows arrive in scrambled insert order, with #10 present to catch a
        // lexical sort ("#10" < "#2") — numeric order is the determinism the
        // rotation cursor stands on.
        for slot in [10u32, 2, 0] {
            let account = slot_account(url, slot);
            manager
                .store
                .store(&account, &ApiKey::new(format!("slot-key-00{slot}")))
                .unwrap();
            let meta = serde_json::json!({"provider": "gemini", "addedAt": 1 + slot as i64});
            settings::set_setting(
                &pool,
                &format!("{KEY_META_PREFIX}{account}"),
                &meta.to_string(),
            )
            .await
            .unwrap();
        }
        let views = manager.list_entries(&pool).await.unwrap();
        let slots: Vec<u32> = views.iter().map(|v| v.slot).collect();
        assert_eq!(slots, vec![0, 2, 10]);
        assert!(
            views.iter().all(|v| v.key_present && v.base_url == url),
            "every slot resolves its own store account under the one base URL"
        );
    }

    #[tokio::test]
    async fn adding_again_appends_a_new_slot_and_displaces_nothing() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let first = manager
            .add_key(&pool, "gemini", None, "first-key-11aa".into(), None)
            .await
            .unwrap();
        assert_eq!(first.slot, 0, "the first key IS the pre-slot account");
        let second = manager
            .add_key(
                &pool,
                "gemini",
                None,
                "second-key-22bb".into(),
                Some("  work "),
            )
            .await
            .unwrap();
        assert_eq!(second.slot, 1);
        assert_eq!(
            second.label.as_deref(),
            Some("work"),
            "labels are trimmed; whitespace is not identity"
        );
        let views = manager.list_entries(&pool).await.unwrap();
        assert_eq!(
            views.iter().map(|v| v.last4.clone()).collect::<Vec<_>>(),
            vec![Some("11aa".into()), Some("22bb".into())],
            "slot 0's bytes survive the append untouched"
        );
    }

    #[tokio::test]
    async fn removing_one_slot_leaves_the_others_and_never_reissues_its_number() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "https://generativelanguage.googleapis.com";
        for key in ["key-zero-00aa", "key-one-11bb", "key-two-22cc"] {
            manager
                .add_key(&pool, "gemini", None, key.into(), None)
                .await
                .unwrap();
        }
        manager.remove_key(&pool, url, 1).await.unwrap();
        let views = manager.list_entries(&pool).await.unwrap();
        assert_eq!(
            views.iter().map(|v| v.slot).collect::<Vec<_>>(),
            vec![0, 2],
            "exactly the named slot went away"
        );
        assert!(matches!(
            manager.remove_key(&pool, url, 1).await,
            Err(KeyError::NotConfigured)
        ));
        let next = manager
            .add_key(&pool, "gemini", None, "key-three-33dd".into(), None)
            .await
            .unwrap();
        assert_eq!(
            next.slot, 3,
            "allocation is max+1, never a freed low number while a higher slot lives — \
             a reissued number would inherit the dead slot's health mark"
        );
    }

    // ---- Key rotation S2: bulk import ----

    #[tokio::test]
    async fn a_pool_file_imports_as_labeled_untested_slots_and_reimports_as_duplicates() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let text = "# pool\nGEMINI_KEY_1=import-secret-aa11\nGEMINI_KEY_2=import-secret-bb22\nbroken line\n";
        let report = manager
            .import_keys(&pool, "gemini", None, text.into())
            .await
            .unwrap();
        assert_eq!(report.imported.len(), 2);
        assert_eq!(report.rejected.len(), 1);
        assert!(report.duplicates.is_empty());
        assert_eq!(report.imported[0].label.as_deref(), Some("GEMINI_KEY_1"));
        assert_eq!(report.imported[0].slot, 0);
        assert_eq!(report.imported[1].slot, 1);
        assert!(
            report
                .imported
                .iter()
                .all(|v| v.state == KeyState::Unchecked),
            "imported keys start untested; only a probe or a live dial may promote them"
        );

        // The same file again: every key is already stored, so nothing is
        // added and every line comes back as a duplicate receipt.
        let again = manager
            .import_keys(&pool, "gemini", None, text.into())
            .await
            .unwrap();
        assert!(again.imported.is_empty());
        assert_eq!(again.duplicates.len(), 2);
        assert_eq!(again.duplicates[0].label, "GEMINI_KEY_1");
        assert_eq!(
            manager.list_entries(&pool).await.unwrap().len(),
            2,
            "a double import must not mint ghost slots"
        );
    }

    #[tokio::test]
    async fn an_import_report_serializes_without_key_bytes() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let report = manager
            .import_keys(
                &pool,
                "gemini",
                None,
                "K1=import-secret-cc33\nK1_AGAIN=import-secret-cc33\nBAD=\n".into(),
            )
            .await
            .unwrap();
        assert_eq!(report.imported.len(), 1);
        assert_eq!(
            report.duplicates.len(),
            1,
            "an in-file repeat is a duplicate even on first import"
        );
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(
            !serialized.contains("import-secret"),
            "the report crosses IPC to the webview; a view may carry last4, never the bytes \
             before it: {serialized}"
        );
        assert!(
            serialized.contains("cc33"),
            "last4 is the one deliberate disclosure — its absence would mean the view \
             shape changed and the needle above went vacuous"
        );
    }

    #[tokio::test]
    async fn importing_for_a_custom_url_rides_the_same_consent_gate() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "https://api.example-llm.dev/v1";
        assert!(matches!(
            manager
                .import_keys(
                    &pool,
                    "openai_compat",
                    Some(url),
                    "K=import-secret-dd44".into()
                )
                .await,
            Err(KeyError::ConsentRequired)
        ));
        manager.record_consent(&pool, url).await.unwrap();
        let report = manager
            .import_keys(
                &pool,
                "openai_compat",
                Some(url),
                "K=import-secret-dd44".into(),
            )
            .await
            .unwrap();
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].base_url, url);
    }

    #[tokio::test]
    async fn importing_from_a_missing_file_names_the_kind_never_a_path_guess() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let err = manager
            .import_keys_from_file(
                &pool,
                "gemini",
                None,
                std::path::PathBuf::from("/nonexistent/pool.env"),
            )
            .await
            .unwrap_err();
        assert!(matches!(&err, KeyError::FileUnreadable(_)));
        assert!(format!("{err}").contains("could not be read"));
    }

    #[tokio::test]
    async fn importing_from_a_real_file_lands_the_same_slots_as_text() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.env");
        std::fs::write(&path, "FILE_KEY_1=import-secret-ee55\n").unwrap();
        let report = manager
            .import_keys_from_file(&pool, "gemini", None, path)
            .await
            .unwrap();
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].label.as_deref(), Some("FILE_KEY_1"));
        assert_eq!(report.imported[0].last4.as_deref(), Some("ee55"));
    }

    #[tokio::test]
    async fn metadata_write_failure_cleans_up_stored_key() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        sqlx::query("DROP TABLE app_setting")
            .execute(&pool)
            .await
            .unwrap();
        let canonical = "https://generativelanguage.googleapis.com";
        let err = manager
            .add_key(&pool, "gemini", None, "orphaned-secret-1234".into(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Db(_)));
        let account = slot_account(canonical, 0);
        let stored = manager
            .store_blocking(move |store| store.load(&account))
            .await
            .unwrap();
        assert!(
            stored.is_none(),
            "key material must be removed from store when metadata write fails"
        );
    }

    #[tokio::test]
    async fn import_keys_aborts_if_existing_store_read_fails() {
        use crate::voice::keys::store::KeyStoreError;
        use crate::voice::keys::vault::FileVault;
        let (dir, pool) = pool().await;
        let vault_path = dir.path().join("keys.vault");
        let canonical = "https://generativelanguage.googleapis.com";
        let account = slot_account(canonical, 0);
        let meta = KeyMeta {
            provider: "gemini".into(),
            added_at: now_ms(),
            label: Some("KEY_1".into()),
            last_used: None,
        };
        let meta_json = serde_json::to_string(&meta).unwrap();
        settings::set_setting(&pool, &format!("{KEY_META_PREFIX}{account}"), &meta_json)
            .await
            .unwrap();

        let manager = KeyManager::with_store(KeyStore::with_forced_backend(
            vault_path.clone(),
            Backend::FileLocked(FileVault::new(vault_path)),
        ));

        let err = manager
            .import_keys(&pool, "gemini", None, "KEY_1=new-secret-1234\n".into())
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Store(KeyStoreError::VaultLocked)));
    }

    // ---- Key rotation S3: round-robin resolution and dial marks ----

    const GEMINI_URL: &str = "https://generativelanguage.googleapis.com";

    async fn manager_with_gemini_keys(n: usize) -> (tempfile::TempDir, SqlitePool, KeyManager) {
        let (dir, pool) = pool().await;
        let manager = session_manager();
        for i in 0..n {
            manager
                .add_key(
                    &pool,
                    "gemini",
                    None,
                    format!("rotation-key-{i}-x{i}{i}"),
                    Some(&format!("KEY_{i}")),
                )
                .await
                .unwrap();
        }
        (dir, pool, manager)
    }

    #[tokio::test]
    async fn tutor_readiness_uses_the_live_pool_without_advancing_or_exposing_keys() {
        let (_dir, pool, manager) = manager_with_gemini_keys(3).await;
        manager.store.remove(GEMINI_URL).unwrap();
        manager
            .set_slot_disabled(&pool, &slot_account(GEMINI_URL, 1), true)
            .await
            .unwrap();
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Present
        );
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Present
        );
        let candidate = manager
            .resolve_pool_key(&pool, "gemini", GEMINI_URL)
            .await
            .unwrap();
        assert_eq!(candidate.slot, 2);
        assert_eq!(manager.take_dial_success(), None);
        assert!(manager
            .list_entries(&pool)
            .await
            .unwrap()
            .iter()
            .all(|entry| entry.last_used.is_none()));
        manager.record_dial_auth_failure(&candidate.account, None);
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Unavailable
        );
        manager.record_dial_quota(&candidate.account, None, Some(3600));
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Unavailable
        );
        manager.record_dial_quota(&candidate.account, None, Some(0));
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Present
        );
        assert_eq!(
            serde_json::to_string(&TutorKeyReadiness::Present).unwrap(),
            "\"present\""
        );
    }

    #[tokio::test]
    async fn tutor_readiness_distinguishes_missing_locked_and_wrong_provider() {
        use crate::voice::keys::vault::FileVault;
        let (dir, pool) = pool().await;
        let manager = session_manager();
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Missing
        );
        manager.record_consent(&pool, GEMINI_URL).await.unwrap();
        manager
            .add_key(&pool, "openai_compat", Some(GEMINI_URL), KEY.into(), None)
            .await
            .unwrap();
        assert_eq!(
            manager.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Missing
        );
        manager
            .add_key(&pool, "gemini", None, "gemini-fixture-1234".into(), None)
            .await
            .unwrap();
        let path = dir.path().join("locked.vault");
        let locked = KeyManager::with_store(KeyStore::with_forced_backend(
            path.clone(),
            Backend::FileLocked(FileVault::new(path)),
        ));
        assert_eq!(
            locked.tutor_readiness(&pool).await.unwrap(),
            TutorKeyReadiness::Locked
        );
    }

    #[tokio::test]
    async fn rotation_is_round_robin_in_slot_order_across_successful_starts() {
        let (_dir, pool, manager) = manager_with_gemini_keys(3).await;
        let mut dialed = Vec::new();
        for _ in 0..4 {
            let ring = manager
                .resolve_rotation(&pool, GEMINI_URL, 3)
                .await
                .unwrap();
            dialed.push(ring[0].slot);
            manager.record_dial_success(&ring[0].account);
        }
        assert_eq!(
            dialed,
            vec![0, 1, 2, 0],
            "round-robin over live/untested keys, wrapping — THE rotation pin"
        );
    }

    #[tokio::test]
    async fn rotation_skips_dead_and_quota_slots_and_bounds_the_ring() {
        let (_dir, pool, manager) = manager_with_gemini_keys(5).await;
        manager.record_dial_auth_failure(&slot_account(GEMINI_URL, 1), Some(1007));
        manager.record_dial_quota(&slot_account(GEMINI_URL, 2), Some(1011), None);
        let ring = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(
            ring.iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![0, 3, 4],
            "dead slot 1 and quota-exhausted slot 2 never enter the ring"
        );
        assert_eq!(ring[1].display_name(), "KEY_3");
        let bounded = manager
            .resolve_rotation(&pool, GEMINI_URL, 2)
            .await
            .unwrap();
        assert_eq!(bounded.len(), 2, "the per-start candidate cap holds");
    }

    #[tokio::test]
    async fn when_every_key_is_marked_unusable_the_error_says_so_with_the_count() {
        let (_dir, pool, manager) = manager_with_gemini_keys(2).await;
        manager.record_dial_auth_failure(&slot_account(GEMINI_URL, 0), Some(1007));
        manager.record_dial_quota(&slot_account(GEMINI_URL, 1), None, None);
        let err = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::NoUsableKey { stored: 2 }));
        let rendered = format!("{err}");
        assert!(
            rendered.contains("all 2") && rendered.contains("rate limited"),
            "the honest-failure text names how many keys exist and why none can dial: \
             {rendered}"
        );
    }

    #[tokio::test]
    async fn a_quota_mark_with_an_elapsed_retry_after_reenters_rotation() {
        let (_dir, pool, manager) = manager_with_gemini_keys(1).await;
        let account = slot_account(GEMINI_URL, 0);
        manager.record_dial_quota(&account, Some(1011), None);
        assert!(
            matches!(
                manager.resolve_rotation(&pool, GEMINI_URL, 3).await,
                Err(KeyError::NoUsableKey { .. })
            ),
            "without a provider deadline the slot stays out for this process"
        );
        manager.record_dial_quota(&account, Some(1011), Some(0));
        let ring = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(
            ring[0].slot, 0,
            "a provider-given retry-after that has passed puts the slot back in"
        );
    }

    #[tokio::test]
    async fn a_dial_success_promotes_the_key_and_hands_over_exactly_one_last_used_cue() {
        let (_dir, pool, manager) = manager_with_gemini_keys(1).await;
        let account = slot_account(GEMINI_URL, 0);
        assert_eq!(manager.take_dial_success(), None);
        manager.record_dial_auth_failure(&account, Some(1007));
        manager.record_dial_success(&account);
        assert_eq!(manager.state_of(&account), KeyState::Active);
        assert_eq!(
            manager.take_dial_success().as_deref(),
            Some(account.as_str())
        );
        assert_eq!(
            manager.take_dial_success(),
            None,
            "the cue is take-once; a second reader must not double-write last_used"
        );
        manager.note_last_used(&pool, &account).await.unwrap();
        let views = manager.list_entries(&pool).await.unwrap();
        assert!(
            views[0].last_used.is_some(),
            "last_used persists on the metadata row"
        );
        assert_eq!(views[0].state, KeyState::Active);
    }

    #[tokio::test]
    async fn removing_every_slot_clears_dial_marks_so_a_reissued_number_starts_clean() {
        let (_dir, pool, manager) = manager_with_gemini_keys(1).await;
        let account = slot_account(GEMINI_URL, 0);
        manager.record_dial_auth_failure(&account, Some(1007));
        manager.remove_key(&pool, GEMINI_URL, 0).await.unwrap();
        let fresh = manager
            .add_key(&pool, "gemini", None, "fresh-key-99zz".into(), None)
            .await
            .unwrap();
        assert_eq!(
            fresh.slot, 0,
            "numbering restarts once the endpoint is empty"
        );
        assert_eq!(
            manager.state_of(&account),
            KeyState::Unchecked,
            "the dead-era mark must not taint the fresh key inheriting the number"
        );
        let ring = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(ring.len(), 1);
    }

    // ---- Key rotation: Role B adversarial pins (G3 import) ----

    #[tokio::test]
    async fn two_lines_with_the_same_name_but_different_keys_both_import() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        // Dedup is by KEY MATERIAL, never by name: a pool file that reuses
        // an env-var name for two different keys yields two slots wearing
        // the same label — losing either would silently shrink the pool.
        let report = manager
            .import_keys(
                &pool,
                "gemini",
                None,
                "TWIN=twin-key-aa11\nTWIN=twin-key-bb22\n".into(),
            )
            .await
            .unwrap();
        assert_eq!(report.imported.len(), 2);
        assert!(report.duplicates.is_empty());
        assert_eq!(
            report
                .imported
                .iter()
                .map(|v| (v.label.as_deref(), v.slot))
                .collect::<Vec<_>>(),
            vec![(Some("TWIN"), 0), (Some("TWIN"), 1)],
            "labels are display, not identity"
        );
    }

    #[tokio::test]
    async fn a_thousand_key_file_imports_with_an_exact_receipt_and_reimports_as_pure_duplicates() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let mut text = String::from("# big pool\n");
        for i in 0..1000 {
            text.push_str(&format!("POOL_KEY_{i:04}=pool-secret-{i:04}\n"));
        }
        text.push_str("BROKEN LINE=x\n");
        let report = manager
            .import_keys(&pool, "gemini", None, text.clone())
            .await
            .unwrap();
        assert_eq!(
            (
                report.imported.len(),
                report.rejected.len(),
                report.duplicates.len()
            ),
            (1000, 1, 0),
            "the receipt counts are exact at pool scale"
        );
        assert_eq!(report.imported[999].slot, 999);
        assert_eq!(report.rejected[0].line, 1002, "line numbers stay exact");

        let again = manager
            .import_keys(&pool, "gemini", None, text)
            .await
            .unwrap();
        assert_eq!(
            (again.imported.len(), again.duplicates.len()),
            (0, 1000),
            "re-importing the same pool mints zero ghost slots"
        );
        assert_eq!(again.duplicates[0].line, 2);
        assert_eq!(again.duplicates[0].label, "POOL_KEY_0000");
        assert_eq!(again.duplicates[999].line, 1001);
        assert_eq!(
            manager.list_entries(&pool).await.unwrap().len(),
            1000,
            "exactly the imported slots exist"
        );
    }

    // ---- Key rotation: Role B adversarial pins (G1/G2) ----

    #[tokio::test]
    async fn a_future_quota_retry_deadline_keeps_the_slot_out_until_it_elapses() {
        let (_dir, pool, manager) = manager_with_gemini_keys(2).await;
        let account = slot_account(GEMINI_URL, 0);
        // An hour-long provider deadline: Role A pinned that an ELAPSED
        // deadline re-admits (Some(0)) and that NO deadline stays out — this
        // is the missing half: a deadline still in the future must keep the
        // slot out of rotation, not re-admit it the moment a mark exists.
        manager.record_dial_quota(&account, Some(1011), Some(3600));
        assert_eq!(manager.state_of(&account), KeyState::OutOfQuota);
        let ring = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(
            ring.iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![1],
            "a quota slot whose retry deadline has not passed must stay out of the ring"
        );
    }

    #[tokio::test]
    async fn a_legacy_pre_slot_row_resolves_as_the_rings_only_candidate() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        // Byte-for-byte what a pre-slot install has: bare-URL store account
        // plus the two-field metadata JSON. 15+ fixture tests stand on this
        // shape continuing to resolve identically.
        manager
            .store
            .store(GEMINI_URL, &ApiKey::new("legacy-key-77gg".to_string()))
            .unwrap();
        settings::set_setting(
            &pool,
            "voice_key.https://generativelanguage.googleapis.com",
            r#"{"provider":"gemini","addedAt":1756000000000}"#,
        )
        .await
        .unwrap();
        let ring = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(ring.len(), 1);
        assert_eq!(
            ring[0].account, GEMINI_URL,
            "the legacy row's candidate account IS the bare URL — the store lookup \
             must hit the pre-slot keyring entry unchanged"
        );
        assert_eq!(ring[0].slot, 0);
        assert_eq!(
            ring[0].key.last4(),
            "77gg",
            "the pre-slot key bytes resolve through the ring untouched"
        );
        assert_eq!(
            ring[0].display_name(),
            "slot 0",
            "an unlabeled legacy row is named by slot number in failure text"
        );
    }

    #[tokio::test]
    async fn resolution_is_stable_until_a_success_moves_the_cursor() {
        let (_dir, pool, manager) = manager_with_gemini_keys(3).await;
        let first = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        let second = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(
            first.iter().map(|c| c.slot).collect::<Vec<_>>(),
            second.iter().map(|c| c.slot).collect::<Vec<_>>(),
            "resolving is read-only: two starts that resolve before either dials get \
             the same ring — only a SUCCESS moves the cursor, never a resolve"
        );
        manager.record_dial_success(&first[0].account);
        let third = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(
            third.iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![1, 2, 0],
            "after the success the ring starts past the winning slot"
        );
    }

    #[tokio::test]
    async fn two_successes_before_a_take_leave_one_cue_the_latest() {
        let (_dir, pool, manager) = manager_with_gemini_keys(2).await;
        let first = slot_account(GEMINI_URL, 0);
        let second = slot_account(GEMINI_URL, 1);
        // The mutex story as built: the cue is a single Option slot, so when
        // two session starts both succeed before either writes last_used,
        // the later success OVERWRITES the earlier cue — exactly one
        // last_used write happens, for the latest dial. Changing this to a
        // queue is fine; losing the take-once discipline is not.
        manager.record_dial_success(&first);
        manager.record_dial_success(&second);
        assert_eq!(
            manager.take_dial_success().as_deref(),
            Some(second.as_str())
        );
        assert_eq!(
            manager.take_dial_success(),
            None,
            "the cue is take-once even after two successes — a second reader must \
             never double-write last_used"
        );
        let ring = manager
            .resolve_rotation(&pool, GEMINI_URL, 3)
            .await
            .unwrap();
        assert_eq!(
            ring[0].slot, 0,
            "the cursor sits past the LAST success (slot 1), wrapping to 0"
        );
    }

    #[tokio::test]
    async fn disabled_slots_persist_and_leave_rotation_without_touching_stored_data() {
        // A28/PF-PH1: operator-disable survives a manager restart (the
        // setting is durable), removes the slot from rotation, and never
        // mutates the stored key, label, or account shape.
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        for key in ["disable-key-00aa", "disable-key-11bb"] {
            manager
                .add_key(&pool, "openai_compat", Some(url), key.into(), None)
                .await
                .unwrap();
        }
        let account = slot_account(url, 1);
        manager
            .set_slot_disabled(&pool, &account, true)
            .await
            .unwrap();

        // Rotation skips the disabled slot.
        let candidates = manager.resolve_rotation(&pool, url, 3).await.unwrap();
        assert!(candidates.iter().all(|c| c.account != account));

        // A fresh manager (new memory state) still sees the disable.
        let reloaded = session_manager();
        assert!(reloaded.slot_disabled(&pool, &account).await);

        // Stored entries are untouched: same accounts, keys present.
        let entries = manager.list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.key_present));

        // Re-enable restores rotation membership.
        manager
            .set_slot_disabled(&pool, &account, false)
            .await
            .unwrap();
        let candidates = manager.resolve_rotation(&pool, url, 3).await.unwrap();
        assert!(candidates.iter().any(|c| c.account == account));
    }

    #[tokio::test]
    async fn pool_key_resolution_is_provider_checked() {
        // PF-PH1 custody: a slot stored under another provider id never
        // resolves, however the URLs coincide.
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        manager
            .add_key(
                &pool,
                "openai_compat",
                Some(url),
                "right-key-00aa".into(),
                None,
            )
            .await
            .unwrap();
        let resolved = manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .unwrap();
        assert_eq!(resolved.base_url, url);
        assert!(matches!(
            manager.resolve_pool_key(&pool, "anthropic", url).await,
            Err(KeyError::NotConfigured)
        ));
    }

    /// S-5 blocker 5, pool-scale rotation: settled outcomes drive the
    /// cursor and the eligibility set — success advances past the slot,
    /// an auth refusal removes it, a quota refusal removes it until the
    /// provider-given retry deadline elapses.
    #[tokio::test]
    async fn pool_outcomes_advance_the_cursor_and_demote_failing_slots() {
        use crate::providers::registry::{ScriptError, ScriptErrorKind};
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        for key in ["rotate-key-00aa", "rotate-key-11bb", "rotate-key-22cc"] {
            manager
                .add_key(&pool, "openai_compat", Some(url), key.into(), None)
                .await
                .unwrap();
        }

        // Success advances the round-robin: the next dial gets slot 1.
        let first = manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .unwrap();
        assert_eq!(first.slot, 0);
        manager.record_pool_outcome(&first.account, Ok(()));
        let second = manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .unwrap();
        assert_eq!(second.slot, 1, "success moved the cursor past slot 0");

        // An auth refusal demotes the slot out of rotation entirely.
        manager.record_pool_outcome(
            &second.account,
            Err(&ScriptError {
                kind: ScriptErrorKind::Auth,
                message: "401".to_string(),
                retry_after_secs: None,
                retryable: false,
                status: Some(401),
            }),
        );
        let third = manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .unwrap();
        assert_eq!(third.slot, 2, "the auth-dead slot is skipped");

        // A quota refusal with a retry deadline removes the slot for now.
        manager.record_pool_outcome(
            &third.account,
            Err(&ScriptError {
                kind: ScriptErrorKind::Quota,
                message: "429".to_string(),
                retry_after_secs: Some(3600),
                retryable: true,
                status: Some(429),
            }),
        );
        let after = manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .unwrap();
        assert_eq!(
            after.slot, 0,
            "with slots 1 and 2 demoted, rotation wraps to the working slot"
        );

        // Transport says nothing about the KEY: no demotion.
        manager.record_pool_outcome(
            &after.account,
            Err(&ScriptError {
                kind: ScriptErrorKind::Transport,
                message: "dns".to_string(),
                retry_after_secs: None,
                retryable: true,
                status: None,
            }),
        );
        assert!(manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .is_ok());

        // The batch success path never touches the Voice Tutor's
        // last_used cue.
        assert_eq!(manager.take_dial_success(), None);
    }

    /// S-5 blocker 6: the pool-test registry is the one Checking truth —
    /// typed progress, a per-probe current account, and a cooperative
    /// cancel that stops the run at the next probe boundary.
    #[tokio::test]
    async fn pool_test_registry_reports_progress_and_cancels_cooperatively() {
        let manager = session_manager();
        let (provider, url) = ("gemini", "https://generativelanguage.googleapis.com");
        assert!(manager.pool_test_status(provider, url).is_none());
        assert!(manager.begin_pool_test(provider, url, 2));
        // A second concurrent run for the SAME pool is refused and touches
        // nothing (re-verdict item 1: no overwrite of the shared entry).
        assert!(
            !manager.begin_pool_test(provider, url, 9),
            "a running pool test must not be clobbered"
        );
        assert_eq!(
            manager.pool_test_status(provider, url).unwrap().total,
            2,
            "the refused begin left the first run's truth intact"
        );
        assert!(
            manager.begin_pool_test(provider, "https://other.example", 1),
            "a DIFFERENT pool runs independently"
        );
        manager.finish_pool_test(provider, "https://other.example");
        assert!(manager.update_pool_test(provider, url, 0, Some("acct-0")));
        let status = manager.pool_test_status(provider, url).unwrap();
        assert_eq!((status.completed, status.total), (0, 2));
        assert_eq!(status.current_account.as_deref(), Some("acct-0"));
        manager.cancel_pool_test(provider, url);
        assert!(
            !manager.update_pool_test(provider, url, 1, Some("acct-1")),
            "the runner learns the cancel at the next boundary"
        );
        manager.finish_pool_test(provider, url);
        assert!(manager.pool_test_status(provider, url).is_none());
        // Cancelling a finished (absent) test is a harmless no-op.
        manager.cancel_pool_test(provider, url);
    }

    /// Re-verdict item 2: a removed slot leaves NO ghost state — neither
    /// the persisted operator-disable nor the memory-only transport mark
    /// may taint a fresh key that later inherits the slot number.
    #[tokio::test]
    async fn removing_a_slot_clears_disabled_and_transport_ghost_state() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        let view = manager
            .add_key(
                &pool,
                "openai_compat",
                Some(url),
                "ghost-key-00aa".into(),
                None,
            )
            .await
            .unwrap();
        manager
            .set_slot_disabled(&pool, &view.account, true)
            .await
            .unwrap();
        manager
            .transport_failed
            .lock()
            .unwrap()
            .insert(view.account.clone());

        manager.remove_key(&pool, url, 0).await.unwrap();
        assert!(
            !manager.slot_disabled(&pool, &view.account).await,
            "the persisted disable died with the slot"
        );
        assert!(!manager.transport_failed_for(&view.account));

        // A fresh key inheriting slot 0 starts clean: Unchecked, enabled,
        // dial-eligible.
        let readded = manager
            .add_key(
                &pool,
                "openai_compat",
                Some(url),
                "fresh-key-11bb".into(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(readded.account, view.account, "slot 0 was inherited");
        assert_eq!(readded.state, KeyState::Unchecked);
        assert!(!manager.slot_disabled(&pool, &readded.account).await);
        assert!(!manager.transport_failed_for(&readded.account));
        let resolved = manager
            .resolve_pool_key(&pool, "openai_compat", url)
            .await
            .expect("the fresh slot is dial-eligible");
        assert_eq!(resolved.account, readded.account);
    }

    #[tokio::test]
    async fn refresh_one_addresses_a_slot_qualified_account() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        for key in ["refresh-key-00aa", "refresh-key-11bb"] {
            manager
                .add_key(&pool, "openai_compat", Some(url), key.into(), None)
                .await
                .unwrap();
        }
        // The command arg is still NAMED base_url, but what the panel passes
        // (and what the lookup matches) is the slot-qualified ACCOUNT — pin
        // the behavior so renaming the arg later is a no-op, and so a
        // "helpful" future canonicalization of the arg cannot break slot >= 1
        // refreshes silently.
        let view = manager
            .refresh_one(&pool, &slot_account(url, 1))
            .await
            .unwrap();
        assert_eq!(view.slot, 1);
        assert_eq!(view.last4.as_deref(), Some("11bb"));
        assert_eq!(manager.probes_run(), 1, "exactly the named slot probed");
        let bare = manager.refresh_one(&pool, url).await.unwrap();
        assert_eq!(bare.slot, 0, "the bare URL still addresses slot 0");
        assert!(matches!(
            manager.refresh_one(&pool, &slot_account(url, 9)).await,
            Err(KeyError::NotConfigured)
        ));
    }

    #[tokio::test]
    async fn settings_entry_probes_every_slot_once_each() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        for key in ["fanout-key-00aa", "fanout-key-11bb", "fanout-key-22cc"] {
            manager
                .add_key(&pool, "openai_compat", Some(url), key.into(), None)
                .await
                .unwrap();
        }
        manager.probe_all(&pool).await.unwrap();
        assert_eq!(
            manager.probes_run(),
            3,
            "probe_all is one HTTP probe PER SLOT — with a 47-key pool, settings \
             entry costs 47 probes. Pinned as-built; if a debounce/limit lands, \
             re-pin this deliberately"
        );
    }

    #[tokio::test]
    async fn a_process_restart_forgets_rotation_memory_but_keeps_the_slots() {
        let (_dir, pool, manager) = manager_with_gemini_keys(2).await;
        // A session ran: cursor moved, a quota mark with a future deadline
        // landed on slot 1.
        manager.record_dial_success(&slot_account(GEMINI_URL, 0));
        manager.record_dial_quota(&slot_account(GEMINI_URL, 1), Some(1011), Some(3600));
        // "Restart": a fresh manager over the SAME settings pool (SQLite
        // survives) with re-unlocked key material — cursor, health, close
        // codes, and quota deadlines are all memory-only (§7) and must die
        // with the process.
        let fresh = session_manager();
        for slot in [0u32, 1] {
            fresh
                .store
                .store(
                    &slot_account(GEMINI_URL, slot),
                    &ApiKey::new(format!("rotation-key-{slot}-x{slot}{slot}")),
                )
                .unwrap();
        }
        let views = fresh.list_entries(&pool).await.unwrap();
        assert_eq!(views.len(), 2, "the slots themselves persist in SQLite");
        assert!(
            views.iter().all(|v| v.state == KeyState::Unchecked),
            "every slot reads Not-yet-used after restart — health is memory-only"
        );
        let ring = fresh.resolve_rotation(&pool, GEMINI_URL, 3).await.unwrap();
        assert_eq!(
            ring.iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![0, 1],
            "the cursor and the quota deadline died with the process: rotation \
             restarts at slot 0 with every slot eligible"
        );
    }

    // ---- VA6 U4: the per-book reverse-anchor consent gate (Role B pins) ----

    const HOST: &str = "https://api.example.com";

    #[tokio::test]
    async fn reverse_anchor_consent_round_trips_record_check_list_revoke() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();

        let before = manager
            .check_reverse_anchor_consent(&pool, "book-a", HOST)
            .await
            .unwrap();
        assert!(!before.consented, "nothing is consented until recorded");

        let canonical = manager
            .record_reverse_anchor_consent(&pool, "book-a", "https://API.Example.com/")
            .await
            .unwrap();
        assert_eq!(canonical, HOST, "the recorded key is the canonical form");

        let after = manager
            .check_reverse_anchor_consent(&pool, "book-a", HOST)
            .await
            .unwrap();
        assert!(
            after.consented,
            "THE gate: recorded consent answers the tap path"
        );

        let rows = manager.list_reverse_anchor_consents(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].book_id, "book-a");
        assert_eq!(rows[0].base_url, HOST);
        assert!(rows[0].recorded_at_ms > 0);

        manager
            .revoke_reverse_anchor_consent(&pool, "book-a", "https://api.example.com/")
            .await
            .unwrap();
        let revoked = manager
            .check_reverse_anchor_consent(&pool, "book-a", HOST)
            .await
            .unwrap();
        assert!(
            !revoked.consented,
            "revocation restores the never-consented state: the next tap asks again"
        );
        assert!(manager
            .list_reverse_anchor_consents(&pool)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn two_spellings_of_one_endpoint_are_one_consent_recorded_once() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .record_reverse_anchor_consent(&pool, "book-a", "https://API.Example.com")
            .await
            .unwrap();
        let first = manager.list_reverse_anchor_consents(&pool).await.unwrap();

        manager
            .record_reverse_anchor_consent(&pool, "book-a", "https://api.example.com/")
            .await
            .unwrap();
        let second = manager.list_reverse_anchor_consents(&pool).await.unwrap();
        assert_eq!(second.len(), 1, "one endpoint, one row, however spelled");
        assert_eq!(
            second[0].recorded_at_ms, first[0].recorded_at_ms,
            "asked once, recorded once: a re-record never refreshes the timestamp"
        );
    }

    #[tokio::test]
    async fn consent_is_per_book_and_never_answers_another_book() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .record_reverse_anchor_consent(&pool, "book-a", HOST)
            .await
            .unwrap();

        let other = manager
            .check_reverse_anchor_consent(&pool, "book-b", HOST)
            .await
            .unwrap();
        assert!(
            !other.consented,
            "consent for book A must never authorize book B (§11.1 per-book floor)"
        );

        // Revoking under the wrong book removes nothing.
        manager
            .revoke_reverse_anchor_consent(&pool, "book-b", HOST)
            .await
            .unwrap();
        assert!(
            manager
                .check_reverse_anchor_consent(&pool, "book-a", HOST)
                .await
                .unwrap()
                .consented,
            "book A's consent survives a revoke addressed to book B"
        );
    }

    #[tokio::test]
    async fn the_cascade_prefix_drops_exactly_one_books_rows() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .record_reverse_anchor_consent(&pool, "book-a", HOST)
            .await
            .unwrap();
        manager
            .record_reverse_anchor_consent(&pool, "book-a", "https://other.example.net")
            .await
            .unwrap();
        manager
            .record_reverse_anchor_consent(&pool, "book-b", HOST)
            .await
            .unwrap();
        // The id-extension sibling: "book-a2" starts with "book-a" and must
        // survive book-a's cascade — the prefix ends in '.' precisely so a
        // LIKE pattern cannot bleed across ids.
        manager
            .record_reverse_anchor_consent(&pool, "book-a2", HOST)
            .await
            .unwrap();

        crate::storage::settings::delete_settings_with_prefix(
            &pool,
            &reverse_anchor_consent_prefix_for_book("book-a"),
        )
        .await
        .unwrap();

        let rows = manager.list_reverse_anchor_consents(&pool).await.unwrap();
        let mut books: Vec<&str> = rows.iter().map(|row| row.book_id.as_str()).collect();
        books.sort_unstable();
        assert_eq!(
            books,
            vec!["book-a2", "book-b"],
            "the deleted book's consents are gone; every other book's survive"
        );
    }

    #[tokio::test]
    async fn listing_skips_malformed_rows_rather_than_inventing_them() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        manager
            .record_reverse_anchor_consent(&pool, "book-a", HOST)
            .await
            .unwrap();
        // A row with no '.' after the book id, and one whose value is not a
        // timestamp: both under the prefix, both unparseable, both skipped.
        settings::set_setting(&pool, "voice_reverse_anchor_consent.orphan", "123")
            .await
            .unwrap();
        settings::set_setting(
            &pool,
            "voice_reverse_anchor_consent.book-x.https://x.example",
            "not-a-timestamp",
        )
        .await
        .unwrap();
        // A neighbouring consent family is not this list's business.
        settings::set_setting(&pool, "voice_url_consent.https://y.example", "456")
            .await
            .unwrap();

        let rows = manager.list_reverse_anchor_consents(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].book_id, "book-a");
    }

    #[tokio::test]
    async fn an_empty_or_dotted_book_id_is_refused_on_every_method() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        for book_id in ["", "book.with.dots"] {
            assert!(
                matches!(
                    manager
                        .record_reverse_anchor_consent(&pool, book_id, HOST)
                        .await,
                    Err(KeyError::InvalidBookId)
                ),
                "record must refuse book id {book_id:?}"
            );
            assert!(matches!(
                manager
                    .check_reverse_anchor_consent(&pool, book_id, HOST)
                    .await,
                Err(KeyError::InvalidBookId)
            ));
            assert!(matches!(
                manager
                    .revoke_reverse_anchor_consent(&pool, book_id, HOST)
                    .await,
                Err(KeyError::InvalidBookId)
            ));
        }
        assert!(
            manager
                .list_reverse_anchor_consents(&pool)
                .await
                .unwrap()
                .is_empty(),
            "the refused calls recorded nothing"
        );
    }

    #[tokio::test]
    async fn reverse_anchor_urls_ride_the_same_policy_as_every_other_endpoint() {
        let (_dir, pool) = pool().await;
        let manager = session_manager();
        for bad in ["ftp://x.example", "https://u:p@x.example", "not a url"] {
            assert!(
                matches!(
                    manager
                        .record_reverse_anchor_consent(&pool, "book-a", bad)
                        .await,
                    Err(KeyError::Policy(_))
                ),
                "{bad:?} must be refused by URL policy"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_slot_disabled_toggles_preserve_all_mutations() {
        let (_dir, pool) = pool().await;
        let manager = Arc::new(session_manager());
        let url = "http://127.0.0.1:1";
        manager.record_consent(&pool, url).await.unwrap();
        let e1 = manager
            .add_key(
                &pool,
                "openai_compat",
                Some(url),
                "concurrent-0".into(),
                None,
            )
            .await
            .unwrap();
        let e2 = manager
            .add_key(
                &pool,
                "openai_compat",
                Some(url),
                "concurrent-1".into(),
                None,
            )
            .await
            .unwrap();
        let account0 = e1.account;
        let account1 = e2.account;

        let m1 = Arc::clone(&manager);
        let p1 = pool.clone();
        let a0 = account0.clone();
        let t1 = tokio::spawn(async move {
            m1.set_slot_disabled(&p1, &a0, true).await.unwrap();
        });

        let m2 = Arc::clone(&manager);
        let p2 = pool.clone();
        let a1 = account1.clone();
        let t2 = tokio::spawn(async move {
            m2.set_slot_disabled(&p2, &a1, true).await.unwrap();
        });

        let (r1, r2) = tokio::join!(t1, t2);
        r1.unwrap();
        r2.unwrap();

        assert!(manager.slot_disabled(&pool, &account0).await);
        assert!(manager.slot_disabled(&pool, &account1).await);
    }
}
