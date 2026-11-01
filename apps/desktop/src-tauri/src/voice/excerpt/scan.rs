//! Source-level enforcement of the single-egress-point rule (VOICE_AGENT §4:
//! "`ExcerptSelector` is the only code path allowed to read book text into a
//! channel. Unit tests assert no other voice module touches page text.").
//!
//! Same discipline as `voice::keys::hygiene` and the `SCAN_ANCHORS` pattern in
//! `tests/mock_http_contract.rs`: the walk is path-anchored to files that must
//! exist, so a rename/move of the guarded code fails this scan loudly instead
//! of silently narrowing it to nothing.

#![cfg(test)]

use std::path::{Path, PathBuf};

/// The one directory allowed to name the page-text access surface.
const EGRESS_DIR: &str = "src/voice/excerpt";

/// Files the walk must see. A move of any of these reds the anchor test, which
/// is the loud signal to re-point the scan rather than let it go vacuous.
const SCAN_ANCHORS: &[&str] = &[
    "src/voice/excerpt/selector.rs",
    "src/voice/excerpt/source.rs",
    "src/voice/audio/jitter.rs",
    "src/voice/channel/types.rs",
    "src/voice/keys/manager.rs",
];

/// The page-text access surface: every symbol that yields book text. Naming
/// any of these outside `src/voice/excerpt` is how a second egress path would
/// start, so it fails here at its first mention, not at its first leak.
///
/// The list is layered on purpose: the module paths below catch reaching into
/// the text-bearing modules however the import is spelled, and these terminal
/// symbols catch the access itself when the module path is laundered through
/// an alias (`use crate::storage as st; st::book::list_pages(...)`).
/// Deliberately broad — `Page` also matches `PageGeometry` and friends —
/// because a loud false positive is reviewable and a quiet bypass is not.
///
/// **What these needles do NOT catch:** raw SQL. A query names a column, and
/// the primary narration body is `script_segment.text` — a bare `text` can
/// never be a needle. Only two guarded columns are distinctive enough to
/// appear here (`text_json`, `words_json`); every other raw-SQL route is
/// closed structurally by the database-boundary rule and the SQL table-name
/// net further down, whose own limits are stated at their definitions.
const PAGE_TEXT_SURFACE: &[&str] = &[
    // The raw page-text column and every storage::book route to it.
    "text_json",
    "Page",
    "list_pages",
    "get_page(",
    "insert_page",
    "set_page_text",
    "replace_blocks",
    "insert_book_with_contents",
    "page_text",
    // Script/segment text (storage::job) and its editors.
    "ScriptSegment",
    "list_segments",
    "segment_text",
    "original_text",
    // Word-level text (alignment words_json carries the spoken words).
    "words_json",
    "list_alignments",
    "get_alignment",
    // The reading structures that already join all of the above.
    "ReadingSegment",
    "ReadingChapter",
];

/// Text-bearing modules, matched **independent of import shape**: a plain
/// path, a grouped import (`use crate::storage::{book as _bk};`), a nested
/// group, or a multi-line group all count. A plain substring needle here was
/// the hole an independent validator drove a compiling bypass through.
const PAGE_TEXT_MODULES: &[(&str, &str)] = &[
    ("crate::", "reading"),
    ("storage::", "book"),
    ("storage::", "job"),
];

/// The database-access surface. SQL cannot execute without a connection, and
/// every connection path in this crate spells one of these tokens (sqlx is the
/// only database dependency; `SqlitePool` is its pool type, re-exported by
/// `crate::storage`; `Executor` is the trait a pool could hide behind as a
/// generic bound). So the raw-SQL egress layer is closed structurally: no
/// voice module outside the name-anchored whitelist below may mention any of
/// them — including the excerpt tree itself, which receives its snapshot as
/// structs and runs no SQL.
const DB_ACCESS_SURFACE: &[&str] = &["sqlx", "SqlitePool", "Executor"];

