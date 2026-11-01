//! Process-wide tracing setup: one subscriber, installed once at launch.
//!
//! The dev terminal is the primary consumer, so the format is built for a
//! human scanning between events: local `HH:MM:SS`, a colored fixed-width
//! level, and the *last* module segment as target — `synthesis:` rather than
//! `txtcaster_lib::orchestrator::synthesis:`. `RUST_LOG` overrides everything;
//! when unset, [`DEFAULT_DIRECTIVES`] keeps app crates at `info` and demotes
//! dependencies that narrate their own internals, so silence means nothing
//! happened rather than the signal scrolled away.
//!
//! There is deliberately no file/JSON layer yet: none existed before this
//! module, and inventing one is a separate decision (rotation, location,
//! retention) — see `docs/LOGGING.md`.

use std::fmt;

use time::{OffsetDateTime, UtcOffset};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// The filter used when `RUST_LOG` is unset.
///
/// App events pass at `info`; the listed dependencies are demoted to `warn`
/// because their `info`/`debug` output describes protocol and I/O internals
/// (connection pools, TLS handshakes, SQL statements, window-system chatter)
/// that drowns the app's own events. Anything they say at `warn`/`error`
/// still comes through. No `h2` directive: absent from Cargo.lock.
const DEFAULT_DIRECTIVES: &str = "info,\
    cpal=warn,\
    hyper=warn,\
    hyper_util=warn,\
    keyring=warn,\
    reqwest=warn,\
    rustls=warn,\
    sqlx=warn,\
    tao=warn,\
    tokio_tungstenite=warn,\
    tower=warn,\
    tower_http=warn,\
    tungstenite=warn,\
    wry=warn,\
    zbus=warn";

/// Install the global subscriber. Call once, first thing in `run()`, while
/// the process is still single-threaded: the local UTC offset can only be
/// read soundly before threads exist, and falls back to UTC otherwise.
///
/// A second call (or a subscriber installed by a test harness) is a no-op
/// rather than a panic.
pub fn init() {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let preset = if std::env::var("RUST_LOG").is_ok() {
        None
    } else {
        txtcaster_debug_value().and_then(|value| debug_preset(&value))
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(preset.as_deref().unwrap_or(DEFAULT_DIRECTIVES)));
    let file_layer = preset.as_ref().and_then(|_| {
        open_session_log_file().map(|file| {
            tracing_subscriber::fmt::layer()
                .event_format(DevFormat { offset })
                .with_ansi(false)
                .with_writer(SessionLogWriter(std::sync::Arc::new(file)))
        })
    });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().event_format(DevFormat { offset }))
        .with(file_layer)
        .try_init();
}

/// The `TXTCASTER_DEBUG` knob (docs/LOGGING.md): a dev-friendly preset that
/// needs no `EnvFilter` syntax. `true`/`1`/`voice` turn the voice narrative
/// on (session + planes + adapter at `debug`); `voice-wire` adds the
/// per-frame `trace` depth. Anything else — `false`, `0`, empty — is off.
/// `RUST_LOG` set in the real environment still overrides everything.
fn debug_preset(value: &str) -> Option<String> {
    let voice = format!(
        "{DEFAULT_DIRECTIVES},txtcaster_lib::voice=debug,txtcaster_lib::voice_session=debug"
    );
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "voice" => Some(voice),
        "voice-wire" => Some(format!("{voice},txtcaster_lib::voice::channel=trace")),
        _ => None,
    }
}

/// `TXTCASTER_DEBUG` from the real environment, falling back to a `.env`
/// file in the working directory (the dev-server root) so the owner's
/// workflow is exactly "put TXTCASTER_DEBUG=true in .env, run, send the
/// file". A set env var wins over the file.
fn txtcaster_debug_value() -> Option<String> {
    if let Ok(value) = std::env::var("TXTCASTER_DEBUG") {
        return Some(value);
    }
    env_file_value(std::path::Path::new(".env"), "TXTCASTER_DEBUG")
}

fn env_file_value(path: &std::path::Path, key: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            return Some(
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }
    }
    None
}

/// How many session log files the rotation keeps.
const SESSION_LOG_KEEP: usize = 10;

