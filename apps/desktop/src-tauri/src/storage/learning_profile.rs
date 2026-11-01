//! Store functions for the user learning profile (migration 0019; owner
//! ruling R1). Written only through the voice runtime's profile writer (the
//! `update_learning_profile` tool path); read at session assembly for the
//! prompt block.

use sqlx::SqlitePool;

use super::now_ms;

/// Hard ceiling on profile rows. The profile is a compact prompt artifact,
/// not a datastore: every entry costs session tokens, so growth beyond a
/// handful of stated preferences is more likely runaway extraction than
/// signal. Enforced here (transactionally) as the backstop behind the
/// sink-side check.
pub const PROFILE_ENTRY_CAP: i64 = 16;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileEntry {
    pub key: String,
    pub value: String,
    pub updated_at: i64,
}

/// Upsert one preference. Refuses (Err with a `Protocol`-shaped database
/// error is NOT used — the refusal is a typed `CapExceeded`) when the entry
/// cap is reached and `key` is new; updating an existing key always works.
pub async fn upsert_entry(pool: &SqlitePool, key: &str, value: &str) -> Result<(), UpsertError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(UpsertError::Db)?;
    let existing = sqlx::query_scalar!("SELECT COUNT(*) FROM learning_profile WHERE key = ?", key)
        .fetch_one(&mut *tx)
        .await
        .map_err(UpsertError::Db)?;
    if existing == 0 {
        let total = sqlx::query_scalar!("SELECT COUNT(*) FROM learning_profile")
            .fetch_one(&mut *tx)
            .await
            .map_err(UpsertError::Db)?;
        if total >= PROFILE_ENTRY_CAP {
            return Err(UpsertError::CapExceeded);
        }
    }
    let now = now_ms();
    sqlx::query!(
        "INSERT INTO learning_profile (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        key,
        value,
        now,
    )
    .execute(&mut *tx)
    .await
    .map_err(UpsertError::Db)?;
    tx.commit().await.map_err(UpsertError::Db)
}

#[derive(Debug)]
pub enum UpsertError {
    /// The entry cap is reached and the key is new.
    CapExceeded,
    Db(sqlx::Error),
}

impl std::fmt::Display for UpsertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapExceeded => write!(
                f,
                "the learning profile is full ({PROFILE_ENTRY_CAP} entries)"
            ),
            Self::Db(error) => write!(f, "learning profile write failed: {error}"),
        }
    }
}

/// All entries, most recently updated first — the injection order (the
/// prompt block truncates oldest-first when over its budget).
pub async fn list_entries(pool: &SqlitePool) -> Result<Vec<ProfileEntry>, sqlx::Error> {
    sqlx::query_as!(
        ProfileEntry,
        r#"SELECT key as "key!", value, updated_at FROM learning_profile
           ORDER BY updated_at DESC, key"#
    )
    .fetch_all(pool)
    .await
}

/// Remove one preference (the settings-UI surface; unused by the runtime).
pub async fn delete_entry(pool: &SqlitePool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM learning_profile WHERE key = ?", key)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("profile.db"))
            .await
            .unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn forgetting_one_preference_keeps_the_others() {
        let (_dir, pool) = pool().await;
        upsert_entry(&pool, "pace", "slow down").await.unwrap();
        upsert_entry(&pool, "language", "Arabic").await.unwrap();
        delete_entry(&pool, "pace").await.unwrap();
        delete_entry(&pool, "never-saved").await.unwrap();
        let keys: Vec<String> = list_entries(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.key)
            .collect();
        assert_eq!(keys, vec!["language".to_string()]);
    }

    #[test]
    fn a_preference_reaches_the_settings_surface_in_camel_case() {
        let entry = ProfileEntry {
            key: "pace".to_string(),
            value: "slow down".to_string(),
            updated_at: 7,
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({"key": "pace", "value": "slow down", "updatedAt": 7})
        );
    }

    #[tokio::test]
    async fn upsert_roundtrips_updates_in_place_and_orders_by_recency() {
        let (_dir, pool) = pool().await;
        upsert_entry(&pool, "equations", "goal first")
            .await
            .unwrap();
        upsert_entry(&pool, "tone", "concise").await.unwrap();
        upsert_entry(&pool, "equations", "intuition before formalism")
            .await
            .unwrap();

        let entries = list_entries(&pool).await.unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.key.as_str(), e.value.as_str()))
                .collect::<Vec<_>>()
                .len(),
            2,
            "an update replaces, never duplicates"
        );
        assert_eq!(
            entries[0].value, "intuition before formalism",
            "most recently updated first (injection order)"
        );
    }

    #[tokio::test]
    async fn the_cap_refuses_new_keys_but_never_updates() {
        let (_dir, pool) = pool().await;
        for at in 0..PROFILE_ENTRY_CAP {
            upsert_entry(&pool, &format!("k{at}"), "v").await.unwrap();
        }
        assert!(
            matches!(
                upsert_entry(&pool, "one-more", "v").await,
                Err(UpsertError::CapExceeded)
            ),
            "a 17th key is refused"
        );
        upsert_entry(&pool, "k0", "updated")
            .await
            .expect("updating an existing key is never capped");
    }

    #[tokio::test]
    async fn the_schema_bounds_are_a_real_backstop() {
        let (_dir, pool) = pool().await;
        assert!(
            upsert_entry(&pool, &"k".repeat(49), "v").await.is_err(),
            "the 0019 CHECK rejects oversized keys even if the tool layer regressed"
        );
        assert!(
            upsert_entry(&pool, "k", &"v".repeat(201)).await.is_err(),
            "the 0019 CHECK rejects oversized values even if the tool layer regressed"
        );
    }
}