/// The only voice files allowed to name database tokens, each with the reason
/// it needs them (mock_http_contract's exemption discipline). Kept tight: an
/// entry that stops naming a db token fails the integrity test and must be
/// removed in the same diff.
const DB_WHITELIST: &[(&str, &str)] = &[
    (
        "src/voice/keys/manager.rs",
        "key metadata index + URL consent live in app_setting via storage::settings; \
         voice::keys::hygiene pins this file away from raw key bytes and the SQL table-name \
         net below pins it away from text-bearing tables",
    ),
    (
        "src/voice/keys/hygiene.rs",
        "cfg(test)-only guard file that spells sqlx/SqlitePool as its own forbidden-needle \
         string literals; it runs no queries",
    ),
];

/// Tables whose rows carry book-derived text — swept from the full schema
/// (migrations 0001–0017), not just the obvious three: `page` (text_json),
/// `script_segment` (text, original_text — the day-one narration body),
/// `alignment` (words_json), `figure_region` (caption, explanation),
/// `chapter` (title), `book` (title, author), `page_geometry` (geometry_json
/// embeds per-word `token` strings). The other nine tables (voice_profile,
/// render_job, model_install, hw_benchmark, cache_entry, app_setting, export,
/// cache_ref, chapter_render) carry paths, hashes, settings and job metadata,
/// not book text.
const TEXT_BEARING_TABLES: &[&str] = &[
    "page",
    "script_segment",
    "alignment",
    "figure_region",
    "chapter",
    "book",
    "page_geometry",
];

/// SQL positions a table name can occupy, matched on the whitespace-stripped,
/// uppercased view (SQLite keywords and identifiers are case-insensitive).
/// Applied to the db-whitelisted files — everyone else cannot run SQL at all
/// per the boundary rule, which is the load-bearing layer. **Honest limits:**
/// a quoted/bracketed identifier (`FROM "page"`) or a string-assembled table
/// name evades this net; it is a second tripwire for the whitelisted files,
/// not the wall.
const SQL_TABLE_POSITIONS: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];

fn names_table_in_sql_position(flat_upper: &str, table: &str) -> bool {
    let table_upper = table.to_uppercase();
    SQL_TABLE_POSITIONS
        .iter()
        .any(|position| flat_upper.contains(&format!("{position}{table_upper}")))
}

