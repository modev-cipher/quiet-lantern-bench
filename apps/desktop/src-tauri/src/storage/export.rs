use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use super::{new_id, now_ms};

#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub id: String,
    pub book_id: Option<String>,
    pub job_id: Option<String>,
    pub path: String,
    pub bytes: i64,
    pub format: String,
    pub mode: String,
    pub chapter_digest: Option<String>,
    pub created_at: i64,
}

pub async fn record(
    pool: &SqlitePool,
    book_id: &str,
    job_id: &str,
    path: &str,
    format: &str,
    mode: &str,
    chapter_digest: &str,
) -> Result<String, sqlx::Error> {
    let id = new_id();
    let bytes = file_bytes(Path::new(path));
    let created_at = now_ms();
    let row = sqlx::query!(
        r#"INSERT INTO export (id, book_id, job_id, path, bytes, format, mode,
                                 chapter_digest, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT(path) DO UPDATE SET
              book_id = excluded.book_id,
              job_id = excluded.job_id,
              bytes = excluded.bytes,
              format = excluded.format,
              mode = excluded.mode,
              chapter_digest = excluded.chapter_digest,
              created_at = excluded.created_at
           RETURNING id as "id!""#,
        id,
        book_id,
        job_id,
        path,
        bytes,
        format,
        mode,
        chapter_digest,
        created_at
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

pub async fn list_exports(pool: &SqlitePool) -> Result<Vec<Export>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT id as "id!", book_id, job_id, path as "path!", bytes as "bytes!",
                  format as "format!", mode as "mode!", chapter_digest,
                  created_at as "created_at!"
           FROM export ORDER BY created_at DESC"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Export {
            id: row.id,
            book_id: row.book_id,
            job_id: row.job_id,
            path: row.path,
            bytes: row.bytes,
            format: row.format,
            mode: row.mode,
            chapter_digest: row.chapter_digest,
            created_at: row.created_at,
        })
        .collect())
}

pub async fn backfill_orphan_links(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    // An exact `json_each` element match rather than `LIKE '%'||export.path||'%'`,
    // for the same reason as in `adopt_orphans`: the path is user data and `_` and
    // `%` in it are LIKE wildcards, so a wildcard pattern can link this export to
    // another cast's job. A substring test on the raw JSON is no better: JSON
    // escapes the `\` of a Windows path, so it never matches there.
    let result = sqlx::query(
        r#"UPDATE export
           SET job_id = (SELECT r.id FROM render_job r WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.output_paths_json) THEN r.output_paths_json ELSE '[]' END) o WHERE o.value = export.path) LIMIT 1),
               book_id = (SELECT r.book_id FROM render_job r WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.output_paths_json) THEN r.output_paths_json ELSE '[]' END) o WHERE o.value = export.path) LIMIT 1)
           WHERE (export.job_id IS NULL OR export.job_id = '' OR export.book_id IS NULL OR export.book_id = '')
             AND EXISTS (SELECT 1 FROM render_job r WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.output_paths_json) THEN r.output_paths_json ELSE '[]' END) o WHERE o.value = export.path))"#
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn get_export(pool: &SqlitePool, id: &str) -> Result<Option<Export>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT id as "id!", book_id, job_id, path as "path!", bytes as "bytes!",
                  format as "format!", mode as "mode!", chapter_digest,
                  created_at as "created_at!"
           FROM export WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| Export {
        id: row.id,
        book_id: row.book_id,
        job_id: row.job_id,
        path: row.path,
        bytes: row.bytes,
        format: row.format,
        mode: row.mode,
        chapter_digest: row.chapter_digest,
        created_at: row.created_at,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChapterEvidence {
    Unattributed,
    NoRows,
    Digest(String),
}

pub fn is_stale(recorded: Option<&str>, evidence: &ChapterEvidence) -> bool {
    match (recorded, evidence) {
        (_, ChapterEvidence::Unattributed) => false,
        (None, ChapterEvidence::NoRows) => false,
        (None, ChapterEvidence::Digest(_)) => true,
        (Some(_), ChapterEvidence::NoRows) => true,
        (Some(recorded), ChapterEvidence::Digest(current)) => recorded != current,
    }
}

pub async fn chapter_evidence(
    pool: &SqlitePool,
    job_id: Option<&str>,
) -> Result<ChapterEvidence, sqlx::Error> {
    let Some(job_id) = job_id else {
        return Ok(ChapterEvidence::Unattributed);
    };
    let rows = super::chapter_render::list(pool, job_id).await?;
    Ok(if rows.is_empty() {
        ChapterEvidence::NoRows
    } else {
        ChapterEvidence::Digest(super::chapter_render::digest(&rows))
    })
}

pub async fn evidence_for_jobs(
    pool: &SqlitePool,
    job_ids: &[&str],
) -> Result<std::collections::HashMap<String, ChapterEvidence>, sqlx::Error> {
    let mut evidence: std::collections::HashMap<String, ChapterEvidence> = job_ids
        .iter()
        .map(|job_id| ((*job_id).to_string(), ChapterEvidence::NoRows))
        .collect();
    if job_ids.is_empty() {
        return Ok(evidence);
    }
    for (job_id, digest) in super::chapter_render::digests_for_jobs(pool, job_ids).await? {
        evidence.insert(job_id, ChapterEvidence::Digest(digest));
    }
    Ok(evidence)
}

pub async fn is_path_stale(pool: &SqlitePool, path: &str) -> Result<bool, sqlx::Error> {
    let Some(export) = get_by_path(pool, path).await? else {
        return Ok(false);
    };
    let evidence = chapter_evidence(pool, export.job_id.as_deref()).await?;
    Ok(is_stale(export.chapter_digest.as_deref(), &evidence))
}

pub async fn get_by_path(pool: &SqlitePool, path: &str) -> Result<Option<Export>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT id as "id!", book_id, job_id, path as "path!", bytes as "bytes!",
                  format as "format!", mode as "mode!", chapter_digest,
                  created_at as "created_at!"
           FROM export WHERE path = ?"#,
        path
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| Export {
        id: row.id,
        book_id: row.book_id,
        job_id: row.job_id,
        path: row.path,
        bytes: row.bytes,
        format: row.format,
        mode: row.mode,
        chapter_digest: row.chapter_digest,
        created_at: row.created_at,
    }))
}

pub async fn paths_for_book(pool: &SqlitePool, book_id: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT path as "path!" FROM export WHERE book_id = ?"#,
        book_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|row| row.path).collect())
}

