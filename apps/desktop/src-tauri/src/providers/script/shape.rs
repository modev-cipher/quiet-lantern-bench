//! Port of `engine/txtcaster_engine/script/shape.py` `looks_like_code`:
//! six signal patterns, code at two or more hits. Kept in behavioral
//! lockstep by the shared corpus in `teacher.rs`.

use std::sync::OnceLock;

use regex::Regex;

const CODE_SIGNAL_MIN: usize = 2;

fn signals() -> &'static [Regex; 6] {
    static SIGNALS: OnceLock<[Regex; 6]> = OnceLock::new();
    SIGNALS.get_or_init(|| {
        [
            Regex::new(
                r"\b(?:def|class|function|return|import|elif|const|let|var|void|public|private|static|lambda|yield|async|await|struct|impl|fn)\b",
            )
            .expect("keyword signal"),
            Regex::new(r"[A-Za-z_][A-Za-z0-9_]*\s*\([^)]*\)").expect("call signal"),
            Regex::new(r"[{};]").expect("brace signal"),
            Regex::new(r"(?:=>|::|->|\+=|-=|==|!=|<=|>=|&&|\|\|)").expect("operator signal"),
            Regex::new(r"(?m)^[ \t]{2,}\S").expect("indent signal"),
            Regex::new(r"[A-Za-z_][A-Za-z0-9_]*\s*=\s*[^=\s]").expect("assignment signal"),
        ]
    })
}

pub fn looks_like_code(text: &str) -> bool {
    signals()
        .iter()
        .filter(|pattern| pattern.is_match(text))
        .count()
        >= CODE_SIGNAL_MIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_and_prose_classify_like_the_engine() {
        assert!(looks_like_code("def main():\n    return compute(x)"));
        assert!(looks_like_code("let x = foo(bar); // etc"));
        assert!(!looks_like_code(
            "The chapter is basically setting the stage for why reliability matters."
        ));
        assert!(!looks_like_code("A short heading"));
    }
}
