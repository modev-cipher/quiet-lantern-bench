use std::fmt;
use std::path::{Path, PathBuf};

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqlitePool};

use super::settings::get_setting;

pub const MODELS_DIR_KEY: &str = "storage.models_dir";
pub const EXPORTS_DIR_KEY: &str = "storage.exports_dir";

pub const MODELS_DIR_NAME: &str = "models";
pub const EXPORTS_DIR_NAME: &str = "exports";
pub const CACHE_DIR_NAME: &str = "cache";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirRejection {
    Empty,
    NotAbsolute,
    Missing,
    NotADirectory,
    NotWritable(String),
}

impl fmt::Display for DirRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the path is empty"),
            Self::NotAbsolute => write!(f, "the path is not absolute"),
            Self::Missing => write!(f, "the directory does not exist"),
            Self::NotADirectory => write!(f, "the path is not a directory"),
            Self::NotWritable(reason) => write!(f, "the directory is not writable: {reason}"),
        }
    }
}

pub fn parse_override(raw: &str) -> Result<PathBuf, DirRejection> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DirRejection::Empty);
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        return Err(DirRejection::NotAbsolute);
    }
    Ok(path.to_path_buf())
}

pub fn validate_dir(path: &Path) -> Result<(), DirRejection> {
    if !path.is_absolute() {
        return Err(DirRejection::NotAbsolute);
    }
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(DirRejection::Missing)
        }
        Err(err) => return Err(DirRejection::NotWritable(err.to_string())),
    };
    if !meta.is_dir() {
        return Err(DirRejection::NotADirectory);
    }
    probe_writable(path)
}

fn probe_writable(dir: &Path) -> Result<(), DirRejection> {
    let probe = dir.join(format!(".txtcaster-write-probe-{}", super::new_id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(err) => Err(DirRejection::NotWritable(err.to_string())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDir {
    pub path: PathBuf,
    pub configured: Option<String>,
    pub rejected: Option<DirRejection>,
}

impl ResolvedDir {
    fn default_at(path: PathBuf) -> Self {
        Self {
            path,
            configured: None,
            rejected: None,
        }
    }

    pub fn is_default(&self) -> bool {
        self.configured.is_none() || self.rejected.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDirs {
    pub models: ResolvedDir,
    pub exports: ResolvedDir,
    pub cache_dir: PathBuf,
}

impl StorageDirs {
    pub fn models_dir(&self) -> &Path {
        &self.models.path
    }

    pub fn exports_dir(&self) -> &Path {
        &self.exports.path
    }
}

pub const LEGACY_IDENTIFIER: &str = "io.github.meedoomostafa.txtcaster";
pub const CANONICAL_APP_NAME: &str = "txtcaster";

/// Local AppData on Windows: engines and models must not sync with a roaming profile, and
/// `%LOCALAPPDATA%\\txtCaster` is the install folder. The asset scope and installer hooks name these too.
pub const WINDOWS_DATA_DIR_NAME: &str = "txtcaster-data";
pub const WINDOWS_CACHE_DIR_NAME: &str = "txtcaster-cache";

pub fn defaults(data_dir: &Path) -> StorageDirs {
    StorageDirs {
        models: ResolvedDir::default_at(data_dir.join(MODELS_DIR_NAME)),
        exports: ResolvedDir::default_at(data_dir.join(EXPORTS_DIR_NAME)),
        cache_dir: data_dir.join(CACHE_DIR_NAME),
    }
}

pub fn defaults_with_cache(data_dir: &Path, cache_dir: PathBuf) -> StorageDirs {
    let mut dirs = defaults(data_dir);
    dirs.cache_dir = cache_dir;
    dirs
}

fn is_cross_device_error(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        // EXDEV is errno 18 on Linux/macOS/BSD
        if err.raw_os_error() == Some(18) {
            return true;
        }
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE is error code 17 on Windows
        if err.raw_os_error() == Some(17) {
            return true;
        }
    }
    false
}

pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let link_target = std::fs::read_link(entry.path())?;
                std::os::unix::fs::symlink(link_target, target)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(entry.path(), target)?;
            }
        }
    }
    Ok(())
}

pub fn move_path_with_fallback(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device_error(&err) => {
            tracing::info!(
                "cross-device move detected for {} -> {}; falling back to copy-then-delete",
                src.display(),
                dst.display()
            );
            if src.is_dir() {
                copy_dir_recursive(src, dst)?;
                std::fs::remove_dir_all(src)?;
            } else {
                std::fs::copy(src, dst)?;
                std::fs::remove_file(src)?;
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

pub fn merge_directory_non_destructive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        return move_path_with_fallback(src, dst);
    }

    if !src.is_dir() || !dst.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let src_child = entry.path();
        let dst_child = dst.join(&file_name);

        if !dst_child.exists() {
            if let Err(err) = move_path_with_fallback(&src_child, &dst_child) {
                tracing::warn!(
                    "failed to move {} to {}: {err}",
                    src_child.display(),
                    dst_child.display()
                );
            }
        } else if src_child.is_dir() && dst_child.is_dir() {
            let _ = merge_directory_non_destructive(&src_child, &dst_child);
        } else if src_child.is_file() && dst_child.is_file() {
            let dst_len = std::fs::metadata(&dst_child).map(|m| m.len()).unwrap_or(0);
            let src_len = std::fs::metadata(&src_child).map(|m| m.len()).unwrap_or(0);
            if dst_len == 0 && src_len > 0 {
                tracing::info!(
                    "replacing 0-byte canonical placeholder {} with legacy file ({} bytes)",
                    dst_child.display(),
                    src_len
                );
                let _ = std::fs::remove_file(&dst_child);
                if let Err(err) = move_path_with_fallback(&src_child, &dst_child) {
                    tracing::warn!(
                        "failed to move {} over 0-byte placeholder {}: {err}",
                        src_child.display(),
                        dst_child.display()
                    );
                }
            } else {
                tracing::info!(
                    "preserving existing non-empty canonical file at {}",
                    dst_child.display()
                );
            }
        } else {
            tracing::info!(
                "preserving existing canonical entry at {}",
                dst_child.display()
            );
        }
    }

    let _ = std::fs::remove_dir(src);
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DbPopulation {
    pub user_content_rows: i64,
    pub metadata_rows: i64,
}

impl DbPopulation {
    pub fn is_empty(&self) -> bool {
        self.user_content_rows == 0 && self.metadata_rows == 0
    }
}

async fn inspect_db_population(db_path: &Path) -> DbPopulation {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true);
    let mut conn = match sqlx::SqliteConnection::connect_with(&options).await {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(
                "failed to open {} for migration check: {err}",
                db_path.display()
            );
            return DbPopulation::default();
        }
    };
    let mut pop = DbPopulation::default();
    for table in &[
        "book",
        "render_job",
        "voice_session",
        "chapter_render",
        "export",
    ] {
        let query =
            format!("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'");
        let exists: i64 = sqlx::query_scalar(&query)
            .fetch_one(&mut conn)
            .await
            .unwrap_or(0);
        if exists > 0 {
            let row_query = format!("SELECT count(*) FROM \"{table}\"");
            let row_count: i64 = sqlx::query_scalar(&row_query)
                .fetch_one(&mut conn)
                .await
                .unwrap_or(0);
            pop.user_content_rows += row_count;
        }
    }
    for table in &["app_setting", "voice_profile"] {
        let query =
            format!("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'");
        let exists: i64 = sqlx::query_scalar(&query)
            .fetch_one(&mut conn)
            .await
            .unwrap_or(0);
        if exists > 0 {
            let row_query = format!("SELECT count(*) FROM \"{table}\"");
            let row_count: i64 = sqlx::query_scalar(&row_query)
                .fetch_one(&mut conn)
                .await
                .unwrap_or(0);
            pop.metadata_rows += row_count;
        }
    }
    pop
}

