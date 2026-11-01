//! B-0 measured-regression harness (charter §9.1.1). Runs the preserved spike
//! fixture against the production provider and checks the tightened ceilings:
//! build ≤150 ms, disk ≤1.0 MB, 3-variant top-k-5 p95 ≤2.5 ms, p99 ≤3.0 ms,
//! max ≤5.0 ms, cold-open first query ≤2.0 ms, serialized top-k-5 payload
//! ≤900 bytes at the 15-word snippet default, needle HIT@1 2/2, code-switch
//! 6/6. Emits one machine-readable JSON report line and exits nonzero on any
//! pin failure. Ceilings bind on release builds only (`--release`).
//!
//! Run: cargo run --release --features book-agent --bin book-agent-bench

use std::time::Instant;

use txtcaster_lib::retrieval::contract::{self, HandleTable};
use txtcaster_lib::retrieval::fixture;
use txtcaster_lib::retrieval::{
    BookFingerprint, RetrievalProvider, SearchOptions, TantivyRetrieval,
};

fn main() {
    let debug_build = cfg!(debug_assertions);
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = TantivyRetrieval::new(dir.path());
    let fp = BookFingerprint::new("bench-book", "job-0001", "hash-0001", "blocks-v1");

    let rows = fixture::generate(0xB00C_A6E7);
    let text_kb: usize = rows.iter().map(|r| r.text.len()).sum::<usize>() / 1024;

    // Build + disk.
    let stats = provider.index_book(&fp, &rows).expect("index build");
    assert!(provider.is_indexed(&fp), "is_indexed after build");

    // Latency: 120 multi-query searches, 3 variants each, after 5 warm-ups.
    let bank = query_bank();
    let opts = SearchOptions::default();
    for q in bank.iter().take(5) {
        provider.search(&fp, q, &opts).expect("warmup");
    }
    let mut lat_us: Vec<u128> = Vec::with_capacity(bank.len());
    for q in &bank {
        let t = Instant::now();
        provider.search(&fp, q, &opts).expect("search");
        lat_us.push(t.elapsed().as_micros());
    }
    lat_us.sort_unstable();
    let pct = |p: f64| lat_us[((lat_us.len() as f64 * p) as usize).min(lat_us.len() - 1)];
    let (p50, p95, p99, max) = (pct(0.50), pct(0.95), pct(0.99), *lat_us.last().unwrap());

    // Cold-open first query: fresh provider, new mmap.
    let cold_provider = TantivyRetrieval::new(dir.path());
    let t = Instant::now();
    cold_provider
        .search(&fp, &["astrolabe calibration".into()], &opts)
        .expect("cold search");
    let cold_us = t.elapsed().as_micros();

    // Needle recall.
    let mut needle_hits = 0;
    for (term, _) in fixture::NEEDLES {
        let hits = provider
            .search(&fp, &[term.to_string()], &opts)
            .expect("needle");
        if hits
            .first()
            .map(|h| h.snippet.to_lowercase().contains(term))
            .unwrap_or(false)
        {
            needle_hits += 1;
        }
    }

    // Code-switched probes over the main dual-field path.
    let cs_probes: &[(&str, &str)] = &[
        ("kamal latitude instrument", "الكمال"),
        ("الكمال knotted cord", "الكمال"),
        ("Canopus سهيل southern", "سهيل"),
        ("lunar mansions كتاب الفوائد", "الفوائد"),
        ("حساب reckoning drift", "حساب"),
        ("قياس ارتفاع النجوم بالأسطرلاب", "الأسطرلاب"),
    ];
    let mut cs_hits = 0;
    for (query, target) in cs_probes {
        let hits = provider
            .search(&fp, &[query.to_string()], &opts)
            .expect("cs");
        if hits.iter().any(|h| h.snippet.contains(target)) {
            cs_hits += 1;
        }
    }

    // Serialized tool payload on the preserved 3-variant payload fixture.
    let mut handles = HandleTable::new(fp.clone());
    let result = contract::search_book(
        &provider,
        &mut handles,
        &[
            "who refined sight-reduction procedures".into(),
            "sight reduction refinement history".into(),
            "navigators improving sight reduction".into(),
        ],
        &opts,
    );
    let payload = contract::serialize_bounded(&result);
    let payload_bytes = payload.len();

    // Pins (charter §9.1.1). Latency/build pins bind on release only.
    let mut failures: Vec<String> = Vec::new();
    let mut pin = |name: &str, ok: bool, timing: bool| {
        if !ok && !(timing && debug_build) {
            failures.push(name.to_string());
        }
    };
    pin("build_ms<=150", stats.build_ms <= 150, true);
    pin("disk<=1.0MB", stats.disk_bytes <= 1_000_000, false);
    pin("p95<=2.5ms", p95 <= 2_500, true);
    pin("p99<=3.0ms", p99 <= 3_000, true);
    pin("max<=5.0ms", max <= 5_000, true);
    pin("cold_first<=2.0ms", cold_us <= 2_000, true);
    pin("needles 2/2", needle_hits == fixture::NEEDLES.len(), false);
    pin("code_switch 6/6", cs_hits == cs_probes.len(), false);
    pin(
        "payload<=900B",
        payload_bytes <= contract::MAX_PAYLOAD_BYTES,
        false,
    );

    let report = serde_json::json!({
        "harness": "book-agent-bench",
        "profile": if debug_build { "debug" } else { "release" },
        "fixture": { "segments": rows.len(), "text_kb": text_kb, "seed": "0xB00CA6E7" },
        "build_ms": stats.build_ms,
        "disk_bytes": stats.disk_bytes,
        "searches": bank.len(),
        "latency_us": { "p50": p50, "p95": p95, "p99": p99, "max": max },
        "cold_first_query_us": cold_us,
        "needle_hits": needle_hits,
        "code_switch_hits": cs_hits,
        "payload_bytes": payload_bytes,
        "estimated_payload_tokens": payload_bytes / 4,
        "failures": failures,
        "pass": failures.is_empty(),
    });
    println!("{report}");
    if debug_build {
        eprintln!(
            "warning: debug build — timing ceilings not enforced; run with --release for the gate"
        );
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
}

/// 120 questions, each as 3 LLM-style lexical variants (preserved spike bank).
fn query_bank() -> Vec<Vec<String>> {
    let topics = [
        "astrolabe",
        "compass",
        "chronometer",
        "sextant",
        "latitude",
        "longitude",
        "monsoon",
        "polaris",
        "kamal",
        "quadrant",
        "star catalogs",
        "tidal tables",
        "portolan charts",
        "dead reckoning",
        "lunar distance",
        "declination",
        "great circle",
        "cross-staff",
        "equation of time",
        "celestial sphere",
    ];
    let frames: [fn(&str) -> [String; 3]; 4] = [
        |t| {
            [
                format!("who used the {t}"),
                format!("{t} history sailors"),
                format!("origin of the {t}"),
            ]
        },
        |t| {
            [
                format!("how was the {t} calibrated"),
                format!("{t} calibration error"),
                format!("measuring accuracy {t}"),
            ]
        },
        |t| {
            [
                format!("what does the author say about {t}"),
                format!("{t} chapter discussion"),
                format!("{t} significance navigation"),
            ]
        },
        |t| {
            [
                format!("{t} secret guild"),
                format!("{t} taught spread"),
                format!("{t} pepper routes generation"),
            ]
        },
    ];
    let mut bank: Vec<Vec<String>> = Vec::new();
    for f in &frames {
        for t in &topics {
            bank.push(f(t).into_iter().collect());
        }
    }
    let extra: &[[&str; 3]] = &[
        ["قياس ارتفاع النجوم", "ارتفاع النجم القطبي", "الأسطرلاب قياس"],
        ["المنازل القمرية", "منازل القمر", "ابن ماجد المنازل"],
        ["الرياح الموسمية", "هبوب الرياح", "المحيط الهندي الرياح"],
        ["جداول الكواكب بغداد", "الفلكيون جداول", "مواقع الكواكب"],
        [
            "kamal الكمال instrument",
            "knotted cord latitude",
            "wooden tablet الكمال",
        ],
        [
            "Canopus سهيل",
            "southern star سهيل",
            "Zanzibar routes Canopus",
        ],
        [
            "quantum blockchain synergy",
            "cryptocurrency staking yield",
            "neural network dropout",
        ],
        [
            "photosynthesis chlorophyll",
            "mitochondria membrane",
            "ribosome translation",
        ],
    ];
    for e in extra {
        for _ in 0..5 {
            bank.push(e.iter().map(|s| s.to_string()).collect());
        }
    }
    bank
}
