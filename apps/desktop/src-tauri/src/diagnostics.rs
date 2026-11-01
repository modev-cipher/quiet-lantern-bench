pub const MAX_DIAGNOSTIC_LEN: usize = 400;

pub fn diagnostic_line(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_control())
        .take(MAX_DIAGNOSTIC_LEN)
        .collect()
}

/// Frame sites reported per crash. The total frame count is always reported, so
/// a cap bounds the log line without hiding that the stack was deeper.
const MAX_CRASH_FRAME_SITES: usize = 8;

/// Salt for the crash signature. `RandomState`'s keys come from the OS CSPRNG
/// once per process, so the signature correlates repeated crashes inside one
/// session and is not a stable offline function of the text: a key pasted into
/// a webview cannot be turned into a precomputable digest of itself.
fn crash_salt() -> &'static std::hash::RandomState {
    static SALT: std::sync::OnceLock<std::hash::RandomState> = std::sync::OnceLock::new();
    SALT.get_or_init(std::hash::RandomState::new)
}

fn crash_signature(raw: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = crash_salt().build_hasher();
    hasher.write(raw.as_bytes());
    let full = format!("{:016x}", hasher.finish());
    full[..8].to_string()
}

/// Parse the `at <fn> (<source>:LINE:COL)` frame shape. The source may itself
/// contain colons (`app://assets/Main.js`, `file:///C:/…`), so line and column
/// come off the right. Anything that does not match contributes nothing.
fn crash_frame_site(frame: &str) -> Option<(u32, u32)> {
    let body = frame.strip_prefix("at ")?;
    let open = body.rfind('(')?;
    let close = body.rfind(')')?;
    let source = body.get(open + 1..close)?;
    if source.is_empty() {
        return None;
    }
    let mut parts = source.rsplitn(3, ':');
    let column: u32 = parts.next()?.parse().ok()?;
    let line: u32 = parts.next()?.parse().ok()?;
    if parts.next()?.is_empty() {
        return None;
    }
    Some((line, column))
}