fn block_on_future<F>(f: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(f)
    })
    .join()
    .expect("migration task thread panicked")
}

fn json_string_body(value: &str) -> String {
    let quoted = serde_json::Value::String(value.to_owned()).to_string();
    quoted[1..quoted.len() - 1].to_owned()
}

async fn rewrite_db_paths(db_path: &Path, legacy_str: &str, canonical_str: &str) {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(false);
    let mut conn = match sqlx::SqliteConnection::connect_with(&options).await {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(
                "failed to open {} for path rewrite: {err}",
                db_path.display()
            );
            return;
        }
    };
    let pattern = format!("%{legacy_str}%");
    let json_legacy = json_string_body(legacy_str);
    let json_canonical = json_string_body(canonical_str);
    let updates = [
        ("book", "source_path", false),
        ("book", "cover_path", false),
        ("book", "ingest_path", false),
        ("render_job", "output_paths_json", true),
    ];
    for (table, col, json_encoded) in updates {
        let check_query =
            format!("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'");
        let exists: i64 = sqlx::query_scalar(&check_query)
            .fetch_one(&mut conn)
            .await
            .unwrap_or(0);
        if exists > 0 {
            let (from, to) = if json_encoded {
                (json_legacy.as_str(), json_canonical.as_str())
            } else {
                (legacy_str, canonical_str)
            };
            let update_query = format!(
                "UPDATE \"{table}\" SET \"{col}\" = replace(\"{col}\", ?1, ?2) WHERE instr(\"{col}\", ?1) > 0"
            );
            let _ = sqlx::query(&update_query)
                .bind(from)
                .bind(to)
                .execute(&mut conn)
                .await;
        }
    }

    let export_check = "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='export'";
    let export_exists: i64 = sqlx::query_scalar(export_check)
        .fetch_one(&mut conn)
        .await
        .unwrap_or(0);
    if export_exists > 0 {
        // `instr(path, ?1) > 0`, not `path LIKE ?1`, and not
        // `replace(path, ...) IN (SELECT path FROM export)` on its own: this
        // guard has to mean "rewriting this row would collide with a row that is
        // already there", and only the first conjunct tests the premise. `LIKE`
        // treats `_` and `%` in the legacy profile path as wildcards — and
        // `C:\Users\John_Doe\...` is routine — so it selects rows that do not
        // contain the legacy string at all. For those, `replace()` returns the
        // path unchanged and the `IN` test is satisfied *by the row itself*, so
        // the statement degenerates to `DELETE ... WHERE path LIKE '%legacy%'`
        // and every wildcard-over-matching row deletes itself, silently, because
        // the error is discarded. A literal containment test has no such
        // degenerate case: `replace()` now always differs from `path`.
        let _ = sqlx::query(
            "DELETE FROM export
             WHERE instr(path, ?1) > 0
               AND replace(path, ?1, ?2) IN (SELECT path FROM export)",
        )
        .bind(legacy_str)
        .bind(canonical_str)
        .execute(&mut conn)
        .await;

        let _ = sqlx::query("UPDATE export SET path = replace(path, ?1, ?2) WHERE path LIKE ?3")
            .bind(legacy_str)
            .bind(canonical_str)
            .bind(&pattern)
            .execute(&mut conn)
            .await;

        // An exact `json_each` element match, not `LIKE '%'||export.path||'%'`, for
        // the same reason as in `export::backfill_orphan_links` and
        // `export::adopt_orphans`: the path is user data and `_` and `%` in it are
        // LIKE wildcards, so a wildcard pattern can attribute one cast's export to
        // another cast's job.
        let reconcile_query = r#"UPDATE export
            SET job_id = (SELECT r.id FROM render_job r WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.output_paths_json) THEN r.output_paths_json ELSE '[]' END) o WHERE o.value = export.path) LIMIT 1),
                book_id = (SELECT r.book_id FROM render_job r WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.output_paths_json) THEN r.output_paths_json ELSE '[]' END) o WHERE o.value = export.path) LIMIT 1)
            WHERE (export.job_id IS NULL OR export.job_id = '' OR export.book_id IS NULL OR export.book_id = '')
              AND EXISTS (SELECT 1 FROM render_job r WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.output_paths_json) THEN r.output_paths_json ELSE '[]' END) o WHERE o.value = export.path))"#;
        let _ = sqlx::query(reconcile_query).execute(&mut conn).await;
    }
}

pub fn reconcile_database(canonical_dir: &Path, legacy_dir: &Path) -> std::io::Result<()> {
    let legacy_db = legacy_dir.join("db.sqlite");
    if !legacy_db.exists() {
        return Ok(());
    }

    let canonical_db = canonical_dir.join("db.sqlite");
    if !canonical_db.exists() {
        for suffix in &["", "-wal", "-shm"] {
            let leg = legacy_dir.join(format!("db.sqlite{suffix}"));
            let can = canonical_dir.join(format!("db.sqlite{suffix}"));
            if leg.exists() {
                let _ = move_path_with_fallback(&leg, &can);
            }
        }
        let leg_str = legacy_dir.to_string_lossy().to_string();
        let can_str = canonical_dir.to_string_lossy().to_string();
        let can_db_for_rewrite = canonical_db.clone();
        block_on_future(async move {
            rewrite_db_paths(&can_db_for_rewrite, &leg_str, &can_str).await;
        });
        return Ok(());
    }

    let can_path = canonical_db.clone();
    let leg_path = legacy_db.clone();
    let (canonical_pop, legacy_pop) = block_on_future(async move {
        (
            inspect_db_population(&can_path).await,
            inspect_db_population(&leg_path).await,
        )
    });

    let should_migrate = (canonical_pop.user_content_rows == 0 && legacy_pop.user_content_rows > 0)
        || (canonical_pop.user_content_rows == 0
            && legacy_pop.user_content_rows == 0
            && canonical_pop.metadata_rows == 0
            && legacy_pop.metadata_rows > 0);

    if should_migrate {
        tracing::info!(
            "reconciling database: canonical has 0 user content rows (metadata: {}), legacy has {} user content rows (metadata: {})",
            canonical_pop.metadata_rows,
            legacy_pop.user_content_rows,
            legacy_pop.metadata_rows
        );
        for suffix in &["", "-wal", "-shm"] {
            let can = canonical_dir.join(format!("db.sqlite{suffix}"));
            let bak = canonical_dir.join(format!("db.sqlite{suffix}.bak"));
            if can.exists() {
                if let Err(err) = std::fs::copy(&can, &bak) {
                    tracing::warn!("failed to create backup {}: {err}", bak.display());
                } else {
                    let _ = std::fs::remove_file(&can);
                }
            }
        }

        for suffix in &["", "-wal", "-shm"] {
            let leg = legacy_dir.join(format!("db.sqlite{suffix}"));
            let can = canonical_dir.join(format!("db.sqlite{suffix}"));
            if leg.exists() {
                if let Err(err) = move_path_with_fallback(&leg, &can) {
                    tracing::warn!(
                        "failed to move database file {} to {}: {err}",
                        leg.display(),
                        can.display()
                    );
                }
            }
        }
    } else {
        tracing::info!(
            "preserving canonical database (canonical content: {}, legacy content: {})",
            canonical_pop.user_content_rows,
            legacy_pop.user_content_rows
        );
    }

    let leg_str = legacy_dir.to_string_lossy().to_string();
    let can_str = canonical_dir.to_string_lossy().to_string();
    let can_db_for_rewrite = canonical_db.clone();
    block_on_future(async move {
        rewrite_db_paths(&can_db_for_rewrite, &leg_str, &can_str).await;
    });

    Ok(())
}

