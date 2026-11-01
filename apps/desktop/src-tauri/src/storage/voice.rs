use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceProfile {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub kind: String,
    pub preset_voice_id: Option<String>,
    pub reference_wav_path: Option<String>,
    pub params_json: Option<String>,
    pub created_at: i64,
}

pub async fn insert_voice_profile(
    pool: &SqlitePool,
    profile: &VoiceProfile,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO voice_profile (id, name, engine, kind, preset_voice_id, reference_wav_path, params_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        profile.id,
        profile.name,
        profile.engine,
        profile.kind,
        profile.preset_voice_id,
        profile.reference_wav_path,
        profile.params_json,
        profile.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn insert_voice_profile_if_missing(
    pool: &SqlitePool,
    profile: &VoiceProfile,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT OR IGNORE INTO voice_profile (id, name, engine, kind, preset_voice_id, reference_wav_path, params_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        profile.id,
        profile.name,
        profile.engine,
        profile.kind,
        profile.preset_voice_id,
        profile.reference_wav_path,
        profile.params_json,
        profile.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_voice_profile(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<VoiceProfile>, sqlx::Error> {
    sqlx::query_as!(
        VoiceProfile,
        r#"SELECT id as "id!", name, engine, kind, preset_voice_id, reference_wav_path, params_json, created_at
           FROM voice_profile WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
}

pub async fn list_voice_profiles(pool: &SqlitePool) -> Result<Vec<VoiceProfile>, sqlx::Error> {
    sqlx::query_as!(
        VoiceProfile,
        r#"SELECT id as "id!", name, engine, kind, preset_voice_id, reference_wav_path, params_json, created_at
           FROM voice_profile ORDER BY created_at"#
    )
    .fetch_all(pool)
    .await
}

pub async fn delete_voice_profile(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM voice_profile WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}
