//! Per-first-use-per-book cast consent records (charter §5.1, A13/A14,
//! T13; Hermes S-4 pre-brief item 6).
//!
//! A cast consent is a SEPARATE record from Voice-Tutor key/URL trust
//! (T13): it names one book, one connection, one phase, the exhaustive
//! canonical destination hosts, and the exact data categories that phase
//! sends (Scripting: source/book text; TTS: script text). A consent is
//! valid only while hosts and categories still match exactly — a host-set
//! or category change invalidates it by construction, and revocation is a
//! recorded fact, not a deletion. Loopback phases never create records
//! (A18: no consent theater for this-computer traffic).

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::storage::settings::{get_setting, set_setting};

const STORE_KEY: &str = "provider_cast_consents_v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CastConsent {
    pub id: String,
    pub book_id: String,
    pub connection_id: String,
    pub provider: String,
    pub phase: String,
    /// Exhaustive canonical destination hosts (A14): auth + media, declared
    /// by the adapter, sorted for comparison.
    pub hosts: Vec<String>,
    /// Exact data categories the phase sends, sorted for comparison.
    pub data_categories: Vec<String>,
    pub granted_unix: i64,
    pub revoked_unix: Option<i64>,
}

#[derive(Debug)]
pub enum ConsentError {
    Storage(String),
}

impl std::fmt::Display for ConsentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsentError::Storage(err) => write!(f, "consent store: {err}"),
        }
    }
}

pub struct ConsentStore;

