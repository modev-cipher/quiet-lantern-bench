use std::path::Path;

use txtcaster_lib::reading::anchor::{
    anchor_segment, parse_geometry, GeometryWord, PageGeometry, Rung,
};

fn fixture() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/whisperx_page25.json");
    let raw = std::fs::read_to_string(path).expect("the real-ASR fixture must be committed");
    serde_json::from_str(&raw).expect("fixture json")
}

fn geometry(value: &serde_json::Value) -> PageGeometry {
    parse_geometry(&value["page"].to_string()).expect("page geometry")
}

fn asr_tokens(value: &serde_json::Value) -> Vec<String> {
    value["asr_words"]
        .as_array()
        .expect("asr words")
        .iter()
        .map(|entry| entry["w"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn real_faster_whisper_output_anchors_on_the_real_page_it_was_spoken_from() {
    let value = fixture();
    let geometry = geometry(&value);
    let spoken = asr_tokens(&value);

    assert!(
        spoken.len() >= 24,
        "the fixture must carry a real utterance"
    );
    assert!(
        geometry.words.len() > 200,
        "and the real page it was spoken from"
    );

    let anchor = anchor_segment(&spoken, &geometry, None, "text", true, None);

    assert_eq!(anchor.rung, Rung::Word);
    assert!(
        anchor.located * 100 / anchor.rate_tokens >= 70,
        "an ASR producer has never written a row on any machine this has run on, so this \
         fixture is the only evidence the ladder survives ASR tokens at all: located {} of {}",
        anchor.located,
        anchor.rate_tokens
    );
}

#[test]
fn the_asr_hyphen_artifact_is_a_real_token_that_no_fake_backend_would_have_produced() {
    let value = fixture();
    let spoken = asr_tokens(&value);
    let joined = spoken.join(" ");

    assert!(
        joined.contains("-made") || joined.contains("-free"),
        "faster-whisper splits hyphenated compounds into a token carrying the hyphen; \
         `FakeAlignBackend` echoes stream 1 and would never emit one, which is why §9.2 \
         requires at least one matcher test on real output: {joined}"
    );
}

#[test]
fn the_mis_anchor_guard_demotes_an_asr_token_the_segment_never_said() {
    let value = fixture();
    let mut geometry = geometry(&value);
    geometry.words.push(GeometryWord {
        rect: txtcaster_lib::reading::anchor::Rect {
            x0: 500.0,
            y0: 700.0,
            x1: 540.0,
            y1: 712.0,
        },
        token: "misrecognised".to_string(),
        line: 0,
    });
    let mut spoken = asr_tokens(&value);
    spoken.push("misrecognised".to_string());
    let reference: Vec<String> = vec![value["asr_words"]
        .as_array()
        .expect("asr words")
        .iter()
        .map(|entry| entry["w"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")];

    let guarded = anchor_segment(&spoken, &geometry, None, "text", true, Some(&reference));
    let unguarded = anchor_segment(&spoken, &geometry, None, "text", true, None);

    assert_eq!(
        guarded.demoted, 1,
        "the token exists on the page and the unguarded walk anchors it with full confidence — \
         the one failure mode every rung and every WordProvenance value reports as success"
    );
    assert_eq!(unguarded.demoted, 0);
    assert!(guarded.word_rects.last().expect("last").is_none());
    assert!(unguarded.word_rects.last().expect("last").is_some());
}
