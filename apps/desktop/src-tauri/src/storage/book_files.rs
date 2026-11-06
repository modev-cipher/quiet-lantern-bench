use std::path::Path;

use sqlx::SqlitePool;

use super::settings;

/// Recorded in the same transaction that deletes the book row, so it survives a books folder
/// that cannot be written.
pub const PENDING_REMOVAL_PREFIX: &str = "removal.book.";
const LEGACY_MARKER_SUFFIX: &str = ".deleted";

pub fn pending_removal_key(book_id: &str) -> String {
    format!("{PENDING_REMOVAL_PREFIX}{book_id}")
}

fn is_book_folder_name(book_id: &str) -> bool {
    !book_id.is_empty() && book_id != "." && book_id != ".." && !book_id.contains(['/', '\\'])
}

fn remove_book_folder(books_root: &Path, book_id: &str) -> std::io::Result<()> {
    match std::fs::remove_dir_all(books_root.join(book_id)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Files that will not go yet (Windows keeps open files, a read-only folder) keep their record,
/// and the next launch tries again.
pub async fn remove_deleted_book_files(
    pool: &SqlitePool,
    books_root: &Path,
    book_id: &str,
) -> bool {
    let key = pending_removal_key(book_id);
    if !is_book_folder_name(book_id) {
        tracing::warn!("dropping a removal record that names no book folder: {book_id:?}");
        let _ = settings::delete_setting(pool, &key).await;
        return false;
    }
    let (root, id) = (books_root.to_path_buf(), book_id.to_string());
    let removed = tokio::task::spawn_blocking(move || remove_book_folder(&root, &id))
        .await
        .unwrap_or_else(|err| Err(std::io::Error::other(err)));
    if let Err(err) = removed {
        tracing::warn!("book {book_id} files stay until the next launch: {err}");
        return false;
    }
    if let Err(err) = settings::delete_setting(pool, &key).await {
        tracing::warn!("book {book_id} files are gone but its removal record stays: {err}");
    }
    true
}

pub async fn finish_pending_removals(pool: &SqlitePool, books_root: &Path) -> usize {
    let mut book_ids: Vec<String> =
        match settings::list_settings_with_prefix(pool, PENDING_REMOVAL_PREFIX).await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|(key, _)| key.strip_prefix(PENDING_REMOVAL_PREFIX).map(str::to_owned))
                .collect(),
            Err(err) => {
                tracing::warn!("could not read pending book removals: {err}");
                Vec::new()
            }
        };
    for marker in std::fs::read_dir(books_root)
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = marker.file_name();
        let Some(book_id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(LEGACY_MARKER_SUFFIX))
        else {
            continue;
        };
        if is_book_folder_name(book_id) && remove_book_folder(books_root, book_id).is_ok() {
            let _ = std::fs::remove_file(marker.path());
            if !book_ids.iter().any(|id| id == book_id) {
                book_ids.push(book_id.to_owned());
            }
        }
    }
    let mut finished = 0;
    for book_id in book_ids {
        if remove_deleted_book_files(pool, books_root, &book_id).await {
            finished += 1;
        }
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("dir");
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .expect("pool");
        (dir, pool)
    }

    async fn add_book(pool: &SqlitePool, id: &str, extracting_since: Option<i64>) {
        sqlx::query(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at, \
             extracting_since) VALUES (?, 'Book', '/b.pdf', 'pdf', 'h', 0, ?)",
        )
        .bind(id)
        .bind(extracting_since)
        .execute(pool)
        .await
        .expect("book");
    }

    async fn pending(pool: &SqlitePool) -> Vec<String> {
        settings::list_settings_with_prefix(pool, PENDING_REMOVAL_PREFIX)
            .await
            .expect("pending")
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    #[tokio::test]
    async fn deleting_a_book_records_its_files_for_removal_with_the_row() {
        let (_dir, pool) = pool().await;
        add_book(&pool, "b1", None).await;
        let outcome = crate::storage::book::delete_book_unless_busy(&pool, "b1", 1_000_000)
            .await
            .expect("delete");
        assert_eq!(outcome, Ok(()));
        assert_eq!(pending(&pool).await, vec![pending_removal_key("b1")]);
    }

    #[tokio::test]
    async fn a_refused_delete_records_nothing() {
        let (_dir, pool) = pool().await;
        let now = 1_000_000_000;
        add_book(&pool, "b1", Some(now)).await;
        let outcome = crate::storage::book::delete_book_unless_busy(&pool, "b1", now)
            .await
            .expect("delete");
        assert_eq!(
            outcome,
            Err(crate::storage::book::DeleteRefusal::ExtractionActive)
        );
        assert!(pending(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_deleted_books_files_and_record_are_gone() {
        let (_dir, pool) = pool().await;
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("b1/pages")).expect("book");
        std::fs::create_dir_all(root.path().join("b2")).expect("other book");
        settings::set_setting(&pool, &pending_removal_key("b1"), "")
            .await
            .expect("record");

        assert!(remove_deleted_book_files(&pool, root.path(), "b1").await);
        assert!(!root.path().join("b1").exists());
        assert!(root.path().join("b2").exists());
        assert!(pending(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_book_without_files_still_clears_its_record() {
        let (_dir, pool) = pool().await;
        let root = tempfile::tempdir().expect("root");
        settings::set_setting(&pool, &pending_removal_key("b1"), "")
            .await
            .expect("record");
        assert!(remove_deleted_book_files(&pool, root.path(), "b1").await);
        assert!(pending(&pool).await.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn files_in_a_read_only_books_folder_are_removed_on_a_later_launch() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, pool) = pool().await;
        let root = tempfile::tempdir().expect("root");
        let books = root.path().join("books");
        std::fs::create_dir_all(books.join("b1/pages")).expect("book");
        std::fs::write(books.join("b1/pages/1.png"), b"page").expect("page");
        add_book(&pool, "b1", None).await;
        crate::storage::book::delete_book_unless_busy(&pool, "b1", 1_000_000)
            .await
            .expect("delete")
            .expect("deleted");
        std::fs::set_permissions(&books, std::fs::Permissions::from_mode(0o500)).expect("lock");

        assert!(!remove_deleted_book_files(&pool, &books, "b1").await);
        assert!(books.join("b1").exists());
        assert_eq!(finish_pending_removals(&pool, &books).await, 0);
        assert_eq!(pending(&pool).await, vec![pending_removal_key("b1")]);

        std::fs::set_permissions(&books, std::fs::Permissions::from_mode(0o700)).expect("unlock");
        assert_eq!(finish_pending_removals(&pool, &books).await, 1);
        assert!(!books.join("b1").exists());
        assert!(pending(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_launch_leaves_books_without_a_record_alone() {
        let (_dir, pool) = pool().await;
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("b1")).expect("book");
        assert_eq!(finish_pending_removals(&pool, root.path()).await, 0);
        assert!(root.path().join("b1").exists());
    }

    #[tokio::test]
    async fn a_marker_left_by_an_earlier_build_is_still_finished() {
        let (_dir, pool) = pool().await;
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("b1")).expect("book");
        std::fs::write(root.path().join(format!("b1{LEGACY_MARKER_SUFFIX}")), b"").expect("marker");
        assert_eq!(finish_pending_removals(&pool, root.path()).await, 1);
        assert!(!root.path().join("b1").exists());
        assert!(!root
            .path()
            .join(format!("b1{LEGACY_MARKER_SUFFIX}"))
            .exists());
    }

    #[tokio::test]
    async fn a_record_naming_a_path_outside_the_books_folder_is_dropped() {
        let (_dir, pool) = pool().await;
        let root = tempfile::tempdir().expect("root");
        let books = root.path().join("books");
        std::fs::create_dir_all(&books).expect("books");
        std::fs::create_dir_all(root.path().join("keep")).expect("outside");
        settings::set_setting(&pool, &pending_removal_key("../keep"), "")
            .await
            .expect("record");
        assert_eq!(finish_pending_removals(&pool, &books).await, 0);
        assert!(root.path().join("keep").exists());
        assert!(pending(&pool).await.is_empty());
    }
}