/// Whitespace-free view: formatting (line breaks in grouped imports, spaces
/// before parens) must not be able to hide a needle from a substring match.
fn normalized(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Does `normalized` reach `module` under `parent`? Handles the direct path
/// (`storage::book`) and brace groups after the parent (`storage::{settings,
/// book as _bk}`), including nested groups, by scanning the group's top-level
/// items. Aliased parents are the terminal symbols' job (see above).
fn reaches_module(normalized: &str, parent: &str, module: &str) -> bool {
    let mut rest = normalized;
    while let Some(at) = rest.find(parent) {
        let after = &rest[at + parent.len()..];
        if after.starts_with(module) {
            return true;
        }
        if let Some(group) = after.strip_prefix('{') {
            if group_names_module(group, module) {
                return true;
            }
        }
        rest = after;
    }
    false
}

/// `group` starts just inside a `{`. An item begins at the group's top level
/// after `{` or a top-level `,`; the module counts only as an item prefix, so
/// `storage::{bookkeeping}` matching `book` is accepted overmatch, while
/// `crate::{storage::{book}, reading}`'s second item is still found.
fn group_names_module(group: &str, module: &str) -> bool {
    let mut depth = 1u32;
    let mut at_item_start = true;
    for (at, c) in group.char_indices() {
        match c {
            '{' => {
                depth += 1;
                at_item_start = false;
            }
            '}' => {
                if depth == 1 {
                    return false;
                }
                depth -= 1;
                at_item_start = false;
            }
            ',' => {
                if depth == 1 {
                    at_item_start = true;
                }
            }
            _ => {
                if at_item_start && depth == 1 {
                    if group[at..].starts_with(module) {
                        return true;
                    }
                    at_item_start = false;
                }
            }
        }
    }
    false
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn voice_sources() -> Vec<(String, String)> {
    let root = crate_root();
    let mut files = Vec::new();
    let mut stack = vec![root.join("src/voice")];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).expect("voice source dir must be readable") {
            let path = entry.expect("dir entry must be readable").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(&root)
                    .expect("under the crate root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).expect("source file must be readable");
                files.push((relative, text));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn is_self(relative: &str) -> bool {
    Path::new(relative)
        .file_name()
        .is_some_and(|f| f == "scan.rs")
}

#[test]
fn the_scan_sees_the_files_it_claims_to_pin() {
    let files = voice_sources();
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    for anchor in SCAN_ANCHORS {
        assert!(
            names.contains(anchor),
            "the walk missed {anchor}; a scan that reads no files passes every assertion \
             below it and pins nothing. Seen: {names:?}"
        );
    }
    let source = files
        .iter()
        .find(|(name, _)| name == "src/voice/excerpt/source.rs")
        .map(|(_, text)| text)
        .expect("anchored above");
    assert!(
        source.contains("crate::reading"),
        "the excerpt module itself must name the page-text surface it guards; if it stopped, \
         the forbidden-elsewhere list below would be scanning for a surface nobody uses"
    );
    assert!(
        reaches_module(&normalized(source), "crate::", "reading"),
        "the module matcher must see the same import the plain needle sees, or the \
         shape-independent layer is scanning with a broken parser"
    );
}

#[test]
fn no_voice_module_outside_the_excerpt_tree_names_the_page_text_surface() {
    for (name, text) in voice_sources() {
        if name.starts_with(EGRESS_DIR) {
            continue;
        }
        let flat = normalized(&text);
        for needle in PAGE_TEXT_SURFACE {
            assert!(
                !flat.contains(needle),
                "{name} mentions `{needle}`: ExcerptSelector is the single enforcement point \
                 for book text egress (VOICE_AGENT §4); a second voice module naming the \
                 page-text surface is a second egress path in the making. Route it through \
                 voice::excerpt instead"
            );
        }
        for (parent, module) in PAGE_TEXT_MODULES {
            assert!(
                !reaches_module(&flat, parent, module),
                "{name} reaches `{parent}{module}` (possibly through a grouped or aliased \
                 import): that module carries book text, and ExcerptSelector is the single \
                 enforcement point for book text egress (VOICE_AGENT §4). Route it through \
                 voice::excerpt instead"
            );
        }
    }
}

/// The matcher itself is pinned on string fixtures — including the exact
/// bypass an independent validator compiled against the substring version of
/// this scan (grouped import + alias + the real column name).
#[test]
fn the_module_matcher_sees_through_import_shapes() {
    let validator_bypass = normalized(
        "use crate::storage::{book as _bk};\n\
         fn rogue(pool: &P, id: &str) { let pages = _bk::list_pages(pool, id); }",
    );
    assert!(reaches_module(&validator_bypass, "storage::", "book"));
    assert!(
        validator_bypass.contains("list_pages"),
        "the terminal symbol catches the same bypass independently"
    );

    let multi_line_group = normalized("use crate::storage::{\n    settings,\n    book,\n};");
    assert!(reaches_module(&multi_line_group, "storage::", "book"));

    let nested_group = normalized("use crate::{storage::{job}, reading};");
    assert!(reaches_module(&nested_group, "storage::", "job"));
    assert!(reaches_module(&nested_group, "crate::", "reading"));

    let plain = normalized("use crate::storage::book::Page;");
    assert!(reaches_module(&plain, "storage::", "book"));

    let innocent = normalized(
        "use crate::storage::{settings, now_ms};\n\
         use crate::voice::channel::types::DelimitedExcerpt;",
    );
    assert!(!reaches_module(&innocent, "storage::", "book"));
    assert!(!reaches_module(&innocent, "storage::", "job"));
    assert!(!reaches_module(&innocent, "crate::", "reading"));
}

#[test]
fn no_voice_module_outside_the_db_whitelist_can_reach_sqlite() {
    let whitelisted: Vec<&str> = DB_WHITELIST.iter().map(|(path, _)| *path).collect();
    for (name, text) in voice_sources() {
        if whitelisted.contains(&name.as_str()) || is_self(&name) {
            continue;
        }
        let flat = normalized(&text);
        for needle in DB_ACCESS_SURFACE {
            assert!(
                !flat.contains(needle),
                "{name} mentions `{needle}`: raw SQL is a page-text egress path no needle \
                 list can see into (the narration body column is a bare `text`), so voice \
                 modules simply do not get database access — book text enters this tree \
                 through voice::excerpt::source structs only. If this file genuinely needs \
                 the database, whitelist it in DB_WHITELIST with a reason, in the same diff \
                 that adds the need"
            );
        }
    }
}

#[test]
fn a_db_whitelisted_file_never_names_a_text_bearing_table_in_sql_position() {
    let files = voice_sources();
    for (path, _) in DB_WHITELIST {
        let text = files
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, text)| text)
            .unwrap_or_else(|| panic!("whitelist names {path}, which the walk did not see"));
        let flat_upper = normalized(text).to_uppercase();
        for table in TEXT_BEARING_TABLES {
            assert!(
                !names_table_in_sql_position(&flat_upper, table),
                "{path} names text-bearing table `{table}` in SQL position: the db whitelist \
                 licenses settings/metadata access, never book text — that is the excerpt \
                 module's monopoly (VOICE_AGENT §4)"
            );
        }
    }
}

#[test]
fn the_db_whitelist_names_real_files_that_still_need_it() {
    let files = voice_sources();
    for (path, reason) in DB_WHITELIST {
        assert!(
            reason.len() > 20,
            "whitelist entry {path} must record why it needs database access, not just that \
             it is excused"
        );
        let text = files
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, text)| text)
            .unwrap_or_else(|| {
                panic!(
                    "whitelist entry {path} names no file the walk can see; \
                 a rotted entry silently widens the boundary"
                )
            });
        let flat = normalized(text);
        assert!(
            DB_ACCESS_SURFACE.iter().any(|needle| flat.contains(needle)),
            "{path} no longer names any database token — remove its whitelist entry in the \
             same diff, or the exemption outlives its reason"
        );
    }
}