pub const WEBVIEW_PROFILE_DIR: &str = "EBWebView";

// The webview keeps its profile in the identifier-named folder, which is also the legacy data
// folder, and holds these open while the app runs.
const WEBKIT_PROFILE_ENTRIES: [&str; 13] = [
    "CacheStorage",
    "WebKitCache",
    "applications",
    "databases",
    "deviceidhashsalts",
    "hsts-storage.sqlite",
    "indexeddb",
    "itp",
    "localstorage",
    "mediakeys",
    "resourceLoadStatistics",
    "serviceworkers",
    "storage",
];

fn is_webview_profile_entry(name: &std::ffi::OsStr) -> bool {
    name == WEBVIEW_PROFILE_DIR || WEBKIT_PROFILE_ENTRIES.iter().any(|entry| name == *entry)
}

fn holds_webview_profile(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| is_webview_profile_entry(&entry.file_name()))
        })
        .unwrap_or(false)
}

fn holds_only_webview_profile(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .all(|entry| is_webview_profile_entry(&entry.file_name()))
        })
        .unwrap_or(false)
}

pub fn resolve_and_migrate_dir(base: &Path, legacy_name: &str, canonical_name: &str) -> PathBuf {
    let canonical = base.join(canonical_name);
    let legacy = base.join(legacy_name);

    if !legacy.exists() {
        return canonical;
    }

    let holds_webview_profile = holds_webview_profile(&legacy);
    if holds_webview_profile && !canonical.exists() {
        if let Err(err) = std::fs::create_dir_all(&canonical) {
            tracing::warn!(
                "failed to create {} beside the webview profile: {err}; falling back to legacy",
                canonical.display()
            );
            return legacy;
        }
    }

    if canonical.exists() && holds_webview_profile && holds_only_webview_profile(&legacy) {
        return canonical;
    }

    if !canonical.exists() {
        if let Err(err) = move_path_with_fallback(&legacy, &canonical) {
            tracing::warn!(
                "failed to migrate legacy dir from {} to {}: {err}; falling back to legacy",
                legacy.display(),
                canonical.display()
            );
            return legacy;
        } else {
            tracing::info!(
                "migrated legacy dir from {} to {}",
                legacy.display(),
                canonical.display()
            );
            let leg_str = legacy.to_string_lossy().to_string();
            let can_str = canonical.to_string_lossy().to_string();
            let can_db = canonical.join("db.sqlite");
            if can_db.exists() {
                block_on_future(async move {
                    rewrite_db_paths(&can_db, &leg_str, &can_str).await;
                });
            }
            return canonical;
        }
    }

    tracing::info!(
        "both canonical ({}) and legacy ({}) exist; performing granular merge",
        canonical.display(),
        legacy.display()
    );

    if let Err(err) = reconcile_database(&canonical, &legacy) {
        tracing::warn!("database reconciliation encountered error: {err}");
    }

    if let Ok(entries) = std::fs::read_dir(&legacy) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.starts_with("db.sqlite") || is_webview_profile_entry(&file_name) {
                continue;
            }

            let src_item = entry.path();
            let dst_item = canonical.join(&file_name);

            if !dst_item.exists() {
                if let Err(err) = move_path_with_fallback(&src_item, &dst_item) {
                    tracing::warn!(
                        "failed to move {} to {}: {err}",
                        src_item.display(),
                        dst_item.display()
                    );
                }
            } else if src_item.is_dir() && dst_item.is_dir() {
                let _ = merge_directory_non_destructive(&src_item, &dst_item);
            } else if src_item.is_file() && dst_item.is_file() {
                let dst_len = std::fs::metadata(&dst_item).map(|m| m.len()).unwrap_or(0);
                let src_len = std::fs::metadata(&src_item).map(|m| m.len()).unwrap_or(0);
                if dst_len == 0 && src_len > 0 {
                    tracing::info!(
                        "replacing 0-byte canonical placeholder {} with legacy file ({} bytes)",
                        dst_item.display(),
                        src_len
                    );
                    let _ = std::fs::remove_file(&dst_item);
                    if let Err(err) = move_path_with_fallback(&src_item, &dst_item) {
                        tracing::warn!(
                            "failed to move {} over 0-byte placeholder {}: {err}",
                            src_item.display(),
                            dst_item.display()
                        );
                    }
                } else {
                    tracing::info!(
                        "preserving existing non-empty canonical file at {}",
                        dst_item.display()
                    );
                }
            } else {
                tracing::info!(
                    "preserving existing canonical entry at {}",
                    dst_item.display()
                );
            }
        }
    }

    let is_empty = std::fs::read_dir(&legacy)
        .map(|mut i| i.next().is_none())
        .unwrap_or(false);

    if is_empty {
        let _ = std::fs::remove_dir(&legacy);
        tracing::info!("removed empty legacy directory {}", legacy.display());
    } else if !holds_webview_profile {
        let migrated_marker = base.join(format!("{}.migrated", legacy_name));
        if !migrated_marker.exists() {
            let _ = move_path_with_fallback(&legacy, &migrated_marker);
            tracing::info!(
                "archived unmerged legacy leftovers to {}",
                migrated_marker.display()
            );
        }
    } else {
        archive_leftovers_beside_profile(&legacy, &base.join(format!("{legacy_name}.migrated")));
    }

    canonical
}

fn archive_leftovers_beside_profile(legacy: &Path, archive: &Path) {
    if holds_only_webview_profile(legacy) {
        return;
    }
    if let Err(err) = std::fs::create_dir_all(archive) {
        tracing::warn!(
            "failed to create legacy archive {}: {err}",
            archive.display()
        );
        return;
    }
    let Ok(entries) = std::fs::read_dir(legacy) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        if is_webview_profile_entry(&file_name) {
            continue;
        }
        let mut target = archive.join(&file_name);
        if target.exists() {
            target = archive.join(format!(
                "{}.{}",
                file_name.to_string_lossy(),
                super::new_id()
            ));
        }
        if let Err(err) = move_path_with_fallback(&entry.path(), &target) {
            tracing::warn!(
                "failed to archive legacy leftover {} to {}: {err}",
                entry.path().display(),
                target.display()
            );
        }
    }
    tracing::info!(
        "archived unmerged legacy leftovers beside the webview profile to {}",
        archive.display()
    );
}

use tauri::Manager;

pub fn ensure_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
            let raw_code = err.raw_os_error();
            let is_not_supported = raw_code == Some(libc::ENOTSUP) || raw_code == Some(libc::EPERM);

            if !is_not_supported {
                return Err(err);
            }
            tracing::warn!("could not tighten permissions on {}: {err}", path.display());
        }
    }
    Ok(())
}

