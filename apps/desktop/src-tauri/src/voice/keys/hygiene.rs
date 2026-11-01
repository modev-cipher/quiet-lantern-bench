//! Source-level hygiene checks (VOICE_AGENT §7: keys never in SQLite, logs,
//! or exports). These are grep-style tests in the repo's established
//! `include_str!`-on-source style (see `orchestrator::state`): they fail the
//! build the moment key material gains a path into the persistence layer or a
//! logging macro appears near secrets, instead of waiting for a leak.

#![cfg(test)]

use std::path::Path;

fn rust_sources_under(dir: &Path) -> Vec<(String, String)> {
    let mut sources = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).expect("source dir must be readable") {
            let path = entry.expect("dir entry must be readable").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "hygiene.rs") {
                continue;
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("source file must be readable");
                sources.push((path.display().to_string(), text));
            }
        }
    }
    assert!(
        !sources.is_empty(),
        "walking {dir:?} found nothing — the guard would be vacuous"
    );
    sources
}

fn manifest_path(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

#[test]
fn the_persistence_layer_never_names_a_key_type() {
    for (file, text) in rust_sources_under(&manifest_path("src/storage")) {
        for needle in ["ApiKey", "api_key", "expose(", "voice::keys"] {
            assert!(
                !text.contains(needle),
                "{file} mentions `{needle}`: the SQLite layer must be structurally unable to \
                 see key material, not merely trusted not to write it"
            );
        }
    }
}

#[test]
fn the_keys_module_contains_no_logging_at_all() {
    for (file, text) in rust_sources_under(&manifest_path("src/voice/keys")) {
        for needle in ["println!", "eprintln!", "dbg!", "log::", "tracing::"] {
            assert!(
                !text.contains(needle),
                "{file} contains `{needle}`: the no-keys-in-logs rule is enforced by having \
                 no log statements near secrets, so a redaction bug has nothing to miss"
            );
        }
    }
}

#[test]
fn no_timer_interval_or_background_task_exists_anywhere_near_probes() {
    for (file, text) in rust_sources_under(&manifest_path("src/voice/keys")) {
        let production = text.split("#[cfg(test)]").next().unwrap_or(&text);
        for needle in [
            "tokio::spawn",
            "thread::spawn",
            "interval(",
            "sleep(",
            "tokio::time",
            "spawn_local",
        ] {
            assert!(
                !production.contains(needle),
                "{file} contains `{needle}` outside test code: VOICE_AGENT §7 allows probes \
                 on settings entry and explicit refresh only — the module must have no \
                 machinery capable of ambient polling, so the rule cannot rot into a \
                 code-review convention"
            );
        }
    }
}

#[test]
fn the_manager_never_touches_raw_key_bytes() {
    let text = std::fs::read_to_string(manifest_path("src/voice/keys/manager.rs"))
        .expect("manager.rs must be readable");
    assert!(
        !text.contains("expose("),
        "manager.rs is the only keys file with database imports; the moment it can also read \
         raw key bytes, one refactor away they meet"
    );
}

#[test]
fn the_probe_builder_cannot_attach_a_request_body() {
    let text = std::fs::read_to_string(manifest_path("src/voice/keys/probe.rs"))
        .expect("probe.rs must be readable");
    for needle in [".body(", ".json(&", ".form(", "Method::POST", "\"POST\""] {
        assert!(
            !text.contains(needle),
            "probe.rs contains `{needle}`: non-billable-by-shape means the probe layer is \
             structurally unable to send a synthesis/transcription/completion payload"
        );
    }
}

#[test]
fn secret_touching_files_cannot_reach_sqlite() {
    for name in ["secret.rs", "vault.rs", "store.rs", "probe.rs", "import.rs"] {
        let path = manifest_path("src/voice/keys").join(name);
        let text = std::fs::read_to_string(&path).expect("keys source must be readable");
        for needle in ["sqlx", "set_setting", "SqlitePool"] {
            assert!(
                !text.contains(needle),
                "{name} mentions `{needle}`: files that hold plaintext key bytes must have \
                 no database imports to misuse"
            );
        }
    }
}

#[test]
fn the_import_parser_cannot_compose_a_string_from_line_content() {
    let text = std::fs::read_to_string(manifest_path("src/voice/keys/import.rs"))
        .expect("import.rs must be readable");
    let production = text.split("#[cfg(test)]").next().unwrap_or(&text);
    for needle in ["format!", "to_string(", "String::from("] {
        assert!(
            !production.contains(needle),
            "import.rs production code contains `{needle}`: rejection reasons are fixed \
             &'static str by construction — the moment the parser can compose a string, \
             a rejected line's content is one refactor from riding into the receipt"
        );
    }
}

#[test]
fn the_import_parser_cannot_reach_process_streams_or_the_filesystem() {
    // Sabotage-proven gap (Role B): a raw `std::io::stderr().write_all(value
    // .as_bytes())` in the parser leaked key bytes past EVERY older gate —
    // no logging macro, no composed string, no sqlite import. The parser
    // needs no I/O of any kind (the file variant reads bytes in manager.rs),
    // so the whole surface is banned rather than enumerated.
    let text = std::fs::read_to_string(manifest_path("src/voice/keys/import.rs"))
        .expect("import.rs must be readable");
    let production = text.split("#[cfg(test)]").next().unwrap_or(&text);
    for needle in [
        "std::io",
        "std::fs",
        "stderr",
        "stdout",
        "write_all",
        "File::",
        "OpenOptions",
        "std::net",
        "reqwest",
    ] {
        assert!(
            !production.contains(needle),
            "import.rs production code mentions `{needle}`: the parser must be structurally \
             unable to move bytes anywhere but into ApiKey — it has no legitimate I/O"
        );
    }
}

#[test]
fn the_key_type_is_not_serializable() {
    let text = std::fs::read_to_string(manifest_path("src/voice/keys/secret.rs"))
        .expect("secret.rs must be readable");
    assert!(
        !text.contains("serde"),
        "a serde impl on ApiKey would let any derived view struct carry key bytes into \
         SQLite rows, IPC responses, or exports without a single suspicious line at the site"
    );
}
