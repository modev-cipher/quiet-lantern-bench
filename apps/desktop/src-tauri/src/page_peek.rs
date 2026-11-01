use std::collections::HashMap;

use crate::reading::ReadingChapter;

pub const PEEK_MAX_CHARS: usize = 420;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PagePeek {
    pub page_no: i64,
    pub chapter_title: Option<String>,
    pub text: String,
    pub clipped: bool,
}

pub fn page_peek(
    chapters: &[ReadingChapter],
    titles: &HashMap<String, String>,
    page_no: i64,
) -> Option<PagePeek> {
    let mut chapter_title = None;
    let mut joined = String::new();
    for chapter in chapters {
        for segment in chapter.segments.iter().filter(|s| s.page_no == page_no) {
            let text = segment
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                continue;
            }
            if chapter_title.is_none() {
                chapter_title = titles
                    .get(&chapter.chapter_id)
                    .map(|title| title.trim().to_string())
                    .filter(|title| !title.is_empty());
            }
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(&text);
            if joined.chars().count() > PEEK_MAX_CHARS {
                break;
            }
        }
    }
    if joined.is_empty() {
        return None;
    }
    let (text, clipped) = clip_on_word(&joined, PEEK_MAX_CHARS);
    Some(PagePeek {
        page_no,
        chapter_title,
        text,
        clipped,
    })
}

fn clip_on_word(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let cut: String = text.chars().take(max_chars).collect();
    let clipped = match cut.rfind(char::is_whitespace) {
        Some(at) if at > 0 => cut[..at].trim_end().to_string(),
        _ => cut,
    };
    (clipped, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reading::{GeometryOutcome, ReadingSegment, TimelineOrigin};
    use crate::words::WordProvenance;

    fn segment(idx: i64, page_no: i64, text: &str) -> ReadingSegment {
        ReadingSegment {
            segment_id: format!("s{idx}"),
            idx,
            page_no,
            kind: "body".into(),
            figure_id: None,
            text: text.into(),
            start_ms: idx * 1_000,
            end_ms: idx * 1_000 + 900,
            provenance: WordProvenance::Missing,
            words: Vec::new(),
            anchor: crate::reading::anchor::Rung::Page,
            rects: Vec::new(),
        }
    }

    fn chapter(chapter_id: &str, segments: Vec<ReadingSegment>) -> ReadingChapter {
        ReadingChapter {
            anchor_geometry: GeometryOutcome::Unavailable,
            chapter_id: chapter_id.into(),
            start_ms: 0,
            end_ms: 0,
            origin: TimelineOrigin::Measured,
            segments,
            mode: None,
        }
    }

    fn titles() -> HashMap<String, String> {
        HashMap::from([
            ("c1".to_string(), "Introduction".to_string()),
            ("c2".to_string(), "  ".to_string()),
        ])
    }

    #[test]
    fn a_page_peek_joins_that_page_only_with_its_chapter_title() {
        let chapters = vec![
            chapter(
                "c1",
                vec![
                    segment(0, 11, "Before the page."),
                    segment(1, 12, "A distributed  system\nis a collection"),
                    segment(2, 12, "of autonomous computers."),
                ],
            ),
            chapter("c2", vec![segment(3, 13, "After.")]),
        ];
        assert_eq!(
            page_peek(&chapters, &titles(), 12),
            Some(PagePeek {
                page_no: 12,
                chapter_title: Some("Introduction".to_string()),
                text: "A distributed system is a collection of autonomous computers.".to_string(),
                clipped: false,
            })
        );
    }

    #[test]
    fn a_page_without_text_has_no_peek_and_a_blank_title_is_absent() {
        let chapters = vec![chapter(
            "c2",
            vec![segment(0, 5, "   "), segment(1, 6, "Text.")],
        )];
        assert_eq!(page_peek(&chapters, &titles(), 5), None);
        assert_eq!(page_peek(&chapters, &titles(), 99), None);
        assert_eq!(
            page_peek(&chapters, &titles(), 6).and_then(|peek| peek.chapter_title),
            None
        );
    }

    #[test]
    fn a_long_page_clips_on_a_word_boundary_and_says_so() {
        let long = "مرحبا بالعالم ".repeat(60);
        let chapters = vec![chapter("c1", vec![segment(0, 7, &long)])];
        let peek = page_peek(&chapters, &titles(), 7).expect("peek");
        assert!(peek.clipped);
        assert!(peek.text.chars().count() <= PEEK_MAX_CHARS);
        assert!(long.starts_with(&peek.text));
        assert!(!peek.text.ends_with(' '));
        assert!(peek.text.ends_with("بالعالم") || peek.text.ends_with("مرحبا"));
    }
}