pub async fn delete_export(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM export WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_by_path(pool: &SqlitePool, path: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM export WHERE path = ?", path)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub deleted: usize,
    pub failed: usize,
}

pub fn resolve_within(exports_dir: &Path, path: &str) -> Result<Option<PathBuf>, String> {
    let candidate = Path::new(path);
    if !candidate.exists() {
        return Ok(None);
    }
    let resolved = candidate
        .canonicalize()
        .map_err(|err| format!("cannot resolve export path: {err}"))?;
    let root = canonical_exports_root(exports_dir)?;
    if !resolved.starts_with(&root) {
        return Err(format!("export '{path}' is outside the exports directory"));
    }
    Ok(Some(resolved))
}

fn canonical_exports_root(exports_dir: &Path) -> Result<PathBuf, String> {
    exports_dir
        .canonicalize()
        .map_err(|err| format!("cannot resolve exports directory: {err}"))
}

/// Resolve the path a *new* export file would occupy, before it exists.
///
/// `resolve_within` answers that question for a file that is already there. A
/// write has no file yet, so it gets the same containment answer from the two
/// halves that make it: `name` must be one ordinary path component — which is
/// what refuses `..`, an empty name and anything carrying a separator, on each
/// platform's own component rules — and the directory it is joined to is the
/// canonical exports root. Joining one component onto a canonical root cannot
/// leave it, so containment holds by construction rather than by a check that
/// could be skipped.
pub fn resolve_new_file_within(exports_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let mut components = Path::new(name).components();
    let single = matches!(components.next(), Some(std::path::Component::Normal(part))
        if part == name);
    if !single || components.next().is_some() {
        return Err(format!(
            "export file name '{name}' is not a single path component"
        ));
    }
    Ok(canonical_exports_root(exports_dir)?.join(name))
}

pub const PARTIAL_SUFFIX: &str = ".partial";
pub const EXPORT_PARTIAL_SUFFIX: &str = ".mp3.partial";

pub fn is_partial_export(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(EXPORT_PARTIAL_SUFFIX))
}

pub fn partial_sibling(export_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{PARTIAL_SUFFIX}", export_path.to_string_lossy()))
}

/// Drop the abandoned sibling of an encode, and nothing else.
///
/// No prune here. The renderer's one caller has just created this cast directory
/// and encodes into it next, so pruning would delete the directory the encode
/// needs — the one state that reaches this function with an already-created
/// parent. A directory emptied here holds nothing but an interrupted encode's
/// remains, and the startup sweep collects those.
pub fn remove_partial_within(exports_dir: &Path, export_path: &Path) -> bool {
    let sibling = partial_sibling(export_path);
    match resolve_within(exports_dir, &sibling.to_string_lossy()) {
        Ok(Some(target)) => remove_export_file(&target),
        Ok(None) => true,
        Err(reason) => {
            tracing::warn!("export cleanup refused: {reason}");
            false
        }
    }
}

/// Delete every abandoned encode under the exports root, then prune what that
/// emptied.
///
/// Collected before anything is removed, because pruning takes a directory and a
/// directory taken while the walk is still reading it turns the rest of that
/// directory's iteration into errors.
pub fn sweep_partials(exports_dir: &Path) -> usize {
    let mut abandoned: Vec<PathBuf> = Vec::new();
    walk_export_files(exports_dir, 0, &mut |path| {
        if is_partial_export(path) {
            abandoned.push(path.to_path_buf());
        }
    });
    let mut removed = 0;
    for path in abandoned {
        if remove_export_file(&path) {
            removed += 1;
            prune_emptied_cast_dir(exports_dir, &path);
        }
    }
    if let Ok(entries) = std::fs::read_dir(exports_dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                prune_emptied_cast_dir(exports_dir, &entry.path().join("removed"));
            }
        }
    }
    removed
}

/// Remove one export file: the one asked for, the abandoned sibling it may
/// still carry, and the cast directory if the pair emptied it.
///
/// The sibling goes with it because it keeps the cast directory occupied, and a
/// directory this sweep or any cleanup has to remove is not a directory any of
/// them can remove. The prune also runs on the `Ok(None)` branch: a file the
/// user deleted in their own file manager leaves exactly the same hollow
/// directory, and that branch is the only one which sees it.
///
/// This takes the sibling **unconditionally**, which is only safe for a caller
/// that cannot be running while an encode owns that sibling. `lib.rs`'s
/// `delete_export` can: it takes no mutation gate, so it runs beside a live
/// render, and on Linux the `unlink` below would succeed against the file ffmpeg
/// has open — the encode finishes, its `rename` fails `ENOENT`, and the job
/// reports "export could not be published". Callers that can overlap a render
/// use [`remove_within_guarded`], which asks the job table first.
pub fn remove_within(exports_dir: &Path, path: &str) -> bool {
    remove_resolved_within(exports_dir, path, true)
}

/// [`remove_within`], but the `.partial` sibling and an emptied cast directory
/// survive whenever a job is live.
///
/// The sibling is the renderer's own output path: `orchestrator/render.rs`
/// hands ffmpeg `<finished>.partial` and renames it over `<finished>` when the
/// encode ends. Unlinking it under a running encode is not a cleanup, it is a
/// lost publish — and the failure is platform-divergent, because Windows refuses
/// the `remove_file` with `ERROR_SHARING_VIOLATION` and Linux does not.
///
/// The test for "a live encode owns it" is the job table, because nothing on
/// disk distinguishes an abandoned sibling from one ffmpeg is writing: same
/// path, same name, both non-empty. It is sound at any worker count — a
/// non-terminal job is the only state that can precede an encode — and
/// single-worker (`docs/SINGLE_WORKER_ASSUMPTIONS.md`) only bounds how often it
/// over-selects. `Paused` counts as live for the same reason: it resumes, and
/// guessing wrong either way is asymmetric — one costs a `.partial` the next
/// launch's sweep collects, the other costs the encode.
pub async fn remove_within_guarded(pool: &SqlitePool, exports_dir: &Path, path: &str) -> bool {
    match live_encode_exists(pool).await {
        Ok(true) => remove_resolved_within(exports_dir, path, false),
        Ok(false) => remove_resolved_within(exports_dir, path, true),
        // Fail closed on the sibling: the published file is what the user asked
        // to delete, and the sweep collects a sibling nobody claimed.
        Err(err) => {
            tracing::warn!("export cleanup could not read job liveness: {err}");
            remove_resolved_within(exports_dir, path, false)
        }
    }
}

/// Whether any job can still be encoding into the exports directory.
///
/// `render_job.state` is `TEXT NOT NULL`, so a row with a state outside the
/// terminal set is a row the worker has not finished with.
pub async fn live_encode_exists(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM render_job WHERE state NOT IN ('DONE', 'CANCELLED', 'FAILED')",
    )
    .fetch_one(pool)
    .await?;
    Ok(live > 0)
}

fn remove_resolved_within(exports_dir: &Path, path: &str, no_live_encode: bool) -> bool {
    match resolve_within(exports_dir, path) {
        Ok(Some(target)) => {
            if !remove_export_file(&target) {
                return false;
            }
            if no_live_encode {
                remove_partial_within(exports_dir, &target);
                prune_emptied_cast_dir(exports_dir, &target);
            }
            true
        }
        Ok(None) => {
            if no_live_encode {
                prune_emptied_cast_dir(exports_dir, Path::new(path));
            }
            true
        }
        Err(reason) => {
            tracing::warn!("export cleanup refused: {reason}");
            false
        }
    }
}