pub fn resolve_app_data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if cfg!(windows) {
        return windows_local_dir(app, WINDOWS_DATA_DIR_NAME);
    }
    let base = app
        .path()
        .data_dir()
        .map_err(|e| format!("failed to determine base data directory: {e}"))?;
    Ok(resolve_and_migrate_dir(
        &base,
        LEGACY_IDENTIFIER,
        CANONICAL_APP_NAME,
    ))
}

fn windows_local_dir(app: &tauri::AppHandle, name: &str) -> Result<PathBuf, String> {
    let base = app
        .path()
        .local_data_dir()
        .map_err(|e| format!("failed to determine local data directory: {e}"))?;
    Ok(base.join(name))
}

pub fn resolve_app_cache_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if cfg!(windows) {
        return windows_local_dir(app, WINDOWS_CACHE_DIR_NAME);
    }
    let base = app
        .path()
        .cache_dir()
        .map_err(|e| format!("failed to determine base cache directory: {e}"))?;
    let canonical = resolve_and_migrate_dir(&base, LEGACY_IDENTIFIER, CANONICAL_APP_NAME);

    // If legacy data_dir/cache exists, merge its contents into canonical cache
    if let Ok(data_dir) = resolve_app_data_dir(app) {
        let legacy_data_cache = data_dir.join(CACHE_DIR_NAME);
        if legacy_data_cache.exists() {
            if let Err(err) = merge_directory_non_destructive(&legacy_data_cache, &canonical) {
                tracing::warn!(
                    "failed to merge legacy data cache from {} to {}: {err}",
                    legacy_data_cache.display(),
                    canonical.display()
                );
            } else {
                tracing::info!(
                    "merged legacy data cache from {} to {}",
                    legacy_data_cache.display(),
                    canonical.display()
                );
            }
        }
    }

    Ok(canonical)
}

fn apply_override(default_path: PathBuf, raw: Option<String>) -> ResolvedDir {
    let Some(raw) = raw else {
        return ResolvedDir::default_at(default_path);
    };
    match parse_override(&raw) {
        Ok(path) => ResolvedDir {
            path,
            configured: Some(raw),
            rejected: None,
        },
        Err(rejection) => ResolvedDir {
            path: default_path,
            configured: Some(raw),
            rejected: Some(rejection),
        },
    }
}

pub async fn resolve(pool: &SqlitePool, data_dir: &Path) -> Result<StorageDirs, sqlx::Error> {
    resolve_dirs(pool, data_dir, data_dir.join(CACHE_DIR_NAME)).await
}