/// Create `logs/voice-session-<timestamp>.log` under `TXTCASTER_LOG_DIR`
/// (default `./logs`, the dev-server root), pruning to the newest
/// [`SESSION_LOG_KEEP`] files. Every failure degrades to no file sink —
/// the terminal layer still runs.
fn open_session_log_file() -> Option<std::fs::File> {
    let dir = std::env::var("TXTCASTER_LOG_DIR").unwrap_or_else(|_| "logs".to_string());
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).ok()?;
    prune_session_logs(&dir, SESSION_LOG_KEEP - 1);
    let now = OffsetDateTime::now_utc();
    let name = format!(
        "voice-session-{:04}{:02}{:02}-{:02}{:02}{:02}-{}.log",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        std::process::id()
    );
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))
        .ok()
}

fn prune_session_logs(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<std::path::PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("voice-session-") && name.ends_with(".log"))
        })
        .collect();
    logs.sort();
    while logs.len() > keep {
        let oldest = logs.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
}

/// The file-sink writer: a shared handle to the session log file. `&File`
/// implements `Write`, so each `make_writer` hands out a cheap clone.
#[derive(Clone)]
struct SessionLogWriter(std::sync::Arc<std::fs::File>);

impl std::io::Write for SessionLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SessionLogWriter {
    type Writer = SessionLogWriter;
    fn make_writer(&'a self) -> SessionLogWriter {
        self.clone()
    }
}

/// `HH:MM:SS LEVEL target: message fields` — compact enough that one event is
/// one line, structured enough that levels align into a scannable column.
struct DevFormat {
    offset: UtcOffset,
}

impl<S, N> FormatEvent<S, N> for DevFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let now = OffsetDateTime::now_utc().to_offset(self.offset);
        write!(
            writer,
            "{:02}:{:02}:{:02} ",
            now.hour(),
            now.minute(),
            now.second()
        )?;
        let meta = event.metadata();
        write_level(&mut writer, *meta.level())?;
        write!(writer, " {}: ", short_target(meta.target()))?;
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// The last `::` segment: enough to say *which* module spoke without the
/// crate-path preamble repeating on every line.
fn short_target(target: &str) -> &str {
    target.rsplit("::").next().unwrap_or(target)
}

