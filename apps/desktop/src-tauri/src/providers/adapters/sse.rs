//! Incremental server-sent-events scanner for provider adapters.
//!
//! Implements the field-level rules of the WHATWG event-stream
//! interpretation algorithm (https://html.spec.whatwg.org/multipage/server-sent-events.html,
//! checked 2026-08-28) for the subset batch adapters consume: line
//! terminators are CRLF, LF, or CR; an event ends at a blank line; an
//! event's payload is all of its `data:` fields joined with newlines, with
//! one optional leading space stripped per field; comment lines (leading
//! `:`) and unknown fields (`event:`, `id:`, `retry:`, anything else) are
//! ignored at this layer — the JSON payloads carry their own `type`
//! discriminator.
//!
//! The scanner is INCREMENTAL and BOUNDED (charter A23, Hermes S-3 MF8):
//! bytes are pushed as they arrive off the wire, complete events pop out as
//! they close, and explicit per-event and event-count limits abort a
//! hostile stream (tiny audio plus unbounded timing metadata, an event that
//! never closes) instead of accumulating it.

use crate::providers::registry::{ScriptError, ScriptErrorKind};

/// A single event never legitimately exceeds this: the largest real payload
/// is one base64 audio chunk, and providers frame those in the tens of
/// kilobytes. 1 MiB is two orders of magnitude of slack.
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
/// One per-segment synthesis is a bounded burst of chunk + timestamp
/// events; thousands means a runaway or hostile stream.
pub const MAX_EVENTS: usize = 8_192;

fn limit_error(detail: String) -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Provider,
        retryable: false,
        message: detail,
        retry_after_secs: None,
        status: None,
    }
}

#[derive(Default)]
pub struct SseScanner {
    /// Unconsumed wire bytes (at most one incomplete line + one event).
    buffer: Vec<u8>,
    /// `data:` payload of the currently open event.
    data: String,
    events_emitted: usize,
}

impl SseScanner {
    pub fn new() -> SseScanner {
        SseScanner::default()
    }

    /// Push wire bytes; returns every event payload that COMPLETED within
    /// them (the joined `data:` fields of each blank-line-terminated event).
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ScriptError> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() + self.data.len() > MAX_EVENT_BYTES {
            return Err(limit_error(format!(
                "sse event exceeded the {MAX_EVENT_BYTES}-byte event bound mid-stream"
            )));
        }
        self.drain_lines()
    }

    fn drain_lines(&mut self) -> Result<Vec<String>, ScriptError> {
        let mut events = Vec::new();
        // One full line at a time, under any legal terminator. A trailing
        // CR is kept back until the next byte arrives, because it may be
        // the first half of a CRLF.
        while let Some((line_end, terminator_len)) = find_line_end(&self.buffer) {
            let line = String::from_utf8_lossy(&self.buffer[..line_end]).into_owned();
            self.buffer.drain(..line_end + terminator_len);
            if line.is_empty() {
                // Blank line: the open event (if it carried data) completes.
                if !self.data.is_empty() {
                    self.events_emitted += 1;
                    if self.events_emitted > MAX_EVENTS {
                        return Err(limit_error(format!(
                            "sse stream exceeded the {MAX_EVENTS}-event bound"
                        )));
                    }
                    events.push(std::mem::take(&mut self.data));
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keep-alive
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_str(), ""),
            };
            if field == "data" {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            // event:/id:/retry:/unknown fields: deliberately ignored here.
        }
        Ok(events)
    }

    /// End of stream. A lone CR held back as a possible half-CRLF is now a
    /// COMPLETE terminator (CR alone is legal), so it is resolved in place —
    /// nothing else gains a terminator it never had: a final line or event
    /// missing its blank-line terminator at EOF is NOT dispatched, per the
    /// spec ("once the end of the file is reached, any pending data must be
    /// discarded") — it is reported instead, so a truncated stream cannot
    /// smuggle a half event (or a done event) through as complete.
    pub fn finish(mut self) -> (Vec<String>, Option<String>) {
        let tail = if self.buffer.last() == Some(&b'\r') {
            // The held CR was a real CR terminator, not half a CRLF.
            *self.buffer.last_mut().expect("just checked") = b'\n';
            self.drain_lines().unwrap_or_default()
        } else {
            Vec::new()
        };
        let leftover = (!self.data.is_empty() || !self.buffer.is_empty()).then(|| {
            format!(
                "stream ended mid-event ({} buffered byte(s), {} pending data byte(s))",
                self.buffer.len(),
                self.data.len()
            )
        });
        (tail, leftover)
    }
}

