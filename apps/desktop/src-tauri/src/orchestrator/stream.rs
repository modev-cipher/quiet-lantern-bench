use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc;

use super::stage::{ControlReceiver, StageOutcome};
use crate::sidecar::client::{post_json, post_sse, SidecarError, SseEvent};

pub const CANCEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub struct AbortOnDrop(pub tokio::task::JoinHandle<Result<(), SidecarError>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamOutcome {
    Completed,
    Interrupted(StageOutcome),
}

#[derive(serde::Deserialize)]
struct CancelAck {
    #[allow(dead_code)]
    cancelled: bool,
}

#[derive(Clone, Copy)]
pub struct SidecarTarget<'a> {
    pub client: &'a reqwest::Client,
    pub port: u16,
    pub token: &'a str,
}

impl<'a> SidecarTarget<'a> {
    pub fn new(client: &'a reqwest::Client, port: u16, token: &'a str) -> Self {
        SidecarTarget {
            client,
            port,
            token,
        }
    }
}

pub async fn cancel_run(target: SidecarTarget<'_>, run_id: &str) {
    let body = json!({ "job_id": run_id });
    let request: Result<CancelAck, SidecarError> = match tokio::time::timeout(
        CANCEL_REQUEST_TIMEOUT,
        post_json(target.client, target.port, target.token, "/cancel", &body),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("sidecar /cancel for '{run_id}' timed out");
            return;
        }
    };
    if let Err(err) = request {
        tracing::warn!("sidecar /cancel for '{run_id}' failed: {err}");
    }
}

pub async fn post_sse_cancellable<B, F>(
    target: SidecarTarget<'_>,
    path: &'static str,
    run_id: &str,
    body: B,
    control: &mut ControlReceiver,
    mut on_event: F,
) -> Result<StreamOutcome, SidecarError>
where
    B: Serialize + Send + Sync + 'static,
    F: FnMut(SseEvent),
{
    let (tx, mut rx) = mpsc::unbounded_channel::<SseEvent>();
    let stream_client = target.client.clone();
    let stream_token = target.token.to_string();
    let stream_run_id = run_id.to_string();
    let port = target.port;
    let mut task = AbortOnDrop(tokio::spawn(async move {
        post_sse(
            &stream_client,
            port,
            &stream_token,
            path,
            &stream_run_id,
            &body,
            move |event| {
                let _ = tx.send(event);
            },
        )
        .await
    }));

    loop {
        let event = tokio::select! {
            outcome = control.interrupted() => {
                cancel_run(target, run_id).await;
                return Ok(StreamOutcome::Interrupted(outcome));
            }
            event = rx.recv() => event,
        };
        match event {
            Some(event) => on_event(event),
            None => break,
        }
    }

    match (&mut task.0).await {
        Ok(Ok(())) => Ok(StreamOutcome::Completed),
        Ok(Err(err)) => Err(err),
        Err(err) if err.is_cancelled() => Ok(StreamOutcome::Completed),
        Err(err) => Err(SidecarError::Transport(format!(
            "sidecar stream task failed: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::orchestrator::stage::control_channel;

    type SeenPaths = Arc<Mutex<Vec<String>>>;

    async fn spawn_recording_sidecar() -> (u16, SeenPaths) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen: SeenPaths = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let sink = sink.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("")
                        .to_string();
                    sink.lock().unwrap().push(path.clone());
                    if path == "/cancel" {
                        let body = b"{\"job_id\":\"job:run\",\"cancelled\":true}";
                        let _ = crate::test_support::mock_http::write_response(
                            &mut sock,
                            "200 OK",
                            "application/json",
                            body,
                        )
                        .await;
                        return;
                    }
                    let _ = crate::test_support::mock_http::write_head(
                        &mut sock,
                        "200 OK",
                        &[("Content-Type", "text/event-stream")],
                        None,
                    )
                    .await;
                    loop {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        if sock
                            .write_all(b"event: keepalive\ndata: {}\n\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                        let _ = sock.flush().await;
                    }
                });
            }
        });
        (port, seen)
    }

    #[tokio::test]
    async fn cancelling_mid_stream_posts_cancel_to_the_sidecar() {
        let (port, seen) = spawn_recording_sidecar().await;
        let (handle, mut control) = control_channel();
        let client = reqwest::Client::new();
        let delivered: Arc<Mutex<Vec<SseEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = delivered.clone();

        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            handle.cancel();
        });

        let outcome = post_sse_cancellable(
            SidecarTarget::new(&client, port, "token"),
            "/vision",
            "job:run",
            json!({}),
            &mut control,
            move |event| sink.lock().unwrap().push(event),
        )
        .await
        .expect("stream result");

        canceller.await.expect("canceller");
        assert_eq!(outcome, StreamOutcome::Interrupted(StageOutcome::Cancelled));
        let events = delivered.lock().unwrap().clone();
        assert_eq!(
            events.first(),
            Some(&SseEvent {
                event: "keepalive".into(),
                data: json!({}),
            }),
            "a cancelled stream must still have delivered the events that arrived before the \
             cancel; without this the mock could emit nothing, or bytes that corrupt a field, \
             and the outcome assertions below would stay green. Measured scope: a framing \
             artefact on a line of its own does NOT redden this, because the SSE parser ignores \
             an unrecognised line by spec: {events:?}"
        );
        let paths = seen.lock().unwrap().clone();
        assert!(paths.contains(&"/vision".to_string()), "paths: {paths:?}");
        assert!(paths.contains(&"/cancel".to_string()), "paths: {paths:?}");
    }

    #[tokio::test]
    async fn interrupted_stream_reports_the_control_outcome() {
        let (handle, mut control) = control_channel();
        handle.cancel();
        let client = reqwest::Client::new();
        let outcome = post_sse_cancellable(
            SidecarTarget::new(&client, 1, "token"),
            "/vision",
            "job:run",
            json!({}),
            &mut control,
            |_event| panic!("no events expected"),
        )
        .await
        .expect("interrupted");
        assert_eq!(outcome, StreamOutcome::Interrupted(StageOutcome::Cancelled));
    }

    #[tokio::test]
    async fn pause_is_reported_as_paused() {
        let (handle, mut control) = control_channel();
        handle.pause();
        let client = reqwest::Client::new();
        let outcome = post_sse_cancellable(
            SidecarTarget::new(&client, 1, "token"),
            "/layout",
            "job:run",
            json!({}),
            &mut control,
            |_event| {},
        )
        .await
        .expect("interrupted");
        assert_eq!(outcome, StreamOutcome::Interrupted(StageOutcome::Paused));
    }

    #[tokio::test]
    async fn unreachable_sidecar_surfaces_stream_error() {
        let (_handle, mut control) = control_channel();
        let client = reqwest::Client::new();
        let result = post_sse_cancellable(
            SidecarTarget::new(&client, 1, "token"),
            "/vision",
            "job:run",
            json!({}),
            &mut control,
            |_event| {},
        )
        .await;
        assert!(result.is_err());
    }
}