fn write_level(writer: &mut Writer<'_>, level: Level) -> fmt::Result {
    let text = match level {
        Level::ERROR => "ERROR",
        Level::WARN => " WARN",
        Level::INFO => " INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    };
    if !writer.has_ansi_escapes() {
        return write!(writer, "{text}");
    }
    let color = match level {
        Level::ERROR => "\x1b[31m",
        Level::WARN => "\x1b[33m",
        Level::INFO => "\x1b[32m",
        Level::DEBUG => "\x1b[34m",
        Level::TRACE => "\x1b[35m",
    };
    write!(writer, "{color}{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("capture lock").clone())
                .expect("captured log output is utf-8")
        }
    }

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    /// Run `emit` under the real dev subscriber (formatter included) and
    /// return exactly what would have reached the terminal.
    fn captured(emit: impl FnOnce()) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .event_format(DevFormat {
                    offset: UtcOffset::UTC,
                })
                .with_ansi(false)
                .with_writer(capture.clone()),
        );
        tracing::subscriber::with_default(subscriber, emit);
        capture.text()
    }

    #[test]
    fn the_default_directives_parse() {
        EnvFilter::try_new(DEFAULT_DIRECTIVES)
            .expect("DEFAULT_DIRECTIVES must stay a valid RUST_LOG expression");
    }

    #[test]
    fn the_debug_presets_parse_and_map_every_documented_value() {
        for on in ["true", "1", "voice", " TRUE "] {
            let preset = debug_preset(on).expect("a documented on-value maps to a preset");
            EnvFilter::try_new(&preset).expect("the voice preset must stay a valid filter");
            assert!(preset.contains("txtcaster_lib::voice=debug"));
            assert!(preset.contains("txtcaster_lib::voice_session=debug"));
            assert!(!preset.contains("trace"));
        }
        let wire = debug_preset("voice-wire").expect("voice-wire maps to a preset");
        EnvFilter::try_new(&wire).expect("the wire preset must stay a valid filter");
        assert!(wire.contains("txtcaster_lib::voice::channel=trace"));
        for off in ["false", "0", "", "banana"] {
            assert_eq!(debug_preset(off), None, "{off:?} must not enable anything");
        }
    }

    #[test]
    fn the_env_file_parser_reads_the_flag_and_nothing_fancier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "# comment\nOTHER=x\nTXTCASTER_DEBUG=\"voice\"\n").unwrap();
        assert_eq!(
            env_file_value(&path, "TXTCASTER_DEBUG"),
            Some("voice".to_string())
        );
        assert_eq!(env_file_value(&path, "MISSING"), None);
        assert_eq!(
            env_file_value(dir.path().join("absent").as_path(), "X"),
            None
        );
    }

    #[test]
    fn session_log_rotation_keeps_the_newest_files() {
        let dir = tempfile::tempdir().unwrap();
        for at in 0..12 {
            std::fs::write(
                dir.path()
                    .join(format!("voice-session-2026010{at:02}-000000.log")),
                "x",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("unrelated.txt"), "keep").unwrap();
        prune_session_logs(dir.path(), 9);
        let mut logs: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("voice-session-"))
            .collect();
        logs.sort();
        assert_eq!(logs.len(), 9, "prune keeps exactly the newest nine");
        assert!(
            logs[0].contains("202601003"),
            "oldest survivors start at index 3, got {}",
            logs[0]
        );
        assert!(dir.path().join("unrelated.txt").exists());
    }

    #[test]
    fn a_line_is_time_level_short_target_message() {
        let line = captured(|| tracing::info!(pages = 3, "render finished"));
        let line = line.trim_end();
        let (clock, rest) = line.split_at(9);
        assert!(
            clock.as_bytes()[2] == b':' && clock.as_bytes()[5] == b':',
            "line must open with HH:MM:SS, got {line:?}"
        );
        assert_eq!(
            rest, " INFO tests: render finished pages=3",
            "full line was {line:?}"
        );
    }

    #[test]
    fn the_target_is_the_last_module_segment() {
        assert_eq!(
            short_target("txtcaster_lib::orchestrator::synthesis"),
            "synthesis"
        );
        assert_eq!(short_target("ui"), "ui");
    }

    #[test]
    fn an_explicit_target_survives_unshortened() {
        let line = captured(|| tracing::debug!(target: "ui", "layout probe"));
        assert!(
            line.contains("DEBUG ui: layout probe"),
            "explicit targets name the subsystem directly, got {line:?}"
        );
    }

    /// The no-keys-in-logs rule is enforced upstream by source-scan hygiene
    /// tests (`voice::keys::hygiene`) and by `ApiKey`'s redacting `Debug`.
    /// This pins the last link: even if a key-holding struct *is* handed to a
    /// logging macro, the line that reaches the terminal carries no key bytes.
    #[test]
    fn a_captured_log_line_never_contains_key_bytes() {
        const KEY: &str = "sk_tlog_supersecret_77zq";

        #[derive(Debug)]
        #[allow(dead_code)]
        struct ProbeFailed {
            provider: &'static str,
            key: crate::voice::keys::ApiKey,
        }

        let line = captured(|| {
            let failure = ProbeFailed {
                provider: "cartesia",
                key: crate::voice::keys::ApiKey::new(KEY.into()),
            };
            tracing::error!(?failure, "voice key probe failed");
        });
        assert!(
            !line.contains("supersecret") && !line.contains("77zq"),
            "key bytes reached a formatted log line: {line:?}"
        );
        assert!(
            line.contains("redacted"),
            "the redacting Debug impl should be what the formatter saw: {line:?}"
        );
    }

    /// The webview sends `Error.stack ?? Error.message` here, and the webview is
    /// where a BYOK key sits in plaintext while it is being typed. This pins the
    /// channel itself rather than the hygiene of a type: whatever the formatter
    /// does, the bytes that arrive here are already structural.
    #[test]
    fn the_ui_crash_channel_never_carries_webview_content() {
        const KEY: &str = "sk_9f4c1e7a2b8d6035ae91c7f4d2b8e6a5";
        let line = captured(|| {
            crate::log_ui_error(format!(
                "TypeError: cannot read properties of undefined\n    at saveVoiceKey \
                 (app://assets/KeyEntry-{KEY}.js:88:17)"
            ));
        });
        assert!(line.contains("ui crashed"), "{line:?}");
        assert!(
            line.contains("frames=1 lines=88 cols=17 sig="),
            "the reported structure is what identifies the crash: {line:?}"
        );
        assert!(!line.contains(KEY), "key bytes reached the log: {line:?}");
        for width in 20..=KEY.len() {
            let run = &KEY[KEY.len() - width..];
            assert!(
                !line.contains(run),
                "key run {run:?} reached the log: {line:?}"
            );
        }
        assert!(
            !line.contains("cannot read properties"),
            "message text reached the log: {line:?}"
        );
        assert!(
            !line.contains("KeyEntry"),
            "the asset path is webview text too: {line:?}"
        );
    }
}