fn remove_export_file(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("export cleanup failed for {}: {err}", path.display());
            false
        }
    }
}

/// Length of a cast directory name: `orchestrator::render::cast_key` returns
/// `hex::encode(sha256(..))`, and a SHA-256 digest is 32 bytes as 64 characters.
const CAST_KEY_NAME_LEN: usize = 64;

/// Whether this app created the directory of that name.
///
/// The only directory the app makes under the exports root is a cast directory,
/// and it is named after a `cast_key`. So the name is the ownership record, and
/// the shape it has is fixed: 64 lowercase hex characters, never a dot, never a
/// separator. This is deliberately **stricter** than [`is_cast_directory`],
/// which answers a different question — see the two rules side by side below.
pub fn is_app_cast_dir_name(name: &str) -> bool {
    name.len() == CAST_KEY_NAME_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Drop the cast directory a removed export left behind, if it is one of ours
/// and now holds nothing.
///
/// Takes the file whose removal emptied it, not the directory, because the two
/// callers reach it differently: one holds a path it resolved, the other only
/// has the name the row recorded, which may name a file that is already gone.
/// The parent is canonicalized either way.
///
/// Two tests then have to pass. The parent must be a direct child of the exports
/// root, which is the only *level* the app creates: a prune that descended
/// further would walk into a tree the user put there. And the directory's own
/// name must be a `cast_key`, which is what makes it *ours*.
///
/// The level test alone is not enough, and the exports root is precisely where a
/// user keeps a tree: `adopt_orphans` deliberately walks every non-dot
/// directory, so `<exports>/My Music/01 - my song.mp3` reaches the Audios shelf
/// as a row, and deleting that row emptied a directory the app never made. Depth
/// is a bound on how far the app reaches; the name is the only thing that says
/// who put it there. Deletion is held to the strict rule and adoption is not,
/// because adoption asks "is this a file the user should see" — true of anything
/// they parked in the exports directory — while deletion asks "did we make
/// this", which is true of a `cast_key` and of nothing else.
///
/// An emptied directory is also not merely untidy: it is invisible to every row
/// and every sweep, so a cast deleted through the UI would leave a hollow name on
/// disk that nothing in the app can account for. Both routes that remove a
/// directory — this prune, and `sweep_partials` calling it — are therefore
/// narrowed here, once, rather than at each call site.
fn prune_emptied_cast_dir(exports_dir: &Path, removed: &Path) {
    let Some(parent) = removed.parent() else {
        return;
    };
    let (Ok(root), Ok(parent)) = (exports_dir.canonicalize(), parent.canonicalize()) else {
        return;
    };
    if parent.parent() != Some(root.as_path()) {
        return;
    }
    if !parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_app_cast_dir_name)
    {
        return;
    }
    let Ok(mut entries) = std::fs::read_dir(&parent) else {
        return;
    };
    if entries.next().is_some() {
        return;
    }
    let _ = std::fs::remove_dir(&parent);
}

/// Remove export files and their rows after the owning book has gone.
///
/// `export.book_id` and `export.job_id` are ON DELETE **SET NULL**, not cascade, so deleting
/// a book leaves its export rows behind pointing at files that are about to be removed. The
/// Audios shelf would then list playable-looking entries whose audio no longer exists. The
/// paths are read before the delete so a refused delete destroys nothing, and the rows are
/// taken here once it has succeeded.
pub async fn purge_orphaned_paths(
    pool: &SqlitePool,
    exports_dir: &Path,
    paths: Vec<String>,
) -> Result<PurgeOutcome, sqlx::Error> {
    purge_paths(pool, exports_dir, paths).await
}

async fn purge_paths(
    pool: &SqlitePool,
    exports_dir: &Path,
    paths: Vec<String>,
) -> Result<PurgeOutcome, sqlx::Error> {
    let mut outcome = PurgeOutcome {
        deleted: 0,
        failed: 0,
    };
    for path in paths {
        if remove_within_guarded(pool, exports_dir, &path).await {
            delete_by_path(pool, &path).await?;
            outcome.deleted += 1;
        } else {
            outcome.failed += 1;
        }
    }
    Ok(outcome)
}

pub async fn purge_all(pool: &SqlitePool, exports_dir: &Path) -> Result<PurgeOutcome, sqlx::Error> {
    let paths = list_exports(pool)
        .await?
        .into_iter()
        .map(|row| row.path)
        .collect();
    purge_paths(pool, exports_dir, paths).await
}

pub async fn purge_for_book(
    pool: &SqlitePool,
    exports_dir: &Path,
    book_id: &str,
) -> Result<PurgeOutcome, sqlx::Error> {
    let paths = paths_for_book(pool, book_id).await?;
    purge_paths(pool, exports_dir, paths).await
}

