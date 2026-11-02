use std::path::{Path, PathBuf};

const REMOVAL_MARKER_SUFFIX: &str = ".deleted";

fn marker_for(books_root: &Path, book_id: &str) -> PathBuf {
    books_root.join(format!("{book_id}{REMOVAL_MARKER_SUFFIX}"))
}

fn remove_dir_and_marker(book_dir: &Path, marker: &Path) -> std::io::Result<()> {
    if book_dir.exists() {
        std::fs::remove_dir_all(book_dir)?;
    }
    match std::fs::remove_file(marker) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// The book row is already gone, so a file that will not go yet (Windows keeps open files) is
/// not a failure of the delete: the marker makes the next launch finish it.
pub fn remove_deleted_book_files(books_root: &Path, book_id: &str) {
    let marker = marker_for(books_root, book_id);
    if let Err(err) = std::fs::write(&marker, b"") {
        tracing::warn!("could not mark {} for removal: {err}", marker.display());
    }
    if let Err(err) = remove_dir_and_marker(&books_root.join(book_id), &marker) {
        tracing::warn!("book {book_id} files stay until the next launch: {err}");
    }
}

pub fn finish_interrupted_removals(books_root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(books_root) else {
        return 0;
    };
    let mut finished = 0;
    for marker in entries.flatten().map(|entry| entry.path()) {
        let Some(book_id) = marker
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(REMOVAL_MARKER_SUFFIX))
        else {
            continue;
        };
        match remove_dir_and_marker(&books_root.join(book_id), &marker) {
            Ok(()) => finished += 1,
            Err(err) => tracing::warn!("book {book_id} files stay until the next launch: {err}"),
        }
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deleted_books_files_and_marker_are_gone() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("b1/pages")).expect("book");
        std::fs::create_dir_all(root.path().join("b2")).expect("other book");
        remove_deleted_book_files(root.path(), "b1");
        assert!(!root.path().join("b1").exists());
        assert!(!marker_for(root.path(), "b1").exists());
        assert!(root.path().join("b2").exists());
    }

    #[test]
    fn a_book_without_files_still_deletes_cleanly() {
        let root = tempfile::tempdir().expect("root");
        remove_deleted_book_files(root.path(), "b1");
        assert!(!marker_for(root.path(), "b1").exists());
    }

    #[cfg(unix)]
    #[test]
    fn files_that_will_not_go_now_are_removed_on_the_next_launch() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("root");
        let stuck = root.path().join("b1/pages");
        std::fs::create_dir_all(&stuck).expect("book");
        std::fs::write(stuck.join("1.png"), b"page").expect("page");
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o500)).expect("lock");

        remove_deleted_book_files(root.path(), "b1");
        assert!(stuck.join("1.png").exists());
        assert!(marker_for(root.path(), "b1").exists());

        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o700)).expect("unlock");
        assert_eq!(finish_interrupted_removals(root.path()), 1);
        assert!(!root.path().join("b1").exists());
        assert!(!marker_for(root.path(), "b1").exists());
    }

    #[test]
    fn a_launch_leaves_books_without_a_marker_alone() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("b1")).expect("book");
        assert_eq!(finish_interrupted_removals(root.path()), 0);
        assert!(root.path().join("b1").exists());
    }
}
