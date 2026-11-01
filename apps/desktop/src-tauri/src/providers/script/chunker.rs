//! Port of `engine/txtcaster_engine/script/chunker.py`: unit costing,
//! oversized-unit splitting at sentence/line/word boundaries, and
//! heading-bounded chunk flushing. Constants verbatim.

pub const CHARS_PER_TOKEN: usize = 4;
pub const MIN_CHUNK_CHARS: usize = 512;

/// The source-unit facts the script contract consumes — the engine's
/// `SourceUnit` fields that chunking, prompting, and marker anchoring read.
/// Serialized shape matches the engine's `/script/units` payload.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceUnit {
    pub chapter_idx: i64,
    pub page_no: i64,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub figure_id: Option<String>,
    #[serde(default)]
    pub region_type: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub block_index: Option<i64>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub idx: usize,
    pub chapter_idx: i64,
    pub page_no: i64,
    pub units: Vec<SourceUnit>,
}

fn unit_cost(unit: &SourceUnit) -> usize {
    unit.text.chars().count() + 2
}

fn split_oversized(unit: &SourceUnit, budget: usize) -> Vec<SourceUnit> {
    if unit.text.chars().count() <= budget || unit.kind == "figure" {
        return vec![unit.clone()];
    }
    let mut pieces = Vec::new();
    let mut remaining: Vec<char> = unit.text.chars().collect();
    while remaining.len() > budget {
        let window: String = remaining[..budget].iter().collect();
        let split_at = [window.rfind(". "), window.rfind('\n'), window.rfind(' ')]
            .into_iter()
            .flatten()
            .max()
            .map(|byte_at| window[..byte_at].chars().count())
            .unwrap_or(0);
        let split_at = if split_at == 0 { budget } else { split_at };
        let head: String = remaining[..(split_at + 1).min(remaining.len())]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        remaining = remaining[(split_at + 1).min(remaining.len())..].to_vec();
        while remaining.first().is_some_and(|c| c.is_whitespace()) {
            remaining.remove(0);
        }
        if !head.is_empty() {
            let mut piece = unit.clone();
            piece.text = head;
            pieces.push(piece);
        }
    }
    let tail: String = remaining.iter().collect::<String>().trim().to_string();
    if !tail.is_empty() {
        let mut piece = unit.clone();
        piece.text = tail;
        pieces.push(piece);
    }
    pieces
}

pub fn chunk_units(units: &[SourceUnit], budget_chars: usize) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current: Vec<SourceUnit> = Vec::new();
    let mut current_cost = 0usize;

    fn flush(chunks: &mut Vec<Chunk>, current: &mut Vec<SourceUnit>, cost: &mut usize) {
        if current.is_empty() {
            return;
        }
        chunks.push(Chunk {
            idx: chunks.len(),
            chapter_idx: current[0].chapter_idx,
            page_no: current[0].page_no,
            units: std::mem::take(current),
        });
        *cost = 0;
    }

    for unit in units {
        let chapter_boundary = !current.is_empty() && unit.chapter_idx != current[0].chapter_idx;
        let section_boundary = unit.kind == "heading";
        if (chapter_boundary || section_boundary) && !current.is_empty() {
            flush(&mut chunks, &mut current, &mut current_cost);
        }
        for piece in split_oversized(unit, budget_chars) {
            let cost = unit_cost(&piece);
            if !current.is_empty() && current_cost + cost > budget_chars {
                flush(&mut chunks, &mut current, &mut current_cost);
            }
            current.push(piece);
            current_cost += cost;
        }
    }
    flush(&mut chunks, &mut current, &mut current_cost);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_unit(text: &str) -> SourceUnit {
        SourceUnit {
            chapter_idx: 0,
            page_no: 1,
            kind: "text".to_string(),
            text: text.to_string(),
            figure_id: None,
            region_type: None,
            caption: None,
            explanation: None,
            block_index: None,
            label: None,
        }
    }

    #[test]
    fn headings_start_fresh_chunks_and_budgets_flush() {
        let mut heading = text_unit("Chapter Two");
        heading.kind = "heading".to_string();
        let units = vec![text_unit("first body"), heading, text_unit("second body")];
        let chunks = chunk_units(&units, 10_000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].units.len(), 1);
        assert_eq!(chunks[1].units.len(), 2);
        assert_eq!(chunks[1].units[0].kind, "heading");
    }

    #[test]
    fn chapter_boundary_flushes_even_with_large_budget() {
        let mut u1 = text_unit("first chapter body");
        u1.chapter_idx = 0;
        let mut u2 = text_unit("second chapter body");
        u2.chapter_idx = 1;
        let chunks = chunk_units(&[u1, u2], 100_000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chapter_idx, 0);
        assert_eq!(chunks[1].chapter_idx, 1);
    }

    #[test]
    fn oversized_units_split_at_natural_boundaries_and_figures_never_split() {
        let long = text_unit(&"A sentence here. ".repeat(100));
        let chunks = chunk_units(&[long], 200);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            for unit in &chunk.units {
                assert!(unit.text.chars().count() <= 200);
            }
        }
        let mut figure = text_unit(&"x".repeat(500));
        figure.kind = "figure".to_string();
        let chunks = chunk_units(&[figure], 200);
        assert_eq!(chunks[0].units[0].text.chars().count(), 500);
    }
}
