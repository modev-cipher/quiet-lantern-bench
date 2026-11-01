use std::path::{Path, PathBuf};

pub const CACHE_SENTINEL: &str = "<cache>";

pub fn cache_path(cache_dir: &Path, hash: &str) -> PathBuf {
    let shard = hash.get(..2).unwrap_or(hash);
    cache_dir.join(shard).join(hash)
}

pub fn to_sentinel(cache_dir: &Path, path: &Path) -> String {
    let normalised = normalise(path);
    let prefix = normalise(cache_dir);
    let prefix = prefix.trim_end_matches('/');
    match normalised.strip_prefix(prefix) {
        Some(rest) => format!("{CACHE_SENTINEL}{rest}"),
        None => normalised,
    }
}

fn normalise(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_is_sharded_by_its_first_two_characters() {
        let path = cache_path(Path::new("/data/cache"), "abcdef");
        assert_eq!(path, Path::new("/data/cache/ab/abcdef"));
    }

    #[test]
    fn a_hash_shorter_than_a_shard_is_its_own_shard_instead_of_a_panic() {
        let path = cache_path(Path::new("/data/cache"), "a");
        assert_eq!(
            path,
            Path::new("/data/cache/a/a"),
            "slicing [..2] on a short hash panics, which turns a corrupt row into a crash"
        );
    }

    #[test]
    fn an_empty_hash_composes_without_panicking() {
        let path = cache_path(Path::new("/data/cache"), "");
        assert_eq!(path, Path::new("/data/cache"));
    }

    #[test]
    fn a_cached_path_is_rewritten_to_the_sentinel() {
        let composed = cache_path(Path::new("/data/cache"), "abcdef");
        assert_eq!(
            to_sentinel(Path::new("/data/cache"), &composed),
            "<cache>/ab/abcdef"
        );
    }

    #[test]
    fn windows_separators_are_normalised_on_both_sides_before_comparing() {
        assert_eq!(
            to_sentinel(
                Path::new("C:\\Users\\x\\AppData\\cache"),
                Path::new("C:\\Users\\x\\AppData\\cache\\ab\\abcdef"),
            ),
            "<cache>/ab/abcdef",
            "the golden is compared byte for byte by the windows job as well as this one"
        );
    }

    #[test]
    fn a_trailing_separator_on_the_cache_dir_does_not_eat_the_leading_one() {
        assert_eq!(
            to_sentinel(
                Path::new("/data/cache/"),
                Path::new("/data/cache/ab/abcdef")
            ),
            "<cache>/ab/abcdef"
        );
    }

    #[test]
    fn a_path_outside_the_cache_keeps_its_own_shape() {
        assert_eq!(
            to_sentinel(
                Path::new("/data/cache"),
                Path::new("/data/exports/book.mp3")
            ),
            "/data/exports/book.mp3",
            "rewriting a non-cache path would hide where the file actually came from"
        );
    }
}
