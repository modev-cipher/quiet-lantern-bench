use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::storage::{settings, SqlitePool};
use crate::voice::keys::{ApiKey, KeyError, KeyManager};

pub const FALLBACK_ENABLED_KEY: &str = "ingest.fallback_enabled";
pub const OPENAI_BASE_URL: &str = "https://api.openai.com";
pub const GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com";
pub const READINGS_DIR_NAME: &str = "fallback-readings";

#[derive(Clone, Default)]
pub struct FallbackSource {
    keys: Option<Arc<KeyManager>>,
}

impl FallbackSource {
    pub fn off() -> Self {
        Self { keys: None }
    }

    pub fn from_manager(keys: Arc<KeyManager>) -> Self {
        Self { keys: Some(keys) }
    }

    pub fn from_optional(keys: Option<Arc<KeyManager>>) -> Self {
        Self { keys }
    }

    pub async fn resolve(&self, pool: &SqlitePool) -> Option<FallbackBlock> {
        let keys = self.keys.as_ref()?;
        if !fallback_enabled(pool).await {
            return None;
        }
        let block = FallbackBlock {
            openai_key: stored_key(keys, OPENAI_BASE_URL).await,
            google_key: stored_key(keys, GOOGLE_BASE_URL).await,
            cache_dir: None,
        };
        block.has_any_key().then_some(block)
    }
}

pub async fn fallback_enabled(pool: &SqlitePool) -> bool {
    matches!(
        settings::get_setting(pool, FALLBACK_ENABLED_KEY)
            .await
            .ok()
            .flatten()
            .as_deref(),
        Some("true") | Some("1") | Some("yes")
    )
}

async fn stored_key(keys: &KeyManager, base_url: &str) -> Option<ApiKey> {
    match keys.session_key(base_url).await {
        Ok(key) if !key.is_empty() => Some(key),
        Ok(_) | Err(KeyError::NotConfigured) => None,
        Err(error) => {
            tracing::warn!("ingest fallback: key for {base_url} unavailable: {error}");
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct FallbackBlock {
    openai_key: Option<ApiKey>,
    google_key: Option<ApiKey>,
    cache_dir: Option<PathBuf>,
}

impl FallbackBlock {
    pub fn readings_beside(mut self, source: &Path) -> Self {
        self.cache_dir = source.parent().map(|dir| dir.join(READINGS_DIR_NAME));
        self
    }

    fn has_any_key(&self) -> bool {
        self.openai_key.is_some() || self.google_key.is_some()
    }

    pub fn providers(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.openai_key.is_some() {
            out.push("openai");
        }
        if self.google_key.is_some() {
            out.push("google");
        }
        out
    }
}

impl fmt::Display for FallbackBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fallback[{}]", self.providers().join(","))
    }
}

impl Serialize for FallbackBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = self.providers().len() + usize::from(self.cache_dir.is_some());
        let mut state = serializer.serialize_struct("FallbackBlock", fields)?;
        if let Some(key) = &self.openai_key {
            state.serialize_field("openai_key", key.expose())?;
        }
        if let Some(key) = &self.google_key {
            state.serialize_field("google_key", key.expose())?;
        }
        if let Some(dir) = &self.cache_dir {
            state.serialize_field("cache_dir", &dir.to_string_lossy())?;
        }
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AIzaSy-test-material-0123456789abcdef";

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .expect("open pool");
        (dir, pool)
    }

    async fn manager_with_google_key(pool: &SqlitePool) -> Arc<KeyManager> {
        let keys = Arc::new(KeyManager::session_only_for_tests());
        keys.add_key(pool, "gemini", None, KEY.to_string(), None)
            .await
            .expect("add key");
        keys
    }

    async fn enable(pool: &SqlitePool) {
        settings::set_setting(pool, FALLBACK_ENABLED_KEY, "true")
            .await
            .expect("set");
    }

    #[tokio::test]
    async fn no_manager_means_no_block_even_when_enabled() {
        let (_dir, pool) = pool().await;
        enable(&pool).await;
        assert!(FallbackSource::off().resolve(&pool).await.is_none());
        assert!(FallbackSource::from_optional(None)
            .resolve(&pool)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn no_stored_key_means_no_block_not_empty_strings() {
        let (_dir, pool) = pool().await;
        enable(&pool).await;
        let keys = Arc::new(KeyManager::session_only_for_tests());
        let block = FallbackSource::from_manager(keys).resolve(&pool).await;
        assert!(block.is_none(), "{block:?}");
    }

    #[tokio::test]
    async fn toggle_off_or_unset_means_no_block_even_with_a_key() {
        let (_dir, pool) = pool().await;
        let keys = manager_with_google_key(&pool).await;
        let source = FallbackSource::from_manager(keys);
        assert!(source.resolve(&pool).await.is_none(), "unset must be off");
        settings::set_setting(&pool, FALLBACK_ENABLED_KEY, "false")
            .await
            .expect("set");
        assert!(source.resolve(&pool).await.is_none(), "false must be off");
    }

    #[tokio::test]
    async fn enabled_with_a_key_serialises_only_the_stored_provider() {
        let (_dir, pool) = pool().await;
        enable(&pool).await;
        let keys = manager_with_google_key(&pool).await;
        let block = FallbackSource::from_manager(keys)
            .resolve(&pool)
            .await
            .expect("block present");
        let json = serde_json::to_value(&block).expect("json");
        assert_eq!(json["google_key"], KEY);
        assert!(json.get("openai_key").is_none(), "{json}");
        assert_eq!(json.as_object().map(|o| o.len()), Some(1));
        assert_eq!(block.providers(), vec!["google"]);
    }

    #[tokio::test]
    async fn readings_beside_a_book_source_name_its_own_directory() {
        let (_dir, pool) = pool().await;
        enable(&pool).await;
        let keys = manager_with_google_key(&pool).await;
        let source = Path::new("books").join("book-1").join("source.pdf");
        let block = FallbackSource::from_manager(keys)
            .resolve(&pool)
            .await
            .expect("block present")
            .readings_beside(&source);
        let json = serde_json::to_value(&block).expect("json");
        let expected = Path::new("books").join("book-1").join(READINGS_DIR_NAME);
        assert_eq!(json["cache_dir"], expected.to_string_lossy().as_ref());
        assert_eq!(json["google_key"], KEY);
        assert_eq!(json.as_object().map(|o| o.len()), Some(2));
        assert!(!format!("{block:?}").contains(KEY));
    }

    #[tokio::test]
    async fn debug_display_and_tracing_carry_no_key_bytes() {
        use std::io;
        use std::sync::Mutex;
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("lock").extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
            type Writer = Capture;
            fn make_writer(&'a self) -> Capture {
                self.clone()
            }
        }

        let (_dir, pool) = pool().await;
        enable(&pool).await;
        let keys = manager_with_google_key(&pool).await;
        let block = FallbackSource::from_manager(keys)
            .resolve(&pool)
            .await
            .expect("block present");
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("trace"))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(capture.clone()),
            );
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            tracing::info!(block = ?block, summary = %block, "ingest request built");
        }
        let logged = String::from_utf8(capture.0.lock().expect("lock").clone()).expect("utf-8");
        assert!(logged.contains("ingest request built"), "{logged}");
        assert!(!logged.contains(KEY), "{logged}");
        assert!(logged.contains("redacted"), "{logged}");
        assert!(logged.contains("fallback[google]"), "{logged}");

        let debug = format!("{block:?}");
        assert!(!debug.contains(KEY), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
        assert_eq!(block.to_string(), "fallback[google]");
    }
}
