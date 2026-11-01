use std::path::{Path, PathBuf};

use txtcaster_lib::reading::anchor::{anchor_segment, BlockRef, GeometryWord, PageGeometry, Rect};

const UPDATE_ENV: &str = "UPDATE_ANCHOR_CONFORMANCE";

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../engine/tests/anchor_conformance.json")
}

fn geometry(tokens: &[(&str, usize)]) -> PageGeometry {
    let mut lines: Vec<Rect> = Vec::new();
    let words = tokens
        .iter()
        .enumerate()
        .map(|(slot, (token, line))| {
            while lines.len() <= *line {
                let idx = lines.len() as f64;
                lines.push(Rect {
                    x0: 50.0,
                    y0: 100.0 + idx * 20.0,
                    x1: 550.0,
                    y1: 112.0 + idx * 20.0,
                });
            }
            GeometryWord {
                rect: Rect {
                    x0: 50.0 + slot as f64 * 25.0,
                    y0: 100.0 + *line as f64 * 20.0,
                    x1: 70.0 + slot as f64 * 25.0,
                    y1: 112.0 + *line as f64 * 20.0,
                },
                token: token.to_string(),
                line: *line,
            }
        })
        .collect();
    PageGeometry {
        page_width_pt: 612.0,
        page_height_pt: 792.0,
        lines,
        words,
    }
}

struct Case {
    name: &'static str,
    spoken: Vec<&'static str>,
    page: Vec<(&'static str, usize)>,
    block: Option<Rect>,
    kind: &'static str,
    countable: bool,
    reference: Option<Vec<&'static str>>,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "plain forward walk",
            spoken: vec!["raw", "cpu", "power"],
            page: vec![("Raw", 0), ("CPU", 0), ("power", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "repeated word disambiguated by the block",
            spoken: vec!["system", "alpha"],
            page: vec![("system", 0), ("alpha", 0), ("system", 1), ("beta", 1)],
            block: Some(Rect {
                x0: 90.0,
                y0: 115.0,
                x1: 560.0,
                y1: 140.0,
            }),
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "hyphen fragments are not joined",
            spoken: vec!["systems"],
            page: vec![("sys", 0), ("tems", 1)],
            block: None,
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "figure cue takes the page rung and leaves the denominator",
            spoken: vec!["figure", "one", "shows"],
            page: vec![("Figure", 0), ("one", 0)],
            block: None,
            kind: "figure_cue",
            countable: false,
            reference: None,
        },
        Case {
            name: "nothing located falls to the block",
            spoken: vec!["lecture", "commentary"],
            page: vec![("printed", 0), ("prose", 0)],
            block: Some(Rect {
                x0: 40.0,
                y0: 90.0,
                x1: 560.0,
                y1: 140.0,
            }),
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "apostrophes fold rather than split",
            spoken: vec!["don't", "stop"],
            page: vec![("dont", 0), ("stop", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "a compound page word serves two needles",
            spoken: vec!["man", "made", "systems"],
            page: vec![("man-made", 0), ("systems", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "a compound spoken entry searches all of its tokens",
            spoken: vec!["man-made"],
            page: vec![("man", 0), ("made", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "a reference the segment never said demotes the match",
            spoken: vec!["alpha", "beta", "gamma"],
            page: vec![("alpha", 0), ("beta", 0), ("gamma", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: Some(vec!["alpha", "beta"]),
        },
        Case {
            name: "a reference that agrees demotes nothing",
            spoken: vec!["alpha", "beta"],
            page: vec![("alpha", 0), ("beta", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: Some(vec!["alpha", "beta"]),
        },
        Case {
            name: "every match demoted lands on the line rung",
            spoken: vec!["alpha", "beta"],
            page: vec![("alpha", 0), ("beta", 0)],
            block: None,
            kind: "text",
            countable: true,
            reference: Some(vec!["gamma"]),
        },
        Case {
            name: "a block that encloses no page word falls back to the whole page",
            spoken: vec!["systems"],
            page: vec![("systems", 0)],
            block: Some(Rect {
                x0: 10.0,
                y0: 600.0,
                x1: 40.0,
                y1: 640.0,
            }),
            kind: "text",
            countable: true,
            reference: None,
        },
        Case {
            name: "code block placeholder leaves the denominator",
            spoken: vec!["a", "code", "block", "appears", "here"],
            page: vec![("printed", 0)],
            block: None,
            kind: "text",
            countable: false,
            reference: None,
        },
    ]
}

#[test]
fn the_python_probe_and_the_rust_ladder_agree_on_every_case() {
    let mut rendered = Vec::new();
    for case in cases() {
        let geometry = geometry(&case.page);
        let block = case.block.map(|bbox| BlockRef {
            reading_order: 0,
            bbox,
            text: String::new(),
        });
        let spoken: Vec<String> = case.spoken.iter().map(|s| s.to_string()).collect();
        let reference: Option<Vec<String>> = case
            .reference
            .as_ref()
            .map(|tokens| tokens.iter().map(|t| t.to_string()).collect());
        let anchor = anchor_segment(
            &spoken,
            &geometry,
            block.as_ref(),
            case.kind,
            case.countable,
            reference.as_deref(),
        );
        rendered.push(serde_json::json!({
            "name": case.name,
            "spoken": case.spoken,
            "page": case.page.iter().map(|(t, l)| serde_json::json!([t, l])).collect::<Vec<_>>(),
            "block": case.block.map(|b| vec![b.x0, b.y0, b.x1, b.y1]),
            "kind": case.kind,
            "countable": case.countable,
            "reference": case.reference,
            "expect": {
                "rung": anchor.rung.as_str(),
                "tokens": anchor.tokens,
                "rateTokens": anchor.rate_tokens,
                "located": anchor.located,
                "demoted": anchor.demoted,
            },
        }));
    }
    let payload = serde_json::to_string_pretty(&serde_json::json!({ "cases": rendered })).unwrap();
    let path = fixture_path();
    if std::env::var(UPDATE_ENV).is_ok() {
        std::fs::write(&path, format!("{payload}\n")).expect("write conformance fixture");
        return;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "the conformance fixture is the only thing stopping the Python corpus probe from \
             measuring a different matcher than the product ships — the R-R/R-AE two-extractor \
             disease in a third costume. Regenerate with {UPDATE_ENV}=1 cargo test --test \
             anchor_conformance ({error})"
        )
    });
    assert_eq!(
        on_disk.trim(),
        payload.trim(),
        "the Rust ladder moved without the corpus probe's fixture moving with it"
    );
}