/// Lost-update guard (Hermes R2 fold-in 3): every mutation of the one
/// consent document is serialized process-wide, the same single-writer
/// discipline the connection store uses.
fn store_mutex() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn normalized(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

impl ConsentStore {
    pub async fn list(pool: &SqlitePool) -> Result<Vec<CastConsent>, ConsentError> {
        let raw = get_setting(pool, STORE_KEY)
            .await
            .map_err(|e| ConsentError::Storage(e.to_string()))?;
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        serde_json::from_str(&raw).map_err(|e| ConsentError::Storage(e.to_string()))
    }

    async fn persist(pool: &SqlitePool, all: &[CastConsent]) -> Result<(), ConsentError> {
        let json = serde_json::to_string(all).map_err(|e| ConsentError::Storage(e.to_string()))?;
        set_setting(pool, STORE_KEY, &json)
            .await
            .map_err(|e| ConsentError::Storage(e.to_string()))
    }

    /// Record a grant. Hosts/categories are normalized so later validity
    /// checks compare sets, not orderings.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        pool: &SqlitePool,
        book_id: &str,
        connection_id: &str,
        provider: &str,
        phase: &str,
        hosts: Vec<String>,
        data_categories: Vec<String>,
        now_unix: i64,
    ) -> Result<CastConsent, ConsentError> {
        let consent = CastConsent {
            id: crate::storage::new_id(),
            book_id: book_id.to_string(),
            connection_id: connection_id.to_string(),
            provider: provider.to_string(),
            phase: phase.to_string(),
            hosts: normalized(hosts),
            data_categories: normalized(data_categories),
            granted_unix: now_unix,
            revoked_unix: None,
        };
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        // Dedupe: a re-grant of the identical valid tuple REPLACES the old
        // record instead of growing the document forever.
        all.retain(|existing| {
            !(existing.revoked_unix.is_none()
                && existing.book_id == consent.book_id
                && existing.connection_id == consent.connection_id
                && existing.provider == consent.provider
                && existing.phase == consent.phase
                && existing.hosts == consent.hosts
                && existing.data_categories == consent.data_categories)
        });
        all.push(consent.clone());
        Self::persist(pool, &all).await?;
        Ok(consent)
    }

    /// Consent dies with its book (Hermes R2 fold-in 3): a deleted book's
    /// cast consents must not outlive it.
    pub async fn delete_for_book(pool: &SqlitePool, book_id: &str) -> Result<(), ConsentError> {
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        all.retain(|consent| consent.book_id != book_id);
        Self::persist(pool, &all).await
    }

    /// Consent dies with its connection: the tuple it authorized no longer
    /// exists.
    pub async fn delete_for_connection(
        pool: &SqlitePool,
        connection_id: &str,
    ) -> Result<(), ConsentError> {
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        all.retain(|consent| consent.connection_id != connection_id);
        Self::persist(pool, &all).await
    }

    /// A valid consent for exactly this tuple: same book, connection,
    /// provider, phase, EXACT host set, EXACT category set, not revoked.
    /// Any drift — a new auth host, a widened data category, a rebound
    /// connection — makes the old record simply not match (invalidation by
    /// construction, pre-brief item 6).
    #[allow(clippy::too_many_arguments)]
    pub async fn find_valid(
        pool: &SqlitePool,
        book_id: &str,
        connection_id: &str,
        provider: &str,
        phase: &str,
        hosts: Vec<String>,
        data_categories: Vec<String>,
    ) -> Result<Option<CastConsent>, ConsentError> {
        let hosts = normalized(hosts);
        let categories = normalized(data_categories);
        Ok(Self::list(pool).await?.into_iter().rev().find(|consent| {
            consent.revoked_unix.is_none()
                && consent.book_id == book_id
                && consent.connection_id == connection_id
                && consent.provider == provider
                && consent.phase == phase
                && consent.hosts == hosts
                && consent.data_categories == categories
        }))
    }

    pub async fn revoke(
        pool: &SqlitePool,
        consent_id: &str,
        now_unix: i64,
    ) -> Result<bool, ConsentError> {
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        let mut revoked = false;
        for consent in all.iter_mut() {
            if consent.id == consent_id && consent.revoked_unix.is_none() {
                consent.revoked_unix = Some(now_unix);
                revoked = true;
            }
        }
        if revoked {
            Self::persist(pool, &all).await?;
        }
        Ok(revoked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        (dir, pool)
    }

    fn hosts() -> Vec<String> {
        vec!["api.cartesia.ai".to_string()]
    }

    fn categories() -> Vec<String> {
        vec!["script_text".to_string()]
    }

    #[tokio::test]
    async fn grant_then_find_then_revoke() {
        let (_dir, pool) = pool().await;
        let consent = ConsentStore::record(
            &pool,
            "book-1",
            "c-1",
            "cartesia_tts",
            "tts",
            hosts(),
            categories(),
            1_756_000_000,
        )
        .await
        .unwrap();
        let found = ConsentStore::find_valid(
            &pool,
            "book-1",
            "c-1",
            "cartesia_tts",
            "tts",
            hosts(),
            categories(),
        )
        .await
        .unwrap();
        assert_eq!(
            found.as_ref().map(|c| c.id.as_str()),
            Some(consent.id.as_str())
        );

        assert!(ConsentStore::revoke(&pool, &consent.id, 1_756_000_001)
            .await
            .unwrap());
        let after = ConsentStore::find_valid(
            &pool,
            "book-1",
            "c-1",
            "cartesia_tts",
            "tts",
            hosts(),
            categories(),
        )
        .await
        .unwrap();
        assert!(after.is_none(), "a revoked consent never satisfies a check");
    }

    #[tokio::test]
    async fn any_tuple_drift_invalidates_by_construction() {
        let (_dir, pool) = pool().await;
        ConsentStore::record(
            &pool,
            "book-1",
            "c-1",
            "cartesia_tts",
            "tts",
            hosts(),
            categories(),
            1,
        )
        .await
        .unwrap();
        for (book, conn, phase, host_set, category_set) in [
            ("book-2", "c-1", "tts", hosts(), categories()),
            ("book-1", "c-2", "tts", hosts(), categories()),
            ("book-1", "c-1", "script", hosts(), categories()),
            (
                "book-1",
                "c-1",
                "tts",
                vec![
                    "api.cartesia.ai".to_string(),
                    "media.cartesia.ai".to_string(),
                ],
                categories(),
            ),
            (
                "book-1",
                "c-1",
                "tts",
                hosts(),
                vec!["script_text".to_string(), "source_text".to_string()],
            ),
        ] {
            assert!(
                ConsentStore::find_valid(
                    &pool,
                    book,
                    conn,
                    "cartesia_tts",
                    phase,
                    host_set,
                    category_set
                )
                .await
                .unwrap()
                .is_none(),
                "drifted tuple must not reuse the old grant"
            );
        }
    }

    #[tokio::test]
    async fn host_ordering_does_not_defeat_matching() {
        let (_dir, pool) = pool().await;
        ConsentStore::record(
            &pool,
            "b",
            "c",
            "p",
            "tts",
            vec!["b-host".to_string(), "a-host".to_string()],
            categories(),
            1,
        )
        .await
        .unwrap();
        assert!(ConsentStore::find_valid(
            &pool,
            "b",
            "c",
            "p",
            "tts",
            vec!["a-host".to_string(), "b-host".to_string()],
            categories()
        )
        .await
        .unwrap()
        .is_some());
    }
}
