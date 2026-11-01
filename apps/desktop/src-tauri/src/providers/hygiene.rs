//! Source-level enforcement of the provider-flexibility custody and boundary
//! law (charter §3.2 hygiene gate, §4.2 custody, §5.6 O-1 boundary). Same
//! path-anchored discipline as `voice::keys::hygiene` and
//! `voice::excerpt::scan`: the walks pin files that must exist, so a move
//! reds the anchor instead of silently narrowing the scan to nothing.

#![cfg(test)]

use std::path::PathBuf;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn engine_root() -> PathBuf {
    crate_root().join("../../../engine/txtcaster_engine")
}

fn python_sources(dir: &PathBuf, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("engine directory readable") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            python_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "py") {
            let source = std::fs::read_to_string(&path).expect("python source readable");
            out.push((path.display().to_string(), source));
        }
    }
}

fn engine_sources() -> Vec<(String, String)> {
    let root = engine_root();
    assert!(
        root.join("__main__.py").exists(),
        "engine root moved; re-point the hygiene scan rather than let it go vacuous"
    );
    for anchor in [
        "ingest/textlayer.py",
        "ingest/pdfium_adapter.py",
        "ingest/fallback/runner.py",
        "ingest/fallback/keys.py",
        "ingest/fallback/providers/gemini.py",
    ] {
        assert!(
            root.join(anchor).exists(),
            "{anchor} moved; re-anchor the hygiene scan rather than let it narrow"
        );
    }
    let mut sources = Vec::new();
    python_sources(&root, &mut sources);
    assert!(
        sources.len() > 20,
        "the walk found suspiciously few engine sources ({})",
        sources.len()
    );
    sources
        .into_iter()
        .map(|(path, source)| {
            let relative = std::path::Path::new(&path)
                .strip_prefix(&root)
                .expect("every walked source sits under the engine root")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (relative, source)
        })
        .collect()
}

const FALLBACK_PACKAGE: &str = "ingest/fallback/";

/// Owner rulings #36/#40 supersede the charter §3.2 "keyless engine" sentence for
/// exactly one package: the BYOK vision cascade lives in `ingest/fallback/` behind the
/// pdfium path, keys arrive per request in `server.py`'s `FallbackRequest` and are never
/// stored. Provider hostnames, auth headers and key-field spellings are confined to the
/// transport files below, save the two Batch endpoint constants in `batch.py`, pinned by
/// exact line until they move under `providers/`; the vendor name may appear across the
/// cascade package and as the one request field; nothing else may name provider material.
fn may_carry_wire_material(path: &str) -> bool {
    path.starts_with("ingest/fallback/providers/") || path == "ingest/fallback/keys.py"
}

const WIRE_NEEDLES: [&str; 8] = [
    "api_key",
    "apikey",
    "anthropic",
    "googleapis",
    "openrouter",
    "x-goog",
    "x-api-key",
    "generativelanguage",
];

const HOSTNAME_NEEDLES: [&str; 4] = [
    "api.openai.com",
    "googleapis.com",
    "openrouter.ai",
    "anthropic.com",
];

const BATCH_ENDPOINT_CONSTANTS: [&str; 2] = [
    "OPENAI_FILES_URL = \"https://api.openai.com/v1/files\"",
    "OPENAI_BATCHES_URL = \"https://api.openai.com/v1/batches\"",
];

#[test]
fn the_engine_stays_keyless_and_cloud_free() {
    for (path, source) in engine_sources() {
        if may_carry_wire_material(&path) {
            continue;
        }
        let lower = source.to_lowercase();
        for needle in WIRE_NEEDLES {
            assert!(
                !lower.contains(needle),
                "{path}: provider hostname, header or key-field spelling ({needle:?}) outside \
                 ingest/fallback/providers/ and keys.py (charter §3.2 as re-scoped by #36/#40)"
            );
        }
        let hostname_scope = if path == "ingest/fallback/batch.py" {
            let mut stripped = source.clone();
            for constant in BATCH_ENDPOINT_CONSTANTS {
                assert!(
                    source.lines().any(|line| line == constant),
                    "{path}: the pinned Batch endpoint constant drifted: {constant}"
                );
                stripped = stripped.replace(constant, "");
            }
            stripped.to_lowercase()
        } else {
            lower.clone()
        };
        for needle in HOSTNAME_NEEDLES {
            assert!(
                !hostname_scope.contains(needle),
                "{path}: provider hostname ({needle:?}) outside ingest/fallback/providers/ and \
                 keys.py; batch.py may carry only its two pinned endpoint constants"
            );
        }
        if path.starts_with(FALLBACK_PACKAGE) {
            continue;
        }
        let outside_the_request_field = if path == "server.py" {
            lower.replace("openai_key", "").replace("openai=", "")
        } else {
            lower
        };
        assert!(
            !outside_the_request_field.contains("openai"),
            "{path}: a provider name entered engine code outside the cascade package; the \
             native and text-layer paths must stay cloud-free (charter §3.2, #36/#40)"
        );
    }
}