pub async fn adopt_orphans(pool: &SqlitePool, exports_dir: &Path) -> Result<usize, sqlx::Error> {
    let mut adopted = 0;
    for path in scan_exports(exports_dir) {
        let display = path.to_string_lossy().to_string();
        let existing = sqlx::query!(r#"SELECT id as "id!" FROM export WHERE path = ?"#, display)
            .fetch_optional(pool)
            .await?;
        if existing.is_some() {
            continue;
        }
        let id = new_id();
        let bytes = file_bytes(&path);
        let created_at = file_created_ms(&path);
        let format = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "mp3".to_string());
        let mode = ADOPTED_ORPHAN_MODE;
        // An exact `json_each` element match, not `LIKE '%'||path||'%'` (and not a
        // substring of the raw JSON, which escapes a Windows `\`): the path is user data, and a book
        // title sanitized into `My_Book - Ch 1.mp3` puts a `_` into the pattern,
        // which `LIKE` reads as "any one character". A wildcard pattern can
        // therefore report that this export belongs to a *different* cast's job.
        let matching_job: Option<(String, String)> = sqlx::query_as(
            "SELECT id, book_id FROM render_job WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN \
             json_valid(output_paths_json) THEN output_paths_json ELSE '[]' END) o \
             WHERE o.value = ?) LIMIT 1",
        )
        .bind(&display)
        .fetch_optional(pool)
        .await?;
        let (job_id, book_id) = match matching_job {
            Some((j_id, b_id)) => (Some(j_id), Some(b_id)),
            None => (None, None),
        };
        sqlx::query(
            "INSERT OR IGNORE INTO export
             (id, book_id, job_id, path, bytes, format, mode, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&book_id)
        .bind(&job_id)
        .bind(&display)
        .bind(bytes)
        .bind(&format)
        .bind(mode)
        .bind(created_at)
        .execute(pool)
        .await?;
        adopted += 1;
    }
    Ok(adopted)
}

pub async fn refresh_sizes(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query!(r#"SELECT id as "id!", path as "path!" FROM export WHERE bytes <= 0"#)
        .fetch_all(pool)
        .await?;
    for row in rows {
        let bytes = file_bytes(Path::new(&row.path));
        if bytes > 0 {
            sqlx::query!("UPDATE export SET bytes = ? WHERE id = ?", bytes, row.id)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

fn scan_exports(exports_dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    walk_export_files(exports_dir, 0, &mut |path| {
        if is_adoptable_media(path) {
            found.push(path.to_path_buf());
        }
    });
    found.sort();
    found
}

/// Depth bound for the export walk.
///
/// The layout this app writes is exactly one level deep — `<exports_dir>/
/// <cast_key>/<file>` — so the walk has to reach depth 1 to see the products at
/// all. The rest is headroom for one future level and, more importantly, a
/// guarantee that no tree it is pointed at can be walked without end: the bound
/// holds whatever a directory entry turns out to be, including the ones
/// `DirEntry::file_type` reports as a plain directory but that a later
/// filesystem resolves into one. A budget, not a tuned constant: the number that
/// matters (one) comes from the layout.
const EXPORT_WALK_MAX_DEPTH: usize = 4;

/// Visit every file under the exports root, descending into cast directories.
///
/// One walk serves both callers so the adoption rule and the partial sweep
/// cannot disagree about what is inside the exports directory — they already
/// had to agree, because an adopted file with no row and a swept file with no row
/// are both rows and bytes the app forgot about.
///
/// Dot-prefixed directories are skipped. The exports root already holds
/// working state that is not product: `.staging` carries the LAN sender's
/// alignment sidecars and `.cache` is a fallback cache root, and adopting from
/// either would offer the user files on the Audios shelf that the app will then
/// offer to delete as if they were a cast. Every directory this app creates for
/// exports is a `cast_key`, which is lowercase hex and never starts with a dot,
/// so the rule cannot skip a real cast.
///
/// A symlink is skipped, at every level, rather than walked or visited: the
/// exports directory is the user's, and `<exports>/music -> ~/Music` would
/// otherwise hand the app every `.mp3` the user keeps there — on the Audios
/// shelf, in the storage count, and adopted with a row that deletion cannot
/// remove, because `resolve_within` refuses a target outside the exports root.
/// `DirEntry::file_type` is the check, and it does not follow the link, so the
/// same rule covers the directory and the file case.
fn walk_export_files(exports_dir: &Path, depth: usize, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(exports_dir) else {
        tracing::debug!("export walk skipped: {exports_dir:?} is unreadable");
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_file() {
            visit(&path);
        } else if depth < EXPORT_WALK_MAX_DEPTH && is_cast_directory(&path, &file_type) {
            walk_export_files(&path, depth + 1, visit);
        }
    }
}

/// Whether to descend into this directory on the way to adopting a product.
///
/// Permissive on purpose — any directory that is not dot-prefixed — because the
/// walk exists to find products the app cannot account for, and a file the user
/// parked in `<exports>/My Music/` is one of them. Deletion is the opposite
/// question and uses the opposite rule: see [`is_app_cast_dir_name`]. The two
/// rules differ deliberately, because a file the user parked can be adopted and
/// deleted while the directory they put it in is still theirs.
fn is_cast_directory(path: &Path, file_type: &std::fs::FileType) -> bool {
    file_type.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.is_empty() && !name.starts_with('.'))
}

fn is_adoptable_media(path: &Path) -> bool {
    path.extension()
        .map(|ext| {
            let ext = ext.to_string_lossy().to_lowercase();
            ext == "mp3" || ext == "mp4" || ext == "m4a" || ext == "wav"
        })
        .unwrap_or(false)
}

/// Mode recorded for an adopted file that has no row.
///
/// The row is missing *by definition* — that is what makes the file an orphan —
/// so the voice, mode and book it would have carried are gone, and nothing left
/// on disk states the mode. It used to be inferred from a `- teacher` suffix in
/// the filename, which stopped being possible when the mode moved into the
/// opaque cast directory. One documented default beats a guess that reads like
/// a derivation and would label a teacher's audio as narration in the shelf: the
/// bytes that play are the same either way, and only the caption differs.
pub const ADOPTED_ORPHAN_MODE: &str = "narrator";

fn file_bytes(path: &Path) -> i64 {
    std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or(0)
}

fn file_created_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|delta| delta.as_millis() as i64)
        .unwrap_or_else(now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CAST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn cast_dir(exports: &Path, key: &str) -> PathBuf {
        let dir = exports.join(key);
        std::fs::create_dir_all(&dir).expect("cast dir");
        dir
    }

    /// False when the platform refused to create the link, so a symlink
    /// assertion reports a skip instead of passing because nothing was there.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    /// False when the platform refused to create the link.
    fn link_file(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    #[tokio::test]
    async fn a_file_inside_a_cast_directory_is_adopted_not_invisible() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let orphan = cast_dir(&exports, CAST_A).join("My Book - Ch 1-3.mp3");
        std::fs::write(&orphan, b"audio-bytes").unwrap();

        assert_eq!(
            scan_exports(&exports),
            vec![orphan.clone()],
            "a flat read_dir would leave this file with no row, which is exactly the \
             data-visibility bug: invisible in the shelf and unreachable by any cleanup"
        );

        let adopted = adopt_orphans(&pool, &exports).await.unwrap();
        assert_eq!(adopted, 1);
        let rows = list_exports(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, orphan.to_string_lossy());
        assert_eq!(rows[0].bytes, 11);
        assert_eq!(adopt_orphans(&pool, &exports).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn an_adopted_orphan_is_recorded_as_a_narrator_cast() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        // A pre-layout export whose name still spells out the mode it was made
        // with. The mode is not recoverable from it any more: the row that held
        // it is what is missing, so the name is the only thing left and reading
        // it is exactly the inference that stops being possible.
        std::fs::write(
            exports.join("Some Book - Pages 1-8 [voice-12345678] - teacher [b1234567].mp3"),
            b"audio",
        )
        .unwrap();

        assert_eq!(adopt_orphans(&pool, &exports).await.unwrap(), 1);
        let rows = list_exports(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].mode, ADOPTED_ORPHAN_MODE,
            "an orphan has no row, so its real mode is unknown by definition; the documented \
             default is recorded instead of a guess that reads like a derivation"
        );
        assert_eq!(ADOPTED_ORPHAN_MODE, "narrator");
    }

    #[tokio::test]
    async fn a_cast_file_in_a_working_state_directory_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        for working in [".staging", ".cache"] {
            std::fs::create_dir_all(exports.join(working).join("row-1")).unwrap();
            std::fs::write(
                exports.join(working).join("row-1").join("scratch.mp3"),
                b"not a product",
            )
            .unwrap();
        }
        let product = cast_dir(&exports, CAST_A).join("My Book - Ch 1.mp3");
        std::fs::write(&product, b"a product").unwrap();

        assert_eq!(
            scan_exports(&exports),
            vec![product],
            ".staging and .cache hold the LAN sender's sidecars and a fallback cache root; \
             adopting from them would put working state on the Audios shelf"
        );
        assert_eq!(adopt_orphans(&pool, &exports).await.unwrap(), 1);
        assert_eq!(list_exports(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_symlinked_ancestor_cannot_cycle_the_walk_or_hand_back_the_products_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        let cast = cast_dir(&exports, CAST_A);
        let product = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&product, b"a product").unwrap();
        // A link to an ancestor is the shape that would otherwise be walked
        // without end, and it also reaches the real product a second time under
        // a name that has no row.
        if !link_dir(&exports, &cast.join("loop")) {
            eprintln!("skipping: this platform refused to create a directory symlink");
            return;
        }

        assert_eq!(
            scan_exports(&exports),
            vec![product.clone()],
            "the link is not a cast directory, so the walk neither recurses into it nor \
             returns the same bytes a second time under a path no row will ever hold"
        );
    }

    #[tokio::test]
    async fn a_symlinked_directory_outside_the_exports_root_is_neither_walked_nor_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let library = dir.path().join("My Music");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("01 - my song.mp3"), b"not a cast").unwrap();
        let product = cast_dir(&exports, CAST_A).join("My Book - Ch 1.mp3");
        std::fs::write(&product, b"a product").unwrap();
        if !link_dir(&library, &exports.join("music")) {
            eprintln!("skipping: this platform refused to create a directory symlink");
            return;
        }

        assert_eq!(
            scan_exports(&exports),
            vec![product.clone()],
            "the exports directory belongs to the user: `music -> ~/Music` must not turn \
             everything the user keeps there into an export of this app"
        );
        assert_eq!(
            adopt_orphans(&pool, &exports).await.unwrap(),
            1,
            "adoption grants the app authority to offer a file and then delete it, which \
             `resolve_within` refuses for anything whose target leaves the exports root — so \
             adopting the library would leave rows the user can never remove"
        );
        let rows = list_exports(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, product.to_string_lossy());
    }

    #[tokio::test]
    async fn a_symlink_inside_a_cast_directory_is_not_descended() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        let cast = cast_dir(&exports, CAST_A);
        let product = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&product, b"a product").unwrap();
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("a song.mp3"), b"not a cast").unwrap();
        if !link_dir(&outside, &cast.join("extras")) {
            eprintln!("skipping: this platform refused to create a directory symlink");
            return;
        }

        assert_eq!(
            scan_exports(&exports),
            vec![product],
            "the level below a cast directory is checked by the same rule as the level above it"
        );
        assert_eq!(adopt_orphans(&pool, &exports).await.unwrap(), 1);
        assert_eq!(list_exports(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_symlinked_file_at_the_exports_root_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let library = dir.path().join("My Music");
        std::fs::create_dir_all(&library).unwrap();
        let track = library.join("01 - my song.mp3");
        std::fs::write(&track, b"not a cast").unwrap();
        if !link_file(&track, &exports.join("My Book - Ch 1.mp3")) {
            eprintln!("skipping: this platform refused to create a file symlink");
            return;
        }

        assert_eq!(
            scan_exports(&exports),
            Vec::<PathBuf>::new(),
            "a row for this would carry the *target's* bytes and mtime, would be refused by \
             `resolve_within` on every delete, and would be served from a name the user never \
             wrote: nothing here is ours to adopt"
        );
        assert_eq!(adopt_orphans(&pool, &exports).await.unwrap(), 0);
        assert!(
            list_exports(&pool).await.unwrap().is_empty(),
            "an adopted row the user cannot delete is worse than an unadopted file, which at \
             worst stays the user's own file"
        );
    }

    #[test]
    fn the_walk_stops_at_its_depth_bound_and_still_reaches_the_product_one_level_down() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let product = cast_dir(&exports, CAST_A).join("My Book - Ch 1.mp3");
        std::fs::write(&product, b"a product").unwrap();
        // No symlink: the bound is what stops this, so the bound keeps its own
        // coverage where creating a link is not permitted.
        let mut deep = exports.clone();
        for level in 0..(EXPORT_WALK_MAX_DEPTH + 2) {
            deep = deep.join(format!("nested-{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("too deep.mp3"), b"not reachable").unwrap();

        let found = scan_exports(&exports);
        let root_depth = exports.components().count();
        for path in &found {
            let depth = path.components().count() - root_depth;
            assert!(
                depth <= EXPORT_WALK_MAX_DEPTH + 1,
                "{path:?} sits {depth} components below the exports root, past the bound: the \
                 bound is the guarantee that no tree it is pointed at can be walked without end"
            );
        }
        assert_eq!(
            found,
            vec![product],
            "the layout is one level deep, so the bound's headroom costs nothing and the \
             product is still reached"
        );
    }

    #[test]
    fn the_partial_sweep_reaches_a_partial_inside_a_cast_directory() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path();
        let cast = cast_dir(exports, CAST_A);
        let abandoned = partial_sibling(&cast.join("My Book - Ch 1.mp3"));
        std::fs::write(&abandoned, b"half an encode").unwrap();
        let published = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&published, b"the good export").unwrap();

        assert_eq!(
            sweep_partials(exports),
            1,
            "an interrupted encode leaves its .partial inside the cast directory; a flat \
             sweep would strand those bytes for the life of the install"
        );
        assert!(!abandoned.exists());
        assert!(published.exists());
        assert_eq!(sweep_partials(exports), 0, "the sweep is idempotent");
    }

    #[test]
    fn deleting_a_nested_export_removes_the_file_and_the_emptied_cast_directory() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let only = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&only, b"the only export of this cast").unwrap();

        assert!(remove_within(&exports, &only.to_string_lossy()));
        assert!(!only.exists());
        assert!(
            !cast.exists(),
            "an emptied cast directory is invisible to every row and every sweep, so leaving \
             it behind leaves a hollow name nothing in the app can account for"
        );
        assert!(exports.exists(), "the exports root itself is never removed");
    }

    #[test]
    fn deleting_an_export_with_a_stale_partial_leaves_no_hollow_cast_directory() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let only = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&only, b"the good export").unwrap();
        // What a power loss mid-encode leaves behind: the abandoned encode, not
        // the export. On its own this is enough to keep the directory occupied
        // so that nothing can prune it.
        let stale = partial_sibling(&only);
        std::fs::write(&stale, b"half an encode").unwrap();

        assert!(remove_within(&exports, &only.to_string_lossy()));
        assert!(!only.exists());
        assert!(
            !cast.exists(),
            "the sibling has to go with the export: a directory this cleanup and the startup \
             sweep both leave occupied is a directory neither of them can remove"
        );
        assert_eq!(
            sweep_partials(&exports),
            0,
            "and nothing is left for the next launch's sweep to find either"
        );
        assert!(exports.exists(), "the exports root itself is never removed");
    }

    #[test]
    fn removing_an_export_that_is_already_gone_still_prunes_its_hollow_cast_directory() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        // No file at all: the same hollow state arrives when the user removes the
        // `.mp3` in their own file manager, which is the branch a prune limited
        // to a successful removal would miss.
        let gone = cast.join("My Book - Ch 1.mp3");

        assert!(
            remove_within(&exports, &gone.to_string_lossy()),
            "an export that is already absent is still a successful delete"
        );
        assert!(
            !cast.exists(),
            "the directory is empty and still unaccounted for by any row or sweep"
        );
    }

    #[test]
    fn the_partial_sweep_prunes_the_cast_directory_an_interrupted_encode_emptied() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let abandoned = partial_sibling(&cast.join("My Book - Ch 1.mp3"));
        std::fs::write(&abandoned, b"half an encode").unwrap();

        assert_eq!(sweep_partials(&exports), 1);
        assert!(!abandoned.exists());
        assert!(
            !cast.exists(),
            "a crash before the rename leaves a cast directory holding nothing but the \
             abandoned encode; sweeping the file without the directory trades a visible byte \
             leak for a hollow name nothing in the app can account for"
        );
        assert!(exports.exists(), "the exports root itself is never removed");
    }

    #[test]
    fn the_partial_sweep_leaves_a_cast_directory_that_still_holds_a_product() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let published = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&published, b"the good export").unwrap();
        std::fs::write(partial_sibling(&published), b"half an encode").unwrap();

        assert_eq!(sweep_partials(&exports), 1);
        assert!(
            cast.exists() && published.exists(),
            "pruning is for the directory this sweep emptied, not every directory it passed"
        );
    }

    #[test]
    fn clearing_a_sibling_partial_never_prunes_the_directory_the_renderer_about_to_encode_into() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let output = cast.join("My Book - Ch 1.mp3");
        let stale = partial_sibling(&output);
        std::fs::write(&stale, b"half an encode").unwrap();

        assert!(remove_partial_within(&exports, &output));
        assert!(!stale.exists());
        assert!(
            cast.exists(),
            "the renderer created this directory to encode into it and clears the stale sibling \
             in between; pruning here deletes the directory the encode is about to write, and \
             the startup sweep is what collects a directory this leaves empty"
        );
    }

    #[test]
    fn a_cast_directory_another_export_still_uses_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let first = cast.join("My Book - Ch 1.mp3");
        let second = cast.join("My Book - Ch 2.mp3");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();

        assert!(remove_within(&exports, &first.to_string_lossy()));
        assert!(!first.exists());
        assert!(
            cast.exists() && second.exists(),
            "a directory that still holds a product is not ours to remove"
        );
    }

    #[test]
    fn deleting_a_nested_export_still_refuses_a_path_outside_the_exports_directory() {
        let root = tempfile::tempdir().unwrap();
        let exports = root.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let escape = root.path().join(CAST_A).join("My Book - Ch 1.mp3");
        std::fs::create_dir_all(escape.parent().unwrap()).unwrap();
        std::fs::write(&escape, b"not an export").unwrap();
        let sibling = root.path().join(CAST_A).join("My Book - Ch 2.mp3");
        std::fs::write(&sibling, b"also not an export").unwrap();

        assert!(matches!(
            resolve_within(&exports, &escape.to_string_lossy()),
            Err(reason) if reason.contains("outside the exports directory")
        ));
        assert!(
            !remove_within(&exports, &escape.to_string_lossy()),
            "the nested layout must not weaken the containment check"
        );
        assert!(escape.exists() && sibling.exists());
    }

    #[tokio::test]
    async fn a_nested_export_is_linked_to_its_own_job_and_to_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let mine = cast_dir(&exports, CAST_A).join("My Book - Ch 1.mp3");
        let theirs = cast_dir(&exports, CAST_B).join("My Book - Ch 2.mp3");
        std::fs::write(&mine, b"mine").unwrap();
        std::fs::write(&theirs, b"theirs").unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1);",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (job_id, output) in [("j-mine", &mine), ("j-theirs", &theirs)] {
            sqlx::query(
                "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                          output_format, state, output_paths_json, created_at, \
                                          updated_at) \
                 VALUES (?, 'b1', '{}', 'narrator', 'vp1', 'mp3', 'DONE', ?, 1, 1)",
            )
            .bind(job_id)
            .bind(serde_json::to_string(&[output.to_string_lossy().as_ref()]).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e1', NULL, NULL, ?, 11, 'mp3', 'narrator', 1)",
        )
        .bind(mine.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            backfill_orphan_links(&pool).await.unwrap(),
            1,
            "the export's own nested path is in j-mine's output_paths_json"
        );
        let row = get_export(&pool, "e1").await.unwrap().unwrap();
        assert_eq!(row.job_id.as_deref(), Some("j-mine"), "{row:?}");
        assert_eq!(row.book_id.as_deref(), Some("b1"));
    }

    #[tokio::test]
    async fn a_wildcard_in_a_path_cannot_steal_another_casts_job() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, updated_at) \
             VALUES ('j-us', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'DONE', \
                     '[\"/exports/MyXBook - Ch 1.mp3\"]', 1, 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, updated_at) \
             VALUES ('j-pct', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'DONE', \
                     '[\"/exports/Shrink 30X Off - Ch 1.mp3\"]', 1, 1); \
             INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e-us', NULL, NULL, '/exports/My_Book - Ch 1.mp3', 11, 'mp3', 'narrator', 1); \
             INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e-pct', NULL, NULL, '/exports/Shrink 30% Off - Ch 1.mp3', 11, 'mp3', 'narrator', 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        // `%` is kept verbatim by `sanitize_filename` and `_` is what it puts in
        // place of an illegal character, so both land in a real export name. Each
        // export above differs from a *different* job's output path by exactly
        // the characters LIKE reads as wildcards, and one job per export keeps a
        // `LIMIT 1` from hiding the difference.
        assert_eq!(
            backfill_orphan_links(&pool).await.unwrap(),
            0,
            "neither path is in the job a wildcard pattern would match, so no row may be \
             attributed: 'My_Book' must not read as 'MyXBook' and '30%' as '30X'"
        );
        for id in ["e-us", "e-pct"] {
            let row = get_export(&pool, id).await.unwrap().unwrap();
            assert_eq!(row.job_id, None, "{} took another cast's job", row.path);
            assert_eq!(row.book_id, None, "{} took another cast's book", row.path);
        }
    }

    #[tokio::test]
    async fn list_exports_is_pure_select_and_does_not_mutate_database() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e1', NULL, NULL, '/tmp/export1.mp3', 1024, 'mp3', 'narrator', 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let exports = list_exports(&pool).await.unwrap();
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].id, "e1");
        assert_eq!(exports[0].book_id, None);
        assert_eq!(exports[0].job_id, None);

        let row: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT book_id, job_id FROM export WHERE id = 'e1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, None);
        assert_eq!(row.1, None);
    }

    #[tokio::test]
    async fn backfill_orphan_links_reconciles_unlinked_exports() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'DONE', '[\"/tmp/export1.mp3\"]', 1, 1); \
             INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e1', NULL, NULL, '/tmp/export1.mp3', 1024, 'mp3', 'narrator', 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let affected = backfill_orphan_links(&pool).await.unwrap();
        assert_eq!(affected, 1);

        let export = get_export(&pool, "e1").await.unwrap().unwrap();
        assert_eq!(export.book_id.as_deref(), Some("b1"));
        assert_eq!(export.job_id.as_deref(), Some("j1"));
    }

    #[tokio::test]
    async fn a_windows_path_links_through_the_json_the_renderer_writes() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let path = r"C:\Users\u\AppData\Roaming\txtCaster\exports\ab12\My Book - Ch 1.mp3";
        let written = serde_json::to_string(&serde_json::json!([path])).unwrap();
        assert!(
            written.contains(r"\\"),
            "JSON escapes the separator: {written}"
        );
        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, updated_at) \
             VALUES ('j-bad', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'FAILED', 'not json', 1, 1);",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
             output_format, state, output_paths_json, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'DONE', ?, 1, 1)",
        )
        .bind(&written)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e1', NULL, NULL, ?, 11, 'mp3', 'narrator', 1)",
        )
        .bind(path)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            backfill_orphan_links(&pool).await.unwrap(),
            1,
            "a row whose output list is not JSON must not stop the others from linking"
        );
        let export = get_export(&pool, "e1").await.unwrap().unwrap();
        assert_eq!(export.job_id.as_deref(), Some("j1"));
        assert_eq!(export.book_id.as_deref(), Some("b1"));
    }

    #[test]
    fn the_partial_a_sibling_sweep_removes_is_the_one_the_renderer_writes() {
        let export = Path::new("/exports/Book - Pages 1-8 [v] - narrator [b1234567].mp3");
        let sibling = partial_sibling(export);
        assert_eq!(
            sibling,
            Path::new("/exports/Book - Pages 1-8 [v] - narrator [b1234567].mp3.partial"),
            "the renderer appends .partial to the finished name, so the sibling sweep must compute \
             that same path rather than applying the export suffix a second time"
        );
        assert!(
            is_partial_export(&sibling),
            "the startup sweep must recognise the path the sibling sweep removes"
        );
    }

    #[test]
    fn the_partial_sweep_takes_only_the_shape_the_renderer_writes() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path();
        let export = exports.join("Book - Pages 1-8 [v] - narrator [b1234567].mp3");
        let published = export;
        std::fs::write(&published, b"the good export").unwrap();
        let abandoned = partial_sibling(&published);
        std::fs::write(&abandoned, b"half an encode").unwrap();
        let lookalike = exports.join("notes.mp3.partial.txt");
        std::fs::write(&lookalike, b"not ours").unwrap();
        let stray = exports.join("scratch.partial");
        std::fs::write(&stray, b"not ours either").unwrap();

        assert!(is_partial_export(&abandoned));
        assert!(!is_partial_export(&published));
        assert!(!is_partial_export(&lookalike));

        assert_eq!(sweep_partials(exports), 1, "exactly the abandoned encode");
        assert!(!abandoned.exists());
        assert!(
            published.exists(),
            "the published export is never a sweep target"
        );
        assert!(
            lookalike.exists(),
            "only the exact suffix is ours to remove"
        );
        assert!(stray.exists());
        assert_eq!(
            std::fs::read(&published).unwrap(),
            b"the good export",
            "the sweep must never touch a published export's bytes"
        );
        assert_eq!(sweep_partials(exports), 0, "the sweep is idempotent");
    }

    #[test]
    fn the_partial_sweep_is_a_no_op_on_a_missing_exports_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        assert_eq!(sweep_partials(&missing), 0);
    }

    #[test]
    fn removing_a_sibling_partial_refuses_a_path_outside_the_exports_directory() {
        let root = tempfile::tempdir().unwrap();
        let exports = root.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let outside = root.path().join("elsewhere.mp3");
        let outside_partial = root.path().join(format!("elsewhere.mp3{PARTIAL_SUFFIX}"));
        std::fs::write(&outside_partial, b"not an export").unwrap();

        assert!(
            !remove_partial_within(&exports, &outside),
            "a sibling of a path outside the exports directory must be refused, not deleted"
        );
        assert!(outside_partial.exists());
    }

    #[test]
    fn removing_a_sibling_partial_tolerates_its_absence() {
        let root = tempfile::tempdir().unwrap();
        let exports = root.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let export = exports.join("Book - Pages 1-8 [v] - narrator [b1234567].mp3");
        std::fs::write(&export, b"the good export").unwrap();

        assert!(remove_partial_within(&exports, &export));
        assert!(
            export.exists(),
            "the export itself is never a removal target"
        );
        assert!(!exports
            .join(format!("{}{PARTIAL_SUFFIX}", export.to_string_lossy()))
            .exists());
    }

    #[test]
    fn only_a_cast_key_directory_name_is_one_this_app_created() {
        assert!(
            is_app_cast_dir_name(CAST_A) && is_app_cast_dir_name(CAST_B),
            "cast_key is hex::encode(sha256(..)), so a real one is 64 lowercase hex characters"
        );
        assert_eq!(CAST_A.len(), CAST_KEY_NAME_LEN);
        let too_short = &CAST_A[..CAST_KEY_NAME_LEN - 1];
        let too_long = format!("{CAST_A}0");
        for ours in [
            "My Music", // the exports root is the user's
            "aaaaaaaa", // right alphabet, wrong length
            too_short,
            &too_long,
            "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef", // uppercase
            "0123456789abcdefg0123456789abcdef0123456789abcdef0123456789abcde", // not hex
            ".staging", // working state, not a cast
            "",
        ] {
            assert!(
                !is_app_cast_dir_name(ours),
                "{ours:?} is not a name this app created and must never be pruned"
            );
        }
    }

    #[tokio::test]
    async fn a_directory_the_user_created_survives_deleting_the_file_adopted_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let mine = cast_dir(&exports, CAST_A).join("My Book - Ch 1.mp3");
        std::fs::write(&mine, b"a product").unwrap();
        let library = exports.join("My Music");
        std::fs::create_dir_all(&library).unwrap();
        let track = library.join("01 - my song.mp3");
        std::fs::write(&track, b"not a cast").unwrap();

        assert_eq!(
            adopt_orphans(&pool, &exports).await.unwrap(),
            2,
            "adoption is deliberately permissive: a file the user parked in the exports \
             directory is a product they should see"
        );
        let rows = list_exports(&pool).await.unwrap();
        let adopted = rows
            .iter()
            .find(|row| row.path == track.to_string_lossy())
            .expect("the user's track is on the shelf");
        assert_eq!(adopted.book_id, None);
        assert_eq!(adopted.mode, ADOPTED_ORPHAN_MODE);

        assert!(remove_within(&exports, &adopted.path));
        assert!(!track.exists(), "the file the user asked to delete is gone");
        assert!(
            library.exists(),
            "the directory is the user's: it is a direct child of the exports root like a \
             cast directory is, and depth 1 is exactly where the user keeps a tree — so \
             only the name, which records who made it, can tell the two apart"
        );
        assert!(mine.exists(), "the app's own product is untouched");
    }

    #[test]
    fn the_partial_sweep_does_not_prune_a_directory_the_user_created() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let library = exports.join("My Music");
        std::fs::create_dir_all(&library).unwrap();
        // Nothing else in it: the shape the sweep's prune is reached with.
        let abandoned = library.join("anything.mp3.partial");
        std::fs::write(&abandoned, b"the user's own download in progress").unwrap();

        assert_eq!(sweep_partials(&exports), 1, "the file is still swept");
        assert!(!abandoned.exists());
        assert!(
            library.exists(),
            "the sweep prunes the directory it emptied, and this one is not the app's to prune"
        );
    }

    #[tokio::test]
    async fn a_live_job_keeps_the_partial_its_own_encode_is_writing() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let output = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&output, b"the previous export").unwrap();
        let partial = partial_sibling(&output);
        std::fs::write(&partial, b"half an encode").unwrap();
        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, \
                                     updated_at) \
             VALUES ('j1', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'RENDERING', '[]', 1, 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        // A real thread holding the file open, which is what ffmpeg is. The
        // handle keeps the bytes reachable on Linux long after the name is gone,
        // so the damage does not show up here — it shows up one step later, at
        // the renderer's rename.
        let (opened, is_open) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (renamed, renamed_rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let encoding = {
            let partial = partial.clone();
            let output = output.clone();
            std::thread::spawn(move || -> std::io::Result<()> {
                let mut handle = std::fs::OpenOptions::new().write(true).open(&partial)?;
                opened.send(()).expect("the test is waiting for the handle");
                released.recv().expect("the delete ran");
                handle.write_all(b"the encode ffmpeg just finished")?;
                handle.sync_all()?;
                drop(handle);
                std::fs::rename(&partial, &output)?;
                renamed.send(Ok(())).ok();
                Ok(())
            })
        };
        // The handle is open before the delete starts, so this exercises the
        // window rather than a race the test has to win.
        is_open.recv().expect("the encoder opened the partial");

        assert!(live_encode_exists(&pool).await.unwrap());
        assert!(
            remove_within_guarded(&pool, &exports, &output.to_string_lossy()).await,
            "the export the user deleted is still deleted"
        );
        assert!(!output.exists(), "the published export is gone");
        assert!(
            partial.exists(),
            "the sibling is the path the running encode is writing, so unlinking it would \
             leave ffmpeg writing to an orphan and fail its rename with ENOENT — the \
             defect this guard exists to close, and one Windows never had, because an open \
             handle refuses the delete there with ERROR_SHARING_VIOLATION"
        );
        release.send(()).expect("the encoder is waiting");

        renamed_rx
            .recv()
            .expect("the encoder reports its rename")
            .expect("the renderer's rename still finds the file it wrote");
        encoding
            .join()
            .expect("the encoder thread")
            .expect("the encoder reported no error");
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"the encode ffmpeg just finished",
            "the job's own publish lands, which is the whole point"
        );
        assert!(
            remove_within(&exports, &output.to_string_lossy()),
            "and the export that publish created is an ordinary delete"
        );
        assert!(
            !cast.exists(),
            "a real cast directory is still pruned the moment it is emptied — the name rule \
             narrows the prune to the app's own directories, it does not switch it off"
        );
    }

    #[tokio::test]
    async fn a_live_job_keeps_an_emptied_cast_directory_until_the_next_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let output = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&output, b"the export being re-rendered").unwrap();
        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, \
                                     updated_at) \
             VALUES ('j1', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'RENDERING', '[]', 1, 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(remove_within_guarded(&pool, &exports, &output.to_string_lossy()).await);
        assert!(!output.exists(), "the export the user deleted is gone");
        assert!(
            cast.is_dir(),
            "a re-render of this cast created the directory and has not opened its .partial \
             yet; pruning it now fails that encode with ENOENT"
        );

        sqlx::query("DELETE FROM render_job")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(sweep_partials(&exports), 0);
        assert!(
            !cast.exists(),
            "the launch sweep collects a cast directory a guarded delete had to leave behind"
        );
    }

    #[tokio::test]
    async fn an_abandoned_partial_is_still_cleared_and_its_cast_directory_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let output = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&output, b"the good export").unwrap();
        let stale = partial_sibling(&output);
        std::fs::write(&stale, b"half an encode").unwrap();
        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, output_paths_json, created_at, \
                                     updated_at) \
             VALUES ('j1', 'b1', '{}', 'narrator', 'vp1', 'mp3', 'DONE', '[]', 1, 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            !live_encode_exists(&pool).await.unwrap(),
            "a finished job owns nothing; the guard must not become a blanket refusal"
        );
        assert!(remove_within_guarded(&pool, &exports, &output.to_string_lossy()).await);
        assert!(!output.exists());
        assert!(
            !stale.exists(),
            "the abandoned encode is what the earlier fix existed to clear, and skipping it \
             would leave the cast directory occupied and unaccounted for"
        );
        assert!(
            !cast.exists(),
            "and the directory is pruned immediately, not left for a later launch"
        );
    }

    #[tokio::test]
    async fn the_sibling_survives_even_when_the_job_table_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let cast = cast_dir(&exports, CAST_A);
        let output = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&output, b"the good export").unwrap();
        let stale = partial_sibling(&output);
        std::fs::write(&stale, b"half an encode").unwrap();
        // The guard's own query cannot run.
        sqlx::raw_sql("DROP TABLE render_job")
            .execute(&pool)
            .await
            .unwrap();
        assert!(live_encode_exists(&pool).await.is_err());

        assert!(remove_within_guarded(&pool, &exports, &output.to_string_lossy()).await);
        assert!(!output.exists());
        assert!(
            stale.exists(),
            "failing closed keeps the sibling: an unreadable job table is not evidence that \
             no encode is running, and the next launch's sweep collects it"
        );
    }

    #[test]
    fn a_new_export_path_is_refused_before_the_file_exists_when_the_name_escapes() {
        let root = tempfile::tempdir().unwrap();
        let exports = root.path().join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        let outside = root.path().join("escaped.mp3");

        assert!(matches!(
            resolve_new_file_within(&exports, "../escaped.mp3"),
            Err(reason) if reason.contains("single path component")
        ));
        assert!(
            !outside.exists(),
            "nothing is written: `Path::join` honours `..`, so an unchecked name lands the \
             write outside the exports root"
        );
        assert!(resolve_new_file_within(&exports, "../../escaped.mp3").is_err());
        assert!(resolve_new_file_within(&exports, "sub/dir/name.mp3").is_err());
        assert!(resolve_new_file_within(&exports, "..").is_err());

        let inside = resolve_new_file_within(&exports, "book-g5.mp3").unwrap();
        assert_eq!(
            inside.parent().unwrap(),
            exports.canonicalize().unwrap(),
            "one ordinary component joined onto the canonical root cannot leave it"
        );
    }
}
