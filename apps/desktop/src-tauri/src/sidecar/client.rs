use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;

const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

#[derive(Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.find("\n\n") {
            let block = self.buffer[..pos].to_string();
            self.buffer.drain(..pos + 2);
            if let Some(event) = parse_block(&block) {
                events.push(event);
            }
        }
        events
    }
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data = None;
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data = serde_json::from_str(value.trim()).ok();
        }
    }
    Some(SseEvent {
        event: event?,
        data: data?,
    })
}

const REFUSAL_DETAIL_CHARS: usize = 500;
pub const REFUSAL_READ_TIMEOUT: Duration = Duration::from_secs(5);
pub const UNSTRUCTURED_CODE: &str = "ERR_UNSTRUCTURED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    Transport(String),
    Refused {
        status: u16,
        path: String,
        code: String,
        message: String,
    },
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Transport(message) => write!(f, "{message}"),
            SidecarError::Refused {
                status,
                path,
                code,
                message,
            } => write!(f, "sidecar returned {status} for {path}: {code}: {message}"),
        }
    }
}

fn transport(message: String) -> SidecarError {
    SidecarError::Transport(message)
}

fn truncate(value: &str) -> String {
    value.chars().take(REFUSAL_DETAIL_CHARS).collect()
}

pub fn parse_refusal_detail(body: &str) -> (String, String) {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("detail").cloned());
    if let Some(detail) = detail {
        if let Some(code) = detail.get("code").and_then(Value::as_str) {
            let message = detail
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(body);
            return (code.to_string(), truncate(message));
        }
    }
    (UNSTRUCTURED_CODE.to_string(), truncate(body))
}

async fn refusal(
    status: reqwest::StatusCode,
    path: &str,
    response: reqwest::Response,
) -> SidecarError {
    let body = tokio::time::timeout(REFUSAL_READ_TIMEOUT, response.text())
        .await
        .unwrap_or_else(|_| Ok(String::new()))
        .unwrap_or_default();
    let (code, message) = parse_refusal_detail(&body);
    SidecarError::Refused {
        status: status.as_u16(),
        path: path.to_string(),
        code,
        message,
    }
}

pub async fn post_sse<B, F>(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    path: &str,
    job_id: &str,
    body: &B,
    mut on_event: F,
) -> Result<(), SidecarError>
where
    B: Serialize,
    F: FnMut(SseEvent),
{
    let url = format!("http://127.0.0.1:{port}{path}");
    let response = client
        .post(&url)
        .bearer_auth(token)
        .header("X-Job-Id", job_id)
        .json(body)
        .send()
        .await
        .map_err(|e| transport(format!("sidecar request failed: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(refusal(status, path, response).await);
    }
    let mut parser = SseParser::default();
    let mut stream = response.bytes_stream();
    let mut pending: Vec<u8> = Vec::new();
    while let Some(chunk) = tokio::time::timeout(STREAM_READ_TIMEOUT, stream.next())
        .await
        .map_err(|_| transport("sidecar stream timed out".to_string()))?
    {
        let chunk = chunk.map_err(|e| transport(format!("sidecar stream failed: {e}")))?;
        pending.extend_from_slice(&chunk);
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) => error.valid_up_to(),
        };
        if valid == 0 {
            continue;
        }
        let text = std::str::from_utf8(&pending[..valid])
            .expect("validated utf-8 prefix")
            .to_string();
        pending.drain(..valid);
        for event in parser.feed(&text) {
            on_event(event);
        }
    }
    Ok(())
}

pub async fn post_json<B, R>(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    path: &str,
    body: &B,
) -> Result<R, SidecarError>
where
    B: Serialize,
    R: serde::de::DeserializeOwned,
{
    let url = format!("http://127.0.0.1:{port}{path}");
    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|e| transport(format!("sidecar request failed: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(refusal(status, path, response).await);
    }
    response
        .json::<R>()
        .await
        .map_err(|e| transport(format!("failed to parse {path} response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_keeps_its_code_even_when_the_engine_sends_no_message() {
        assert_eq!(
            parse_refusal_detail(r#"{"detail":{"code":"ERR_BUSY","message":"slot held"}}"#),
            ("ERR_BUSY".to_string(), "slot held".to_string())
        );
        let (code, message) =
            parse_refusal_detail(r#"{"detail":{"code":"ERR_BUSY","retry_after":30}}"#);
        assert_eq!(
            code, "ERR_BUSY",
            "classification switches on the code alone, so a detail that omits the human \
             message must not cost us the retry"
        );
        assert!(
            message.contains("retry_after"),
            "with no message to show, the body is the most useful thing left: {message}"
        );
    }

    #[test]
    fn a_body_that_is_not_the_engine_error_shape_reports_no_code() {
        for body in [
            "",
            "<html>502 Bad Gateway</html>",
            r#"{"detail":[{"loc":["body","pages"],"msg":"ERR_BUSY appears in the book text"}]}"#,
            r#"{"detail":{"message":"no code here"}}"#,
        ] {
            let (code, _) = parse_refusal_detail(body);
            assert_eq!(
                code, UNSTRUCTURED_CODE,
                "a validation detail echoes the request, and /script requests carry raw book \
                 text, so nothing but the engine's own code field may produce a code: {body}"
            );
        }
    }

    #[test]
    fn parses_single_event() {
        let mut parser = SseParser::default();
        let events = parser.feed("event: progress\ndata: {\"page\": 1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "progress");
        assert_eq!(events[0].data["page"], 1);
    }

    #[test]
    fn buffers_partial_chunks() {
        let mut parser = SseParser::default();
        assert!(parser.feed("event: result\nda").is_empty());
        let events = parser.feed("ta: {\"ok\": true}\n\nevent: done\ndata: {}\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "result");
        assert_eq!(events[0].data["ok"], true);
        assert_eq!(events[1].event, "done");
    }

    #[test]
    fn skips_blocks_without_event_or_data() {
        let mut parser = SseParser::default();
        assert!(parser.feed(": comment\n\n").is_empty());
        assert!(parser.feed("event: orphan\n\n").is_empty());
        assert!(parser.feed("data: {}\n\n").is_empty());
    }

    #[test]
    fn skips_invalid_json_data() {
        let mut parser = SseParser::default();
        assert!(parser.feed("event: bad\ndata: not-json\n\n").is_empty());
    }
}