#[test]
fn only_the_request_model_and_the_pdfium_call_sites_import_the_cascade() {
    let mut importers: Vec<String> = engine_sources()
        .into_iter()
        .filter(|(path, _)| !path.starts_with(FALLBACK_PACKAGE))
        .filter(|(_, source)| {
            source.lines().map(str::trim).any(|line| {
                (line.starts_with("from ") || line.starts_with("import "))
                    && (line.contains("ingest.fallback")
                        || line.contains(".fallback ")
                        || line.contains("import fallback"))
            })
        })
        .map(|(path, _)| path)
        .collect();
    importers.sort_unstable();
    assert_eq!(
        importers,
        vec!["ingest/endpoint.py", "ingest/manifest.py", "server.py"],
        "the cascade's import graph is owner law: the request model, the ingest endpoint and \
         the pdfium manifest step only; native and text-layer code stays import-free of it"
    );
}

#[test]
fn the_cascade_never_stores_a_key_or_reads_one_from_the_environment() {
    let package: Vec<(String, String)> = engine_sources()
        .into_iter()
        .filter(|(path, _)| path.starts_with(FALLBACK_PACKAGE))
        .collect();
    assert!(
        package.len() >= 10,
        "the cascade package walk found suspiciously few sources ({})",
        package.len()
    );
    let mut expose_sites: Vec<(String, usize, String)> = Vec::new();
    for (path, source) in &package {
        let lower = source.to_lowercase();
        for needle in [
            "keyring", "dotenv", "\".env\"", "'.env'", "environ", "getenv",
        ] {
            assert!(
                !lower.contains(needle),
                "{path}: the cascade reaches for {needle:?}; keys arrive per request and are \
                 never stored or read from the environment (#36/#40)"
            );
        }
        for (index, line) in source.lines().enumerate() {
            let writes = ["write_text(", "write_bytes(", "open(", "json.dump"]
                .iter()
                .any(|call| line.contains(call));
            if writes {
                let lowered = line.to_lowercase();
                assert!(
                    !lowered.contains("secret") && !lowered.contains("expose"),
                    "{path}:{}: a write touches key material: {line}",
                    index + 1
                );
            }
            if line.contains("expose()") {
                expose_sites.push((path.clone(), index + 1, line.trim().to_string()));
            }
        }
    }
    for (path, line_no, line) in &expose_sites {
        assert!(
            may_carry_wire_material(path),
            "{path}:{line_no}: key bytes exposed outside the transport files: {line}"
        );
        assert!(
            line.contains("headers") || path == "ingest/fallback/keys.py",
            "{path}:{line_no}: expose() may feed an auth header or the redactor only: {line}"
        );
    }
    assert_eq!(
        expose_sites.len(),
        3,
        "the expose() sites moved; re-pin deliberately or not at all: {expose_sites:?}"
    );
}

/// The metadata half of the module — the adapters are deliberately NOT in
/// this list: charter §3.3 law 3 places key resolution INSIDE adapters, and
/// their own tests pin header-only key transport (never URLs, never bodies
/// beyond the documented auth header).
fn providers_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("providers/mod.rs", include_str!("mod.rs")),
        ("providers/registry.rs", include_str!("registry.rs")),
        ("providers/binding.rs", include_str!("binding.rs")),
        ("providers/connection.rs", include_str!("connection.rs")),
        ("providers/directory.rs", include_str!("directory.rs")),
    ]
}

fn all_provider_sources() -> Vec<(&'static str, &'static str)> {
    let mut sources = providers_sources();
    sources.push(("providers/adapters/mod.rs", include_str!("adapters/mod.rs")));
    sources.push((
        "providers/adapters/openai_compat.rs",
        include_str!("adapters/openai_compat.rs"),
    ));
    sources.push((
        "providers/adapters/gemini.rs",
        include_str!("adapters/gemini.rs"),
    ));
    sources.push(("providers/script/mod.rs", include_str!("script/mod.rs")));
    sources.push((
        "providers/script/teacher.rs",
        include_str!("script/teacher.rs"),
    ));
    sources.push((
        "providers/script/chunker.rs",
        include_str!("script/chunker.rs"),
    ));
    sources.push(("providers/script/cues.rs", include_str!("script/cues.rs")));
    sources.push(("providers/script/shape.rs", include_str!("script/shape.rs")));
    sources
}

