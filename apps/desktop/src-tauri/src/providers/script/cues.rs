//! Port of the `decide_code` half of `engine/txtcaster_engine/script/cues.py`:
//! an explicit `code` label decides; otherwise the shape heuristic does.

use super::shape::looks_like_code;

pub const CODE_KIND: &str = "code";

pub fn decide_code(label: Option<&str>, text: &str) -> bool {
    match label {
        Some(label) => label == CODE_KIND,
        None => looks_like_code(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_decide_and_heuristic_covers_the_unlabelled() {
        assert!(decide_code(Some("code"), "anything"));
        assert!(!decide_code(Some("body"), "let x = f(y);"));
        assert!(decide_code(None, "let x = f(y); return x;"));
        assert!(!decide_code(None, "Plain prose about systems."));
    }
}