pub async fn resolve_dirs(
    pool: &SqlitePool,
    data_dir: &Path,
    cache_dir: PathBuf,
) -> Result<StorageDirs, sqlx::Error> {
    let models_raw = get_setting(pool, MODELS_DIR_KEY).await?;
    let exports_raw = get_setting(pool, EXPORTS_DIR_KEY).await?;
    Ok(StorageDirs {
        models: apply_override(data_dir.join(MODELS_DIR_NAME), models_raw),
        exports: apply_override(data_dir.join(EXPORTS_DIR_NAME), exports_raw),
        cache_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn ensured_directories_are_private_and_existing_ones_are_re_tightened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let private = dir.path().join("cache");
        ensure_private_dir(&private).expect("create");
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&private)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755))
                .expect("loosen");
        }
        ensure_private_dir(&private).expect("retighten");
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&private)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "an existing directory a previous install left world-readable must be re-tightened"
            );
        }
    }

    #[test]
    fn a_tolerated_permission_failure_is_warned_not_silent() {
        let source =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/storage/paths.rs"))
                .expect("paths source")
                .replace("\r\n", "\n");
        let start = source
            .find("pub fn ensure_private_dir")
            .expect("ensure_private_dir is where directory permissions are tightened");
        let end = start + source[start..].find("\n}\n").expect("the function body");
        let body = &source[start..end];
        let guard = body
            .find("is_not_supported")
            .expect("the ENOTSUP/EPERM tolerance classification");
        let fatal_return = body[guard..]
            .find("return Err(err);")
            .expect("failures other than ENOTSUP and EPERM still propagate");
        let warn = body[guard..].find("tracing::warn!").expect(
            "a tolerated ENOTSUP/EPERM chmod failure must be warned: silently continuing \
             leaves the directory less private than 0o700 with no trace in the log",
        );
        assert!(
            warn > fatal_return,
            "the warn belongs on the tolerated path, after the fatal early return"
        );
    }

    const UNMIGRATED_JOIN_SITES: &[(&str, &str, usize)] = &[
        ("lib.rs", MODELS_DIR_NAME, 8),
        ("storage/paths.rs", MODELS_DIR_NAME, 2),
        ("storage/paths.rs", EXPORTS_DIR_NAME, 2),
    ];

    #[test]
    fn every_directory_still_built_by_hand_is_declared_here() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let needles = [
            (
                MODELS_DIR_NAME,
                [
                    format!("join({MODELS_DIR_NAME:?})"),
                    "MODELS_DIR_NAME)".to_string(),
                ],
            ),
            (
                EXPORTS_DIR_NAME,
                [
                    format!("join({EXPORTS_DIR_NAME:?})"),
                    "EXPORTS_DIR_NAME)".to_string(),
                ],
            ),
        ];
        let mut counted: std::collections::BTreeMap<(String, &str), usize> = Default::default();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(&root)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                let body = std::fs::read_to_string(&path).expect("read source");
                let lines: Vec<&str> = body.lines().collect();
                let mut index = 0usize;
                while index < lines.len() {
                    let line = lines[index];
                    if line.trim_start().starts_with("#[cfg(test)]") {
                        let opens_a_module = lines
                            .get(index + 1)
                            .map(|next| next.trim_start())
                            .is_some_and(|next| next.starts_with("mod ") && next.ends_with(" {"));
                        if opens_a_module {
                            break;
                        }
                        let mut depth = 0i32;
                        let mut seen_body = false;
                        while index < lines.len() {
                            for byte in lines[index].chars() {
                                match byte {
                                    '{' => {
                                        depth += 1;
                                        seen_body = true;
                                    }
                                    '}' => depth -= 1,
                                    _ => {}
                                }
                            }
                            index += 1;
                            if seen_body && depth <= 0 {
                                break;
                            }
                        }
                        assert!(
                            seen_body,
                            "{relative}: a cfg(test) gate with no braced item would hide every \
                             production line below it"
                        );
                        continue;
                    }
                    for (kind, forms) in &needles {
                        if forms.iter().any(|form| line.contains(form.as_str())) {
                            *counted.entry((relative.clone(), *kind)).or_default() += 1;
                        }
                    }
                    index += 1;
                }
            }
        }

        let declared: std::collections::BTreeMap<(String, &str), usize> = UNMIGRATED_JOIN_SITES
            .iter()
            .map(|(file, kind, count)| (((*file).to_string(), *kind), *count))
            .collect();

        assert_eq!(
            counted, declared,
            "storage::paths::resolve is the only source for the models and exports \
             directories. Every production site that still builds one by hand is declared in \
             UNMIGRATED_JOIN_SITES, per file and per directory, and must shrink to the two \
             pairs inside storage/paths.rs in R-AC part 2; a new site, or a stale entry left \
             behind after one is migrated, fails here."
        );
    }

    fn abs(segment: &str) -> PathBuf {
        std::env::temp_dir().join(segment)
    }

    #[test]
    fn windows_roots_are_not_the_install_folder() {
        for name in [WINDOWS_DATA_DIR_NAME, WINDOWS_CACHE_DIR_NAME] {
            assert!(!name.eq_ignore_ascii_case("txtCaster"), "{name}");
        }
        assert_ne!(WINDOWS_DATA_DIR_NAME, WINDOWS_CACHE_DIR_NAME);
    }

    #[test]
    fn an_unset_override_resolves_to_the_directory_under_the_app_data_dir() {
        let data_dir = abs("txtcaster-paths-default");
        let dirs = defaults(&data_dir);
        assert_eq!(dirs.models_dir(), data_dir.join("models"));
        assert_eq!(dirs.exports_dir(), data_dir.join("exports"));
        assert_eq!(dirs.cache_dir, data_dir.join("cache"));
        assert!(dirs.models.is_default());
        assert!(dirs.exports.is_default());
    }

    #[test]
    fn an_absolute_override_replaces_the_default_and_records_itself() {
        let chosen = abs("txtcaster-paths-chosen");
        let resolved = apply_override(
            abs("txtcaster-paths-default").join("models"),
            Some(chosen.to_string_lossy().into_owned()),
        );
        assert_eq!(resolved.path, chosen);
        assert_eq!(resolved.rejected, None);
        assert!(!resolved.is_default());
    }

    #[test]
    fn a_blank_override_falls_back_to_the_default_rather_than_resolving_to_nothing() {
        let default_path = abs("txtcaster-paths-default").join("models");
        for raw in ["", "   ", "\t\n"] {
            let resolved = apply_override(default_path.clone(), Some(raw.to_string()));
            assert_eq!(resolved.path, default_path, "raw {raw:?}");
            assert_eq!(resolved.rejected, Some(DirRejection::Empty), "raw {raw:?}");
            assert!(resolved.is_default(), "raw {raw:?}");
        }
    }

    #[test]
    fn a_relative_override_falls_back_and_reports_why_instead_of_joining_the_cwd() {
        let default_path = abs("txtcaster-paths-default").join("exports");
        let resolved = apply_override(default_path.clone(), Some("models/tts".to_string()));
        assert_eq!(resolved.path, default_path);
        assert_eq!(resolved.rejected, Some(DirRejection::NotAbsolute));
        assert_eq!(resolved.configured.as_deref(), Some("models/tts"));
    }

    #[test]
    fn an_override_is_trimmed_before_it_is_used_as_a_path() {
        let chosen = abs("txtcaster-paths-trim");
        let raw = format!("  {}\n", chosen.to_string_lossy());
        let resolved = apply_override(abs("txtcaster-paths-default").join("models"), Some(raw));
        assert_eq!(resolved.path, chosen);
        assert_eq!(resolved.rejected, None);
    }

    #[test]
    fn validation_names_every_reason_a_directory_can_be_refused() {
        let root =
            std::env::temp_dir().join(format!("txtcaster-validate-{}", super::super::new_id()));
        std::fs::create_dir_all(&root).expect("root");

        assert_eq!(
            validate_dir(Path::new("relative/dir")),
            Err(DirRejection::NotAbsolute)
        );
        assert_eq!(
            validate_dir(&root.join("absent")),
            Err(DirRejection::Missing)
        );

        let file = root.join("a-file");
        std::fs::write(&file, b"x").expect("file");
        assert_eq!(validate_dir(&file), Err(DirRejection::NotADirectory));

        assert_eq!(validate_dir(&root), Ok(()));

        std::fs::remove_dir_all(&root).ok();
    }

    /// Unix only: Windows does not honor the read-only attribute on
    /// directories (FILE_ATTRIBUTE_READONLY, "This attribute is not honored
    /// on directories" — learn.microsoft.com/en-us/windows/win32/fileio/
    /// file-attribute-constants, checked 2026-10-01), so no permission bit
    /// can make this fixture unwritable there; the real-write probe under
    /// test is what keeps the verdict honest on every platform.
    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_is_refused_by_a_real_write_rather_than_a_permission_bit() {
        let root =
            std::env::temp_dir().join(format!("txtcaster-readonly-{}", super::super::new_id()));
        std::fs::create_dir_all(&root).expect("root");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&root).expect("meta").permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(&root, perms).expect("chmod");

        let verdict = validate_dir(&root);

        let mut restore = std::fs::metadata(&root).expect("meta").permissions();
        restore.set_mode(0o700);
        std::fs::set_permissions(&root, restore).ok();
        std::fs::remove_dir_all(&root).ok();

        match verdict {
            Err(DirRejection::NotWritable(_)) => {}
            other => panic!("expected NotWritable, got {other:?}"),
        }
    }

    #[test]
    fn the_write_probe_leaves_nothing_behind() {
        let root = std::env::temp_dir().join(format!("txtcaster-probe-{}", super::super::new_id()));
        std::fs::create_dir_all(&root).expect("root");
        validate_dir(&root).expect("writable");
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .expect("read")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(leftovers, Vec::<String>::new());
    }

    #[test]
    fn resolve_and_migrate_renames_legacy_when_canonical_is_absent() {
        let base = std::env::temp_dir().join(format!("txtcaster-mig-{}", super::super::new_id()));
        std::fs::create_dir_all(&base).expect("base");
        let legacy = base.join("legacy_app");
        std::fs::create_dir_all(&legacy).expect("legacy");
        std::fs::write(legacy.join("sample.txt"), b"persisted data").expect("write");

        let resolved = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(resolved, base.join("new_app"));
        assert!(resolved.join("sample.txt").is_file());
        assert!(!legacy.exists());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_webview_profile_stays_behind_while_the_app_files_move() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-mig-wv-{}", super::super::new_id()));
        let legacy = base.join("legacy_app");
        std::fs::create_dir_all(legacy.join(WEBVIEW_PROFILE_DIR).join("Default")).expect("profile");
        std::fs::write(
            legacy
                .join(WEBVIEW_PROFILE_DIR)
                .join("Default")
                .join("Local Storage"),
            b"preferences",
        )
        .expect("profile file");
        std::fs::create_dir_all(legacy.join("0a")).expect("shard");
        std::fs::write(legacy.join("0a").join("page.bin"), b"cache").expect("cache");

        let resolved = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(resolved, base.join("new_app"));
        assert!(
            resolved.join("0a").join("page.bin").is_file(),
            "app files move"
        );
        assert!(
            legacy
                .join(WEBVIEW_PROFILE_DIR)
                .join("Default")
                .join("Local Storage")
                .is_file(),
            "the browser profile holding localStorage never moves"
        );
        assert!(!resolved.join(WEBVIEW_PROFILE_DIR).exists());
        assert!(
            !base.join("legacy_app.migrated").exists(),
            "a folder still holding the live profile is not archived"
        );

        let again = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(again, base.join("new_app"));
        assert!(legacy.join(WEBVIEW_PROFILE_DIR).is_dir());
        assert!(!base.join("legacy_app.migrated").exists());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_live_webkit_profile_is_left_alone_on_every_lookup() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-mig-wk-{}", super::super::new_id()));
        let legacy = base.join("legacy_app");
        let canonical = base.join("new_app");
        std::fs::create_dir_all(legacy.join("localstorage")).expect("profile");
        std::fs::write(
            legacy
                .join("localstorage")
                .join("tauri_localhost_0.localstorage"),
            b"preferences",
        )
        .expect("local storage");
        std::fs::write(legacy.join("hsts-storage.sqlite"), b"hsts").expect("hsts");

        for _ in 0..3 {
            assert_eq!(
                resolve_and_migrate_dir(&base, "legacy_app", "new_app"),
                canonical
            );
            std::fs::write(
                legacy
                    .join("localstorage")
                    .join("tauri_localhost_0.localstorage-wal"),
                b"written while running",
            )
            .expect("wal");
        }

        assert_eq!(
            std::fs::read(
                legacy
                    .join("localstorage")
                    .join("tauri_localhost_0.localstorage")
            )
            .expect("local storage stays"),
            b"preferences"
        );
        assert!(legacy
            .join("localstorage")
            .join("tauri_localhost_0.localstorage-wal")
            .is_file());
        assert!(legacy.join("hsts-storage.sqlite").is_file());
        assert!(!canonical.join("localstorage").exists());
        assert!(!base.join("legacy_app.migrated").exists());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn app_data_moves_out_from_beside_a_webkit_profile() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-mig-wk2-{}", super::super::new_id()));
        let legacy = base.join("legacy_app");
        std::fs::create_dir_all(legacy.join("localstorage")).expect("profile");
        std::fs::create_dir_all(legacy.join("books")).expect("books");
        std::fs::write(legacy.join("books").join("a.pdf"), b"book").expect("book");

        let resolved = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert!(resolved.join("books").join("a.pdf").is_file());
        assert!(legacy.join("localstorage").is_dir());
        assert!(!legacy.join("books").exists());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn leftovers_beside_the_webview_profile_are_archived_once() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-mig-wv2-{}", super::super::new_id()));
        let legacy = base.join("legacy_app");
        let canonical = base.join("new_app");
        std::fs::create_dir_all(legacy.join(WEBVIEW_PROFILE_DIR).join("Default")).expect("profile");
        std::fs::create_dir_all(legacy.join("0a")).expect("legacy shard");
        std::fs::write(legacy.join("0a").join("page.bin"), b"old").expect("legacy cache");
        std::fs::write(legacy.join("index"), b"old index").expect("legacy index");
        std::fs::create_dir_all(canonical.join("0a")).expect("canonical shard");
        std::fs::write(canonical.join("0a").join("page.bin"), b"new").expect("canonical cache");
        std::fs::write(canonical.join("index"), b"new index").expect("canonical index");

        let resolved = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(resolved, canonical);
        assert_eq!(
            std::fs::read(canonical.join("0a").join("page.bin")).expect("canonical cache"),
            b"new",
            "the non-destructive merge keeps the canonical copy"
        );
        assert_eq!(
            std::fs::read(canonical.join("index")).expect("canonical index"),
            b"new index"
        );
        let remaining: Vec<_> = std::fs::read_dir(&legacy)
            .expect("legacy")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(
            remaining,
            vec![std::ffi::OsString::from(WEBVIEW_PROFILE_DIR)],
            "only the live profile stays in the legacy folder"
        );
        let archive = base.join("legacy_app.migrated");
        assert_eq!(
            std::fs::read(archive.join("0a").join("page.bin")).expect("archived cache"),
            b"old"
        );
        assert_eq!(
            std::fs::read(archive.join("index")).expect("archived index"),
            b"old index"
        );

        let archived_before: Vec<_> = std::fs::read_dir(&archive)
            .expect("archive")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        let again = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(again, canonical);
        let archived_after: Vec<_> = std::fs::read_dir(&archive)
            .expect("archive")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(
            archived_after, archived_before,
            "a later lookup does no migration work"
        );
        assert!(legacy.join(WEBVIEW_PROFILE_DIR).is_dir());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn resolve_and_migrate_keeps_canonical_when_both_or_neither_exist() {
        let base = std::env::temp_dir().join(format!("txtcaster-mig2-{}", super::super::new_id()));
        std::fs::create_dir_all(&base).expect("base");

        let resolved = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(resolved, base.join("new_app"));

        std::fs::create_dir_all(base.join("new_app")).expect("canonical");
        std::fs::create_dir_all(base.join("legacy_app")).expect("legacy");
        let resolved2 = resolve_and_migrate_dir(&base, "legacy_app", "new_app");
        assert_eq!(resolved2, base.join("new_app"));

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn granular_merge_moves_subdirectories_and_files_without_overwriting_canonical() {
        let base = std::env::temp_dir().join(format!("txtcaster-merge-{}", super::super::new_id()));
        let canonical = base.join("canonical");
        let legacy = base.join("legacy");
        std::fs::create_dir_all(canonical.join("models").join("model-b"))
            .expect("canonical model-b");
        std::fs::create_dir_all(canonical.join("exports")).expect("canonical exports");
        std::fs::write(
            canonical.join("models").join("model-b").join("weights.bin"),
            b"canonical weights b",
        )
        .expect("write b");
        std::fs::write(
            canonical.join("exports").join("audio.mp3"),
            b"canonical audio",
        )
        .expect("write canonical audio");

        std::fs::create_dir_all(legacy.join("models").join("model-a")).expect("legacy model-a");
        std::fs::create_dir_all(legacy.join("books")).expect("legacy books");
        std::fs::create_dir_all(legacy.join("exports")).expect("legacy exports");
        std::fs::write(
            legacy.join("models").join("model-a").join("weights.bin"),
            b"legacy weights a",
        )
        .expect("write a");
        std::fs::write(legacy.join("books").join("book1.epub"), b"book 1 content")
            .expect("write book");
        std::fs::write(
            legacy.join("exports").join("audio.mp3"),
            b"legacy audio overwrite attempt",
        )
        .expect("write legacy audio");

        let resolved = resolve_and_migrate_dir(&base, "legacy", "canonical");
        assert_eq!(resolved, canonical);

        // Model A moved from legacy into canonical
        assert!(canonical
            .join("models")
            .join("model-a")
            .join("weights.bin")
            .is_file());
        // Model B in canonical was preserved
        assert!(canonical
            .join("models")
            .join("model-b")
            .join("weights.bin")
            .is_file());
        // Books moved from legacy into canonical
        assert!(canonical.join("books").join("book1.epub").is_file());
        // Canonical export was NOT overwritten
        assert_eq!(
            std::fs::read(canonical.join("exports").join("audio.mp3")).expect("read"),
            b"canonical audio"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn database_reconciliation_backs_up_empty_canonical_and_migrates_populated_legacy() {
        let base = std::env::temp_dir().join(format!("txtcaster-dbrec-{}", super::super::new_id()));
        let canonical = base.join("canonical");
        let legacy = base.join("legacy");
        std::fs::create_dir_all(&canonical).expect("canonical");
        std::fs::create_dir_all(&legacy).expect("legacy");

        // Create canonical db with 0 user content but 1 seed metadata row in app_setting
        let can_db = canonical.join("db.sqlite");
        {
            let opts = SqliteConnectOptions::new()
                .filename(&can_db)
                .create_if_missing(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            sqlx::query("CREATE TABLE book (id TEXT PRIMARY KEY, title TEXT)")
                .execute(&mut conn)
                .await
                .expect("create book");
            sqlx::query("CREATE TABLE app_setting (key TEXT PRIMARY KEY, value TEXT)")
                .execute(&mut conn)
                .await
                .expect("create app_setting");
            sqlx::query("INSERT INTO app_setting VALUES ('theme', 'dark')")
                .execute(&mut conn)
                .await
                .expect("insert seed setting");
        }

        // Create populated legacy db with user content (books)
        let leg_db = legacy.join("db.sqlite");
        {
            let opts = SqliteConnectOptions::new()
                .filename(&leg_db)
                .create_if_missing(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            sqlx::query("CREATE TABLE book (id TEXT PRIMARY KEY, title TEXT)")
                .execute(&mut conn)
                .await
                .expect("create");
            sqlx::query("INSERT INTO book VALUES ('b1', 'Book 1'), ('b2', 'Book 2')")
                .execute(&mut conn)
                .await
                .expect("insert");
        }

        reconcile_database(&canonical, &legacy).expect("reconcile");

        // Canonical .bak was created
        assert!(canonical.join("db.sqlite.bak").is_file());
        // Canonical db now has the 2 rows from legacy
        {
            let opts = SqliteConnectOptions::new()
                .filename(&can_db)
                .read_only(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM book")
                .fetch_one(&mut conn)
                .await
                .expect("count");
            assert_eq!(count, 2);
        }

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn database_reconciliation_preserves_populated_canonical() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-dbrec2-{}", super::super::new_id()));
        let canonical = base.join("canonical");
        let legacy = base.join("legacy");
        std::fs::create_dir_all(&canonical).expect("canonical");
        std::fs::create_dir_all(&legacy).expect("legacy");

        // Create populated canonical db with 1 book
        let can_db = canonical.join("db.sqlite");
        {
            let opts = SqliteConnectOptions::new()
                .filename(&can_db)
                .create_if_missing(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            sqlx::query("CREATE TABLE book (id TEXT PRIMARY KEY, title TEXT)")
                .execute(&mut conn)
                .await
                .expect("create");
            sqlx::query("INSERT INTO book VALUES ('c1', 'Canonical Book')")
                .execute(&mut conn)
                .await
                .expect("insert");
        }

        // Create legacy db with 2 books
        let leg_db = legacy.join("db.sqlite");
        {
            let opts = SqliteConnectOptions::new()
                .filename(&leg_db)
                .create_if_missing(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            sqlx::query("CREATE TABLE book (id TEXT PRIMARY KEY, title TEXT)")
                .execute(&mut conn)
                .await
                .expect("create");
            sqlx::query("INSERT INTO book VALUES ('b1', 'Book 1'), ('b2', 'Book 2')")
                .execute(&mut conn)
                .await
                .expect("insert");
        }

        reconcile_database(&canonical, &legacy).expect("reconcile");

        // Canonical .bak was NOT created
        assert!(!canonical.join("db.sqlite.bak").exists());
        // Canonical db still has its own 1 row
        {
            let opts = SqliteConnectOptions::new()
                .filename(&can_db)
                .read_only(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM book")
                .fetch_one(&mut conn)
                .await
                .expect("count");
            assert_eq!(count, 1);
        }

        std::fs::remove_dir_all(&base).ok();
    }

    /// The live half of the job-linkage query: `rewrite_db_paths` reaches
    /// production through `resolve_and_migrate_dir`, and this branch used to be
    /// covered by no test at all — the reconciliation fixtures create `book` and
    /// `app_setting` and never an `export` row.
    ///
    /// `%` and `_` are what the filename sanitizer actually produces: `%` is
    /// kept verbatim, and `/ \ : * ? " < > |` become `_`. Both are LIKE
    /// wildcards, so each row below sits one character away from a *different*
    /// cast's output path and must stay unattributed.
    #[tokio::test]
    async fn a_wildcard_in_a_migrated_export_path_cannot_claim_another_casts_job() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-dbrec3-{}", super::super::new_id()));
        let legacy = base.join("legacy");
        std::fs::create_dir_all(&legacy).expect("legacy");
        let legacy_db = legacy.join("db.sqlite");
        {
            let opts = SqliteConnectOptions::new()
                .filename(&legacy_db)
                .create_if_missing(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts)
                .await
                .expect("connect");
            sqlx::raw_sql(
                "CREATE TABLE book (id TEXT PRIMARY KEY, title TEXT, source_path TEXT, \
                                     cover_path TEXT, ingest_path TEXT); \
                 CREATE TABLE render_job (id TEXT PRIMARY KEY, book_id TEXT, output_paths_json TEXT); \
                 CREATE TABLE export (id TEXT PRIMARY KEY, book_id TEXT, job_id TEXT, path TEXT);",
            )
            .execute(&mut conn)
            .await
            .expect("create");
        }
        let legacy_str = legacy.to_string_lossy().to_string();
        // One export per row, one job per row, so no row has a second candidate:
        // a wildcard that over-matches can only ever be seen as *that* row taking
        // the wrong job, never as a `LIMIT 1` race between two right answers.
        let rows = [
            // `My_Book` and `MyXBook`: `_` is "any one character" to LIKE.
            (
                "e-us",
                "My_Book - Ch 1.mp3",
                "j-us",
                "MyXBook - Ch 1.mp3",
                "b-us",
            ),
            // `50%` and `50X`: `%` is "any run" to LIKE.
            (
                "e-pct",
                "Shrink 30% Off - Ch 1.mp3",
                "j-pct",
                "Shrink 30X Off - Ch 1.mp3",
                "b-pct",
            ),
            // The owner itself, whose path its own job states literally: the
            // positive control, so "no wildcards at all" fails here too.
            (
                "e-own",
                "Growth 50% Faster - Ch 1.mp3",
                "j-own",
                "Growth 50% Faster - Ch 1.mp3",
                "b-own",
            ),
        ];
        {
            let mut conn = sqlx::SqliteConnection::connect_with(
                &SqliteConnectOptions::new().filename(&legacy_db),
            )
            .await
            .expect("reconnect");
            for (export_id, export_name, job_id, job_name, book_id) in rows {
                sqlx::query(
                    "INSERT INTO render_job (id, book_id, output_paths_json) VALUES (?, ?, ?)",
                )
                .bind(job_id)
                .bind(book_id)
                .bind(serde_json::json!([format!("{legacy_str}/exports/{job_name}")]).to_string())
                .execute(&mut conn)
                .await
                .expect("job");
                sqlx::query(
                    "INSERT INTO export (id, book_id, job_id, path) VALUES (?, NULL, NULL, ?)",
                )
                .bind(export_id)
                .bind(format!("{legacy_str}/exports/{export_name}"))
                .execute(&mut conn)
                .await
                .expect("export");
            }
        }

        let resolved = resolve_and_migrate_dir(&base, "legacy", "canonical");
        assert_eq!(resolved, base.join("canonical"), "the migration must run");

        let opts = SqliteConnectOptions::new()
            .filename(resolved.join("db.sqlite"))
            .read_only(true);
        let mut conn = sqlx::SqliteConnection::connect_with(&opts)
            .await
            .expect("reopen");
        for (export_id, export_name, job_id, _job_name, book_id) in rows {
            let (claimed_job, claimed_book): (Option<String>, Option<String>) =
                sqlx::query_as("SELECT job_id, book_id FROM export WHERE id = ?")
                    .bind(export_id)
                    .fetch_one(&mut conn)
                    .await
                    .expect("row");
            if export_id == "e-own" {
                assert_eq!(
                    claimed_job.as_deref(),
                    Some(job_id),
                    "{export_id} states its path in {job_id}'s own output_paths_json, and a \
                     literal substring match still finds it"
                );
                assert_eq!(claimed_book.as_deref(), Some(book_id));
            } else {
                assert_eq!(
                    claimed_job, None,
                    "{export_name} is not in {job_id}'s output_paths_json; the `_` and `%` in it \
                     must not be read as wildcards that match a neighbouring name"
                );
                assert_eq!(claimed_book, None);
            }
        }

        drop(conn);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn zero_byte_placeholder_is_replaced_by_non_empty_legacy_file() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-zerobyte-{}", super::super::new_id()));
        let canonical = base.join("canonical");
        let legacy = base.join("legacy");
        std::fs::create_dir_all(canonical.join("models").join("nested")).expect("canonical nested");
        std::fs::create_dir_all(legacy.join("models").join("nested")).expect("legacy nested");

        // 0-byte placeholders in canonical
        std::fs::write(canonical.join("root_dummy.txt"), b"").expect("write 0B root");
        std::fs::write(
            canonical.join("models").join("nested").join("model.bin"),
            b"",
        )
        .expect("write 0B nested");

        // Non-empty legitimate files in legacy
        std::fs::write(legacy.join("root_dummy.txt"), b"legacy root content")
            .expect("write legacy root");
        std::fs::write(
            legacy.join("models").join("nested").join("model.bin"),
            b"5GB model binary payload",
        )
        .expect("write legacy nested");

        let resolved = resolve_and_migrate_dir(&base, "legacy", "canonical");
        assert_eq!(resolved, canonical);

        // Assert 0-byte files were replaced with non-empty legacy contents
        assert_eq!(
            std::fs::read(canonical.join("root_dummy.txt")).expect("read root"),
            b"legacy root content"
        );
        assert_eq!(
            std::fs::read(canonical.join("models").join("nested").join("model.bin"))
                .expect("read nested"),
            b"5GB model binary payload"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// The de-duplicating `DELETE` in `rewrite_db_paths` means "this row's path
    /// would land on another row's path once the legacy prefix is rewritten", and
    /// its two halves have to test that together.
    ///
    /// It used to test only the second half. `LIKE '%<legacy>%'` treats the `_`
    /// in a routine Windows profile path — `C:\Users\John_Doe\…` — as "any one
    /// character", so it also selected rows that never contained the legacy
    /// string; `replace()` then returned those rows unchanged, and
    /// `replace(...) IN (SELECT path FROM export)` was satisfied by the row
    /// selecting itself. The statement degenerated into
    /// `DELETE FROM export WHERE path LIKE '%<legacy>%'`, and every
    /// wildcard-over-matching row removed itself, silently.
    #[tokio::test]
    async fn a_row_that_only_wildcard_matches_the_legacy_path_survives_the_migration() {
        let base =
            std::env::temp_dir().join(format!("txtcaster-wilddel-{}", super::super::new_id()));
        let legacy = base.join("legacy_app");
        std::fs::create_dir_all(&legacy).expect("legacy");
        let legacy_str = legacy.to_string_lossy().to_string();
        let canonical_str = base.join("canonical_app").to_string_lossy().to_string();
        let lookalike = legacy_str.replace("_", "X");
        let legacy_db = legacy.join("db.sqlite");
        {
            let mut conn = sqlx::SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&legacy_db)
                    .create_if_missing(true),
            )
            .await
            .expect("connect");
            sqlx::raw_sql(
                "CREATE TABLE book (id TEXT PRIMARY KEY, title TEXT, source_path TEXT, \
                                     cover_path TEXT, ingest_path TEXT); \
                 CREATE TABLE render_job (id TEXT PRIMARY KEY, book_id TEXT, output_paths_json TEXT); \
                 CREATE TABLE export (id TEXT PRIMARY KEY, book_id TEXT, job_id TEXT, \
                                      path TEXT NOT NULL UNIQUE);",
            )
            .execute(&mut conn)
            .await
            .expect("create");
            for (id, path) in [
                // `_` is "any one character" to LIKE, so this row matches the
                // legacy pattern without containing the legacy string at all.
                ("e-wild", format!("{lookalike}/exports/a.mp3")),
                // A genuinely legacy row whose rewrite collides with the
                // canonical row below it: the delete's actual purpose.
                ("e-collide", format!("{legacy_str}/exports/collide.mp3")),
                (
                    "e-canonical",
                    format!("{canonical_str}/exports/collide.mp3"),
                ),
                // A genuinely legacy row with nowhere to collide: rewritten and
                // kept.
                ("e-clean", format!("{legacy_str}/exports/clean.mp3")),
            ] {
                sqlx::query(
                    "INSERT INTO export (id, book_id, job_id, path) \
                     VALUES (?, NULL, NULL, ?)",
                )
                .bind(id)
                .bind(&path)
                .execute(&mut conn)
                .await
                .expect("export");
            }
            sqlx::query(
                "INSERT INTO render_job (id, book_id, output_paths_json) \
                 VALUES ('j1', 'b1', ?)",
            )
            .bind(serde_json::json!([format!("{legacy_str}/exports/clean.mp3")]).to_string())
            .execute(&mut conn)
            .await
            .expect("job");
        }

        let resolved = resolve_and_migrate_dir(&base, "legacy_app", "canonical_app");
        assert_eq!(
            resolved,
            base.join("canonical_app"),
            "the migration must run"
        );

        let mut conn = sqlx::SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(resolved.join("db.sqlite"))
                .read_only(true),
        )
        .await
        .expect("reopen");
        let survivor: Option<String> =
            sqlx::query_scalar("SELECT path FROM export WHERE id = 'e-wild'")
                .fetch_optional(&mut conn)
                .await
                .expect("query")
                .flatten();
        assert!(
            survivor.is_some(),
            "`{lookalike}/exports/a.mp3` is not under the legacy directory, and the `_` in \
             `legacy_app` must not make the delete read it as one: `replace()` leaves it \
             unchanged, so it satisfies the collision test with itself and deletes itself"
        );
        assert_eq!(
            survivor.unwrap(),
            format!("{lookalike}/exports/a.mp3"),
            "and a row that was never rewritten is still exactly itself"
        );

        let collided: i64 =
            sqlx::query_scalar("SELECT count(*) FROM export WHERE id = 'e-collide'")
                .fetch_one(&mut conn)
                .await
                .expect("count");
        assert_eq!(
            collided, 0,
            "the row the delete exists for: rewriting it would land on `e-canonical`, which \
             `export.path`'s UNIQUE index refuses"
        );

        let clean: Option<String> =
            sqlx::query_scalar("SELECT path FROM export WHERE id = 'e-clean'")
                .fetch_one(&mut conn)
                .await
                .expect("row");
        assert_eq!(
            clean.as_deref(),
            Some(format!("{canonical_str}/exports/clean.mp3").as_str()),
            "a genuinely legacy row with no collision is rewritten, not deleted"
        );

        drop(conn);
        std::fs::remove_dir_all(&base).ok();
    }
}