fn find_line_end(buffer: &[u8]) -> Option<(usize, usize)> {
    for (idx, byte) in buffer.iter().enumerate() {
        match byte {
            b'\n' => return Some((idx, 1)),
            b'\r' => {
                return match buffer.get(idx + 1) {
                    Some(b'\n') => Some((idx, 2)),
                    Some(_) => Some((idx, 1)),
                    // A CR at the very end may be half a CRLF: wait.
                    None => None,
                };
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_events(chunks: &[&[u8]]) -> (Vec<String>, Option<String>) {
        let mut scanner = SseScanner::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(scanner.push(chunk).expect("within limits"));
        }
        let (tail, leftover) = scanner.finish();
        events.extend(tail);
        (events, leftover)
    }

    #[test]
    fn every_legal_line_terminator_frames_events() {
        for (label, body) in [
            ("lf", "data: one\n\ndata: two\n\n".to_string()),
            ("crlf", "data: one\r\n\r\ndata: two\r\n\r\n".to_string()),
            ("cr", "data: one\r\rdata: two\r\r".to_string()),
        ] {
            let (events, leftover) = all_events(&[body.as_bytes()]);
            assert_eq!(events, vec!["one", "two"], "{label}");
            assert!(leftover.is_none(), "{label}");
        }
    }

    #[test]
    fn multi_line_data_joins_and_comments_and_unknown_fields_are_ignored() {
        let body = ": keep-alive\nevent: chunk\nid: 7\nretry: 100\ndata: {\"a\":\ndata: 1}\n\n";
        let (events, leftover) = all_events(&[body.as_bytes()]);
        assert_eq!(events, vec!["{\"a\":\n1}"]);
        assert!(leftover.is_none());
    }

    #[test]
    fn events_split_across_wire_chunks_reassemble() {
        let (events, leftover) =
            all_events(&[b"da", b"ta: hel", b"lo\r", b"\n\r\nda", b"ta: x\n\n"]);
        assert_eq!(events, vec!["hello", "x"]);
        assert!(leftover.is_none());
    }

    #[test]
    fn a_final_event_missing_its_blank_line_is_truncated_not_dispatched() {
        // The final line IS terminated, but the event's blank line never
        // arrived: per spec the pending data is discarded and reported —
        // EOF must not fabricate the terminator (a done event smuggled this
        // way would let a truncated stream cache partial audio).
        let (events, leftover) = all_events(&[b"data: first\n\ndata: {\"type\":\"done\"}\n"]);
        assert_eq!(events, vec!["first"]);
        assert!(
            leftover.is_some(),
            "the unterminated event is reported, never dispatched"
        );
    }

    #[test]
    fn a_truncated_event_is_reported_not_dispatched() {
        let (events, leftover) = all_events(&[b"data: complete\n\ndata: trunca"]);
        assert_eq!(events, vec!["complete"]);
        assert!(leftover.is_some(), "the half event must not pass silently");
    }

    #[test]
    fn the_event_size_bound_aborts_mid_stream() {
        let mut scanner = SseScanner::new();
        let chunk = vec![b'a'; 64 * 1024];
        let mut pushed = 0usize;
        let error = loop {
            match scanner.push(&chunk) {
                Ok(_) => {
                    pushed += chunk.len();
                    assert!(
                        pushed <= MAX_EVENT_BYTES + chunk.len(),
                        "must abort at the bound"
                    );
                }
                Err(error) => break error,
            }
        };
        assert!(error.message.contains("event bound"));
    }

    #[test]
    fn the_event_count_bound_aborts_a_runaway_stream() {
        let mut scanner = SseScanner::new();
        let one = b"data: x\n\n";
        let error = loop {
            match scanner.push(one) {
                Ok(_) => {}
                Err(error) => break error,
            }
        };
        assert!(error.message.contains("event bound") || error.message.contains("-event bound"));
    }
}