/// Reduce a webview crash report to validated structural facts. Nothing from the
/// report survives but integers this module parsed itself and a per-process
/// digest: the webview is where BYOK keys live in plaintext while a user types
/// them, and this text lands in the log file users are told to email.
pub fn ui_crash_digest(line: &str) -> String {
    let mut frames = 0usize;
    let mut sites: Vec<(u32, u32)> = Vec::new();
    for raw in line.lines() {
        let frame = raw.trim();
        if !frame.starts_with("at ") {
            continue;
        }
        frames += 1;
        if sites.len() < MAX_CRASH_FRAME_SITES {
            sites.extend(crash_frame_site(frame));
        }
    }
    let mut digest = format!("frames={frames}");
    if !sites.is_empty() {
        let lines: Vec<String> = sites.iter().map(|(line, _)| line.to_string()).collect();
        let columns: Vec<String> = sites.iter().map(|(_, column)| column.to_string()).collect();
        digest.push_str(&format!(
            " lines={} cols={}",
            lines.join(","),
            columns.join(",")
        ));
    }
    digest.push_str(&format!(" sig={}", crash_signature(line)));
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_never_reach_the_log() {
        assert_eq!(
            diagnostic_line("tickHz=59.9\nresync=1\r\u{0}\u{1b}[2J"),
            "tickHz=59.9resync=1[2J",
            "a webview-supplied string that carries newlines or escapes could forge log lines or \
             move the terminal cursor"
        );
    }

    #[test]
    fn the_cap_applies_after_filtering_so_padding_cannot_push_the_payload_out() {
        let padded = format!("{}{}", "\n".repeat(500), "tickHz=59.9");
        assert_eq!(
            diagnostic_line(&padded),
            "tickHz=59.9",
            "capping before filtering would let 400 newlines hide the whole sample"
        );
    }

    #[test]
    fn a_long_line_is_capped_at_the_declared_length() {
        let line = "x".repeat(MAX_DIAGNOSTIC_LEN + 50);
        assert_eq!(diagnostic_line(&line).chars().count(), MAX_DIAGNOSTIC_LEN);
    }

    #[test]
    fn the_cap_counts_characters_rather_than_bytes() {
        let line = "é".repeat(MAX_DIAGNOSTIC_LEN + 10);
        let out = diagnostic_line(&line);
        assert_eq!(out.chars().count(), MAX_DIAGNOSTIC_LEN);
        assert!(
            out.ends_with('é'),
            "slicing by bytes would split a multi-byte character and panic"
        );
    }

    #[test]
    fn an_empty_line_stays_empty() {
        assert_eq!(diagnostic_line(""), "");
    }

    const KEY: &str = "sk_9f4c1e7a2b8d6035ae91c7f4d2b8e6a5";

    fn key_runs_absent(digest: &str, payload: &str) -> bool {
        let chars: Vec<char> = payload.chars().collect();
        (20..=chars.len()).all(|width| {
            (0..=chars.len() - width).all(|start| {
                let run: String = chars[start..start + width].iter().collect();
                !digest.contains(&run)
            })
        })
    }

    fn assert_well_formed(digest: &str) {
        let signature = digest
            .rsplit_once(" sig=")
            .unwrap_or_else(|| panic!("no signature in {digest:?}"))
            .1;
        assert_eq!(signature.len(), 8, "sig must be 8 hex chars: {digest:?}");
        assert!(
            signature.chars().all(|c| c.is_ascii_hexdigit()),
            "sig must be hex: {digest:?}"
        );
        assert!(
            !digest.chars().any(|c| c.is_control()),
            "webview control characters must not reach the log: {digest:?}"
        );
        assert_eq!(digest.lines().count(), 1, "must be one line: {digest:?}");
    }

    #[test]
    fn a_key_shaped_message_never_reaches_the_crash_digest() {
        let line = format!("TypeError: failed to authenticate with {KEY}");
        let digest = ui_crash_digest(&line);
        assert!(!digest.contains(KEY), "{digest}");
        assert!(key_runs_absent(&digest, KEY), "{digest}");
    }

    #[test]
    fn a_key_shaped_frame_source_never_reaches_the_crash_digest() {
        let line = format!(
            "Error: boom\n    at saveKey (app://assets/Main-{KEY}.js:41:9)\n    at Array.map (<anonymous>:1:1)"
        );
        let digest = ui_crash_digest(&line);
        assert!(!digest.contains(KEY), "{digest}");
        assert!(key_runs_absent(&digest, KEY), "{digest}");
        assert!(
            digest.starts_with("frames=2 lines=41,1 cols=9,1 "),
            "the frame structure is what the digest is for, got {digest}"
        );
    }

    #[test]
    fn a_ui_crash_never_forges_a_log_line() {
        let digest = ui_crash_digest(
            "TypeError: x is not a function\n    at Player (<anonymous>:1:1)\r\n\u{1b}[2JERROR fake level",
        );
        assert!(
            digest.starts_with("frames=1 lines=1 cols=1 "),
            "an Error.stack is webview text; only structure is reportable: {digest:?}"
        );
        assert_well_formed(&digest);
    }

    #[test]
    fn the_same_crash_repeating_correlates() {
        let line = "TypeError: x is not a function\n    at Player (app://assets/Main.js:41:9)";
        assert_eq!(ui_crash_digest(line), ui_crash_digest(line));
    }

    #[test]
    fn two_different_crashes_do_not_share_a_signature() {
        let first = ui_crash_digest("TypeError: x is not a function\n    at Player (a.js:41:9)");
        let second = ui_crash_digest("ReferenceError: y is not defined\n    at Player (a.js:41:9)");
        assert_ne!(first, second);
    }

    #[test]
    fn a_non_ascii_crash_report_digests_without_panicking() {
        let digest = ui_crash_digest("TypeError: 💥 غير معروف\n    at اللاعب (الملف.js:12:5)");
        assert!(
            digest.starts_with("frames=1 lines=12 cols=5 "),
            "got {digest:?}"
        );
        assert_well_formed(&digest);
    }

    #[test]
    fn an_empty_crash_report_digests_without_panicking() {
        let digest = ui_crash_digest("");
        assert!(digest.starts_with("frames=0 sig="), "got {digest:?}");
        assert_well_formed(&digest);
    }

    #[test]
    fn a_frame_that_is_not_the_conventional_shape_contributes_no_site() {
        let digest = ui_crash_digest(
            "    at Player\n    at f (a.js:99999999999999999999:1)\n    at g (a.js:x:y)",
        );
        assert!(
            digest.starts_with("frames=3 sig="),
            "a line or column this module cannot parse contributes nothing rather than text: {digest:?}"
        );
        assert_well_formed(&digest);
    }

    #[test]
    fn a_deep_stack_reports_its_whole_frame_count_with_a_bounded_site_list() {
        let stack: String = (1..=200)
            .map(|line| format!("    at f{line} (app://assets/Main.js:{line}:1)\n"))
            .collect();
        let digest = ui_crash_digest(&stack);
        assert!(
            digest.starts_with("frames=200 lines=1,2,3,4,5,6,7,8 "),
            "the true frame count is reported even when the sites are capped: {digest:?}"
        );
        assert!(
            digest.len() < 200,
            "a webview must not be able to inflate the log line, got {} bytes",
            digest.len()
        );
        assert_well_formed(&digest);
    }

    #[test]
    fn the_crash_digest_carries_no_payload_bytes() {
        let stack = format!(
            "TypeError: cannot read properties of undefined\n    at saveVoiceKey \
             (app://assets/KeyEntry-{KEY}.js:88:17)"
        );
        let digest = ui_crash_digest(&stack);
        assert!(!digest.contains(KEY), "key bytes in the digest: {digest}");
        assert!(key_runs_absent(&digest, KEY), "a key run in: {digest}");
        assert!(
            key_runs_absent(&digest, "cannot read properties of undefined"),
            "message text in the digest: {digest}"
        );
    }
}