/// The raw-SQL layer pinned on the exact quiet bypass an independent validator
/// wrote against the needle-only version of this scan: `script_segment.text`
/// read via `query_scalar`, no guarded module or symbol named.
#[test]
fn the_sql_layers_see_the_validator_bypass() {
    let bypass = normalized(
        "let rows: Vec<String> = sqlx::query_scalar(\
         \"SELECT text FROM script_segment WHERE job_id = ? ORDER BY idx\")\
         .bind(job_id).fetch_all(pool).await.expect(\"rows\");",
    );
    assert!(
        DB_ACCESS_SURFACE
            .iter()
            .any(|needle| bypass.contains(needle)),
        "the boundary rule catches the executor"
    );
    assert!(
        names_table_in_sql_position(&bypass.to_uppercase(), "script_segment"),
        "the table net catches the query text independently"
    );

    let innocent_settings_sql =
        normalized("SELECT value FROM app_setting WHERE key = ?").to_uppercase();
    for table in TEXT_BEARING_TABLES {
        assert!(
            !names_table_in_sql_position(&innocent_settings_sql, table),
            "settings SQL must stay green or the whitelist's licensed use trips its own net"
        );
    }
}

#[test]
fn the_excerpt_module_contains_no_logging_at_all() {
    for (name, text) in voice_sources() {
        if !name.starts_with(EGRESS_DIR) || is_self(&name) {
            continue;
        }
        for needle in ["println!", "eprintln!", "dbg!", "log::", "tracing::"] {
            assert!(
                !text.contains(needle),
                "{name} contains `{needle}`: the sentinel is never logged and excerpts are \
                 book content — the rule is enforced by having no log statements in the \
                 module, so a redaction bug has nothing to miss (voice::keys precedent)"
            );
        }
    }
}

#[test]
fn the_sentinel_is_not_serializable() {
    let text = std::fs::read_to_string(crate_root().join("src/voice/excerpt/sentinel.rs"))
        .expect("sentinel.rs must be readable");
    assert!(
        !text.contains("serde"),
        "a serde impl on the sentinel would let a derived view struct carry the boundary \
         token into SQLite rows, IPC responses, or exports (VOICE_AGENT §4: never persisted, \
         never exposed in UI or exports)"
    );
}