/// Charter §4.2: metadata code in the providers module never touches key bytes.
/// Custody stays in `voice::keys`; this metadata module handles references and
/// last4 handed in from the manager's explicit accessor.
#[test]
fn the_providers_metadata_never_touches_key_bytes() {
    for (path, source) in providers_sources() {
        let runtime: String = source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(source)
            .lines()
            .map(|line| line.split("//").next().unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [".expose(", "ApiKey", "DialCandidate", "expose()"] {
            assert!(
                !runtime.contains(needle),
                "{path}: providers metadata code reached for key material ({needle:?})"
            );
        }
    }
}

/// Charter §3.3: key-bearing egress in adapters and model_fetch may feed an auth
/// header only (never URLs, never request bodies).
#[test]
fn all_expose_calls_in_providers_feed_auth_headers_only() {
    let sources = [
        ("providers/model_fetch.rs", include_str!("model_fetch.rs")),
        (
            "providers/adapters/openai_compat.rs",
            include_str!("adapters/openai_compat.rs"),
        ),
        (
            "providers/adapters/gemini.rs",
            include_str!("adapters/gemini.rs"),
        ),
        (
            "providers/adapters/gemini_tts.rs",
            include_str!("adapters/gemini_tts.rs"),
        ),
        (
            "providers/adapters/cartesia_tts.rs",
            include_str!("adapters/cartesia_tts.rs"),
        ),
        (
            "providers/adapters/openai_tts.rs",
            include_str!("adapters/openai_tts.rs"),
        ),
        (
            "providers/adapters/anthropic.rs",
            include_str!("adapters/anthropic.rs"),
        ),
    ];
    let mut sources = sources.to_vec();
    sources.push(("providers/discovery.rs", include_str!("discovery.rs")));
    sources.push((
        "providers/adapters/openai_realtime_tts.rs",
        include_str!("adapters/openai_realtime_tts.rs"),
    ));
    let mut total_expose = 0;
    for (path, source) in sources {
        for (idx, line) in source.lines().enumerate() {
            if line.contains(".expose()") {
                total_expose += 1;
                let lower = line.to_lowercase();
                assert!(
                    lower.contains("header")
                        || lower.contains("authorization")
                        || lower.contains("key"),
                    "{path}:{}: expose() must feed an auth header only: {line}",
                    idx + 1
                );
            }
        }
    }
    assert_eq!(
        total_expose, 13,
        "the total expose() sites in providers moved"
    );
}

/// Charter §5.6 / O-1: no commercial vocabulary in provider surfaces. BYOK
/// is OSS/developer flexibility, never an entitlement.
#[test]
fn provider_surfaces_carry_no_commercial_vocabulary() {
    for (path, source) in all_provider_sources() {
        for needle in [
            "Premium",
            "premium",
            "Upgrade",
            "upgrade",
            "entitlement",
            "subscription",
            "pricing tier",
            "Pro plan",
            "paywall",
        ] {
            assert!(
                !source.contains(needle),
                "{path}: commercial vocabulary ({needle:?}) belongs in Settings → Account only (O-1)"
            );
        }
    }
}

/// Charter §4.1/§4.2: connection metadata fields are exactly the owner's
/// law; the persisted record never grows a key field or a generation
/// parameter without failing here first.
#[test]
fn the_persisted_record_shape_is_pinned() {
    let record = crate::providers::ConnectionRecord {
        id: "c".to_string(),
        name: "n".to_string(),
        provider: crate::providers::ProviderKind::Gemini,
        base_url: None,
        credential_not_required: false,
        default_model: "m".to_string(),
        enabled: true,
        created_unix: 0,
        updated_unix: 0,
    };
    let json = serde_json::to_value(&record).unwrap();
    let mut fields: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        vec![
            "base_url",
            "created_unix",
            "credential_not_required",
            "default_model",
            "enabled",
            "id",
            "name",
            "provider",
            "updated_unix",
        ],
        "the stored connection shape is owner law (charter §4.1); \
         re-pin deliberately or not at all"
    );
    for banned in [
        "key",
        "temperature",
        "token",
        "top_p",
        "seed",
        "timeout",
        "retry",
    ] {
        assert!(
            !fields.iter().any(|f| f.contains(banned)),
            "a {banned:?} field entered the connection record"
        );
    }
}
