use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::watch;
use tokio::sync::Notify;
use tokio::time::timeout;

use super::protocol::{parse_handshake, BackoffPolicy, EngineStatus};
use super::spawn::{
    compiled_checkout_engine, explicit_dev_engine, generate_token, resolve_launch, spawn_sidecar,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const STABLE_GRACE: Duration = Duration::from_secs(10);
const MAX_HEALTH_FAILURES: u32 = 6;
const GPU_REPORT_POLL: Duration = Duration::from_millis(500);
const GPU_REPORT_DEADLINE: Duration = Duration::from_secs(120);

struct FailureThreshold {
    failures: u32,
    max: u32,
}

impl FailureThreshold {
    fn new(max: u32) -> Self {
        Self { failures: 0, max }
    }

    fn record(&mut self, healthy: bool) -> bool {
        if healthy {
            self.failures = 0;
        } else {
            self.failures += 1;
        }
        self.failures >= self.max
    }
}

pub struct SupervisorHandle {
    inner: Mutex<SupervisorSnapshot>,
    shutting_down: AtomicBool,
    stopped_notify: Notify,
    child_stopped_notify: Notify,
    shutdown_tx: watch::Sender<bool>,
    respawn_tx: watch::Sender<u64>,
    stop_tx: watch::Sender<u64>,
}

#[derive(Clone)]
struct SupervisorSnapshot {
    status: EngineStatus,
    token: String,
    port: Option<u16>,
}

impl SupervisorHandle {
    pub fn new(token: String) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        let (respawn_tx, _) = watch::channel(0);
        let (stop_tx, _) = watch::channel(0);
        Self {
            inner: Mutex::new(SupervisorSnapshot {
                status: EngineStatus::Starting,
                token,
                port: None,
            }),
            shutting_down: AtomicBool::new(false),
            stopped_notify: Notify::new(),
            child_stopped_notify: Notify::new(),
            shutdown_tx,
            respawn_tx,
            stop_tx,
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.inner.lock().expect("supervisor lock").status.clone()
    }

    pub fn connection(&self) -> Option<(u16, String)> {
        let snapshot = self.inner.lock().expect("supervisor lock");
        snapshot.port.map(|port| (port, snapshot.token.clone()))
    }

    pub fn set_status(&self, status: EngineStatus) {
        let mut snapshot = self.inner.lock().expect("supervisor lock");
        if let EngineStatus::Ready { port } = status {
            snapshot.port = Some(port);
        } else {
            snapshot.port = None;
        }
        snapshot.status = status;
    }

    pub fn publish_status(app: &AppHandle, status: EngineStatus) {
        publish(app, status);
    }

    pub fn set_downloading(&self, bytes_done: u64, bytes_total: u64, speed_bps: u64) {
        self.set_status(EngineStatus::Downloading {
            bytes_done,
            bytes_total,
            speed_bps,
        });
    }

    pub fn set_extracting(&self, files_done: usize, files_total: usize) {
        self.set_status(EngineStatus::Extracting {
            files_done,
            files_total,
        });
    }

    pub fn set_starting(&self) {
        self.set_status(EngineStatus::Starting);
    }

    pub fn set_ready(&self, port: u16) {
        self.set_status(EngineStatus::Ready { port });
    }

    pub fn set_not_installed(&self) {
        self.set_status(EngineStatus::NotInstalled);
    }

    pub fn set_failed(&self, error_code: impl Into<String>, message: impl Into<String>) {
        self.set_status(EngineStatus::Failed {
            error_code: error_code.into(),
            message: message.into(),
        });
    }

    fn token(&self) -> String {
        self.inner.lock().expect("supervisor lock").token.clone()
    }

    pub async fn shutdown(&self, timeout: Duration) {
        self.shutting_down.store(true, Ordering::Release);
        let _ = self.shutdown_tx.send(true);
        let stopped = self.stopped_notify.notified();
        if self.status() == EngineStatus::Down {
            return;
        }
        let _ = tokio::time::timeout(timeout, stopped).await;
    }

    pub async fn stop_sidecar(&self, timeout: Duration) -> bool {
        if matches!(
            self.status(),
            EngineStatus::NotInstalled | EngineStatus::NeedsUpdate { .. } | EngineStatus::Down
        ) {
            return true;
        }
        let notified = self.child_stopped_notify.notified();
        let current = *self.stop_tx.borrow();
        let _ = self.stop_tx.send(current.wrapping_add(1));
        tokio::time::timeout(timeout, notified).await.is_ok()
    }

    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let _ = self.shutdown_tx.send(true);
    }

    pub async fn request_engine_shutdown(&self) {
        let Some((port, token)) = self.connection() else {
            return;
        };
        let _ = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/shutdown"))
            .bearer_auth(token)
            .timeout(Duration::from_secs(1))
            .send()
            .await;
    }

    pub fn respawn(&self) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        {
            let mut snapshot = self.inner.lock().expect("supervisor lock");
            snapshot.port = None;
            snapshot.status = EngineStatus::Starting;
        }
        let current = *self.respawn_tx.borrow();
        let _ = self.respawn_tx.send(current.wrapping_add(1));
    }
}

impl crate::orchestrator::SidecarProvider for SupervisorHandle {
    fn connection(&self) -> Option<(u16, String)> {
        SupervisorHandle::connection(self)
    }

    fn respawn(&self) {
        SupervisorHandle::respawn(self);
    }
}

pub fn start(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        run_loop(handle).await;
    });
}

fn publish(app: &AppHandle, status: EngineStatus) {
    app.state::<SupervisorHandle>().set_status(status.clone());
    if let Err(error) = app.emit("engine-status", &status) {
        tracing::warn!("failed to emit engine-status: {error}");
    }
}

async fn run_loop(app: AppHandle) {
    struct Stopped<'a>(&'a SupervisorHandle);
    impl Drop for Stopped<'_> {
        fn drop(&mut self) {
            self.0.set_status(EngineStatus::Down);
            self.0.stopped_notify.notify_waiters();
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let resource_dir = app.path().resource_dir().ok();
    let explicit_engine =
        explicit_dev_engine(std::env::var_os(super::spawn::DEV_ENGINE_ENV).as_deref());
    let checkout_engine = compiled_checkout_engine();
    let Some(exe_dir) = exe_dir else {
        publish(&app, EngineStatus::Down);
        return;
    };

    let supervisor = app.state::<SupervisorHandle>();
    let _stopped = Stopped(&supervisor);
    let token = supervisor.token();
    let mut shutdown_rx = supervisor.shutdown_tx.subscribe();
    let mut respawn_rx = supervisor.respawn_tx.subscribe();
    let mut stop_rx = supervisor.stop_tx.subscribe();
    let client = reqwest::Client::new();
    let mut backoff = BackoffPolicy::default();
    let mut attempt: u32 = 0;
    let mut last_respawn_rev: u64 = *respawn_rx.borrow();

    loop {
        if supervisor.shutting_down.load(Ordering::Acquire) || *shutdown_rx.borrow() {
            return;
        }

        let data_dir = crate::storage::paths::resolve_app_data_dir(&app).ok();
        let launch = match &explicit_engine {
            Some(engine_dir) => Some(super::spawn::SidecarLaunch::Dev {
                engine_dir: engine_dir.clone(),
            }),
            None => resolve_launch(
                data_dir.as_deref(),
                &exe_dir,
                resource_dir.as_deref(),
                checkout_engine.as_deref(),
            ),
        };

        let Some(launch) = launch else {
            tracing::info!("no sidecar binary or dev engine found; engine status is NotInstalled");
            publish(&app, EngineStatus::NotInstalled);
            if !wait_for_respawn(
                &mut shutdown_rx,
                &mut respawn_rx,
                &mut last_respawn_rev,
                &mut attempt,
                &mut backoff,
            )
            .await
            {
                return;
            }
            continue;
        };

        if attempt == 0 {
            publish(&app, EngineStatus::Starting);
        } else {
            publish(&app, EngineStatus::Restarting { attempt });
        }

        let started = tokio::select! {
            _ = shutdown_rx.changed() => return,
            _ = stop_rx.changed() => {
                supervisor.child_stopped_notify.notify_waiters();
                if !wait_for_respawn(
                    &mut shutdown_rx,
                    &mut respawn_rx,
                    &mut last_respawn_rev,
                    &mut attempt,
                    &mut backoff,
                )
                .await
                {
                    return;
                }
                continue;
            }
            result = start_once(&launch, &token) => result,
        };
        match started {
            Ok((mut child, stdin, port)) => {
                let pid = child.id();
                let health_url = format!("http://127.0.0.1:{port}/health");
                let probe_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
                let mut ready = false;
                let mut stopped_during_probe = false;
                while Instant::now() < probe_deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    if health_ok(&client, &health_url, &token).await {
                        ready = true;
                        break;
                    }
                    tokio::select! {
                        _ = shutdown_rx.changed() => return,
                        _ = stop_rx.changed() => {
                            stopped_during_probe = true;
                            break;
                        }
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
                if stopped_during_probe {
                    drop(stdin);
                    terminate_child(&mut child, pid).await;
                    supervisor.child_stopped_notify.notify_waiters();
                    if !wait_for_respawn(
                        &mut shutdown_rx,
                        &mut respawn_rx,
                        &mut last_respawn_rev,
                        &mut attempt,
                        &mut backoff,
                    )
                    .await
                    {
                        return;
                    }
                    continue;
                }
                if !ready {
                    tracing::warn!("sidecar health probe failed or timed out on port {port}");
                    drop(stdin);
                    terminate_child(&mut child, pid).await;
                    continue;
                }
                let installed_api =
                    match read_engine_api(&client, &health_url, &token, probe_deadline).await {
                        Ok(api) => api,
                        Err(()) => {
                            tracing::warn!(
                            "sidecar on port {port} answered once, then not again; restarting it"
                        );
                            drop(stdin);
                            terminate_child(&mut child, pid).await;
                            continue;
                        }
                    };
                if installed_api != Some(crate::engine_install::install::REQUIRED_ENGINE_API) {
                    tracing::warn!(
                        "engine speaks api {installed_api:?}, the app needs {}; it must be updated",
                        crate::engine_install::install::REQUIRED_ENGINE_API
                    );
                    drop(stdin);
                    terminate_child(&mut child, pid).await;
                    publish(
                        &app,
                        EngineStatus::NeedsUpdate {
                            installed_api,
                            required_api: crate::engine_install::install::REQUIRED_ENGINE_API,
                        },
                    );
                    if !wait_for_respawn(
                        &mut shutdown_rx,
                        &mut respawn_rx,
                        &mut last_respawn_rev,
                        &mut attempt,
                        &mut backoff,
                    )
                    .await
                    {
                        return;
                    }
                    continue;
                }
                publish(&app, EngineStatus::Ready { port });
                let ready_at = Instant::now();
                let gpu_generation = crate::hardware::engine_gpus::begin_engine();
                let gpu_report = tokio::spawn(learn_engine_gpus(
                    client.clone(),
                    health_url.clone(),
                    token.clone(),
                    gpu_generation,
                ));
                let mut stopped_by_request = false;
                tokio::select! {
                    _ = shutdown_rx.changed() => {}
                    _ = respawn_rx.changed() => {}
                    _ = stop_rx.changed() => {
                        stopped_by_request = true;
                    }
                    _ = monitor(&client, &mut child, port, &token) => {}
                }
                gpu_report.abort();
                crate::hardware::engine_gpus::end_engine(gpu_generation);
                if ready_at.elapsed() >= STABLE_GRACE {
                    backoff.reset();
                }
                drop(stdin);
                terminate_child(&mut child, pid).await;
                if stopped_by_request {
                    supervisor.child_stopped_notify.notify_waiters();
                    if !wait_for_respawn(
                        &mut shutdown_rx,
                        &mut respawn_rx,
                        &mut last_respawn_rev,
                        &mut attempt,
                        &mut backoff,
                    )
                    .await
                    {
                        return;
                    }
                    continue;
                }
            }
            Err(error) => {
                tracing::warn!("sidecar start failed: {error}");
            }
        }

        if supervisor.shutting_down.load(Ordering::Acquire) || *shutdown_rx.borrow() {
            return;
        }
        let current_respawn = *respawn_rx.borrow();
        if current_respawn > last_respawn_rev {
            last_respawn_rev = current_respawn;
            backoff.reset();
            attempt = 0;
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        attempt += 1;
        tokio::select! {
            _ = shutdown_rx.changed() => return,
            _ = respawn_rx.changed() => {
                backoff.reset();
                attempt = 0;
            }
            _ = stop_rx.changed() => {
                supervisor.child_stopped_notify.notify_waiters();
                if !wait_for_respawn(
                    &mut shutdown_rx,
                    &mut respawn_rx,
                    &mut last_respawn_rev,
                    &mut attempt,
                    &mut backoff,
                )
                .await
                {
                    return;
                }
                continue;
            }
            _ = tokio::time::sleep(backoff.next_delay()) => {}
        }
    }
}

async fn wait_for_respawn(
    shutdown_rx: &mut watch::Receiver<bool>,
    respawn_rx: &mut watch::Receiver<u64>,
    last_respawn_rev: &mut u64,
    attempt: &mut u32,
    backoff: &mut BackoffPolicy,
) -> bool {
    tokio::select! {
        _ = shutdown_rx.changed() => false,
        _ = respawn_rx.changed() => {
            *last_respawn_rev = *respawn_rx.borrow();
            *attempt = 0;
            backoff.reset();
            true
        }
    }
}

async fn terminate_child(child: &mut Child, pid: Option<u32>) {
    let _ = crate::process_group::terminate(child, pid, Duration::from_secs(2)).await;
}

async fn start_once(
    launch: &super::spawn::SidecarLaunch,
    token: &str,
) -> Result<(Child, Option<ChildStdin>, u16), String> {
    let mut child = spawn_sidecar(launch, token).map_err(|e| e.to_string())?;
    let guard = crate::process_group::Guard::new(child.id());
    let stdin = child.stdin.take();
    if let Some(stderr) = child.stderr.take() {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match line.split(' ').next() {
                    Some("CRITICAL") | Some("ERROR") => {
                        tracing::error!(target: "sidecar", "{line}");
                    }
                    Some("WARNING") => tracing::warn!(target: "sidecar", "{line}"),
                    Some("DEBUG") => tracing::debug!(target: "sidecar", "{line}"),
                    _ => tracing::info!(target: "sidecar", "{line}"),
                }
            }
        });
    }
    let stdout = child.stdout.take().ok_or("sidecar stdout not captured")?;
    let mut lines = BufReader::new(stdout).lines();

    let port = timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Some(port) = parse_handshake(&line) {
                        return Ok(port);
                    }
                }
                Ok(None) => return Err("sidecar exited before handshake".to_string()),
                Err(error) => return Err(format!("sidecar stdout read failed: {error}")),
            }
        }
    })
    .await
    .map_err(|_| "handshake timed out".to_string())??;

    guard.disarm();
    Ok((child, stdin, port))
}

async fn monitor(client: &reqwest::Client, child: &mut Child, port: u16, token: &str) {
    let url = format!("http://127.0.0.1:{port}/health");
    let mut threshold = FailureThreshold::new(MAX_HEALTH_FAILURES);
    loop {
        tokio::select! {
            _ = child.wait() => return,
            _ = tokio::time::sleep(HEALTH_INTERVAL) => {}
        }
        let healthy = tokio::select! {
            _ = child.wait() => return,
            healthy = health_ok(client, &url, token) => healthy,
        };
        if !healthy && threshold.failures + 1 >= 2 {
            tracing::warn!(
                "health check failed ({}/{})",
                threshold.failures + 1,
                MAX_HEALTH_FAILURES
            );
        }
        if threshold.record(healthy) {
            return;
        }
    }
}

async fn engine_api(client: &reqwest::Client, url: &str, token: &str) -> Result<Option<u32>, ()> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let body: serde_json::Value = response.json().await.map_err(|_| ())?;
    Ok(body
        .get("engine_api")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok()))
}

async fn read_engine_api(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    deadline: Instant,
) -> Result<Option<u32>, ()> {
    loop {
        match engine_api(client, url, token).await {
            Ok(api) => return Ok(api),
            Err(()) if Instant::now() >= deadline => return Err(()),
            Err(()) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn learn_engine_gpus(client: reqwest::Client, url: String, token: String, generation: u64) {
    use crate::hardware::engine_gpus::{record, report_in, Report};
    let deadline = Instant::now() + GPU_REPORT_DEADLINE;
    while Instant::now() < deadline {
        let body = match client
            .get(&url)
            .bearer_auth(&token)
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
        {
            Ok(response) => response.json::<serde_json::Value>().await.ok(),
            Err(_) => None,
        };
        match body.as_ref().map(report_in) {
            Some(Report::Ready(gpus)) => {
                let names: Vec<String> = gpus
                    .iter()
                    .map(|gpu| format!("{}:{}", gpu.backend, gpu.description))
                    .collect();
                tracing::info!("engine reports {} gpu(s): {}", gpus.len(), names.join(", "));
                record(generation, gpus);
                return;
            }
            Some(Report::Absent) => return,
            Some(Report::Pending) | None => {}
        }
        tokio::time::sleep(GPU_REPORT_POLL).await;
    }
    tracing::warn!("engine did not report its gpus within {GPU_REPORT_DEADLINE:?}");
}

async fn health_ok(client: &reqwest::Client, url: &str, token: &str) -> bool {
    client
        .get(url)
        .bearer_auth(token)
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

pub fn new_handle() -> SupervisorHandle {
    SupervisorHandle::new(generate_token())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_engine_api_differs_from_a_probe_that_failed() {
        use crate::engine_install::fetch::fixture::{serve, Served};
        use std::sync::{Arc, Mutex};

        let state = Arc::new(Mutex::new(Served::default()));
        state.lock().expect("lock").files.insert(
            "/current".into(),
            br#"{"status":"ok","engine_api":1,"version":"0.1.0"}"#.to_vec(),
        );
        state.lock().expect("lock").files.insert(
            "/old".into(),
            br#"{"status":"ok","version":"0.0.9"}"#.to_vec(),
        );
        let base = serve(Arc::clone(&state)).await;
        let client = reqwest::Client::new();
        let current = engine_api(&client, &format!("{base}/current"), "tok").await;
        assert_eq!(current, Ok(Some(1)));
        let old = engine_api(&client, &format!("{base}/old"), "tok").await;
        assert_eq!(
            old,
            Ok(None),
            "an engine that answered without the field is an old engine"
        );
        let missing = engine_api(&client, &format!("{base}/missing"), "tok").await;
        assert_eq!(
            missing,
            Err(()),
            "a 404 is a failed probe, not an old engine"
        );
        let unreachable = engine_api(&client, "http://127.0.0.1:9/health", "tok").await;
        assert_eq!(
            unreachable,
            Err(()),
            "a refused connection is a failed probe"
        );
    }

    #[test]
    fn handle_starts_in_starting_state() {
        let handle = SupervisorHandle::new("tok".into());
        assert_eq!(handle.status(), EngineStatus::Starting);
        assert_eq!(handle.connection(), None);
    }

    #[test]
    fn ready_status_exposes_connection() {
        let handle = SupervisorHandle::new("tok".into());
        handle.set_status(EngineStatus::Ready { port: 1234 });
        assert_eq!(handle.status(), EngineStatus::Ready { port: 1234 });
        assert_eq!(handle.connection(), Some((1234, "tok".into())));
    }

    #[test]
    fn non_ready_status_clears_connection() {
        let handle = SupervisorHandle::new("tok".into());
        handle.set_status(EngineStatus::Ready { port: 1234 });
        handle.set_status(EngineStatus::Restarting { attempt: 1 });
        assert_eq!(handle.connection(), None);
    }

    #[test]
    fn respawn_resets_connection_and_notifies() {
        let handle = SupervisorHandle::new("tok".into());
        handle.set_status(EngineStatus::Ready { port: 1234 });
        assert_eq!(handle.connection(), Some((1234, "tok".into())));
        let mut rx = handle.respawn_tx.subscribe();
        handle.respawn();
        assert_eq!(handle.status(), EngineStatus::Starting);
        assert_eq!(handle.connection(), None);
        assert_eq!(*rx.borrow_and_update(), 1);
    }

    #[test]
    fn threshold_trips_after_consecutive_failures() {
        let mut threshold = FailureThreshold::new(3);
        assert!(!threshold.record(false));
        assert!(!threshold.record(false));
        assert!(threshold.record(false));
    }

    #[test]
    fn threshold_resets_on_success() {
        let mut threshold = FailureThreshold::new(3);
        assert!(!threshold.record(false));
        assert!(!threshold.record(false));
        assert!(!threshold.record(true));
        assert!(!threshold.record(false));
        assert!(!threshold.record(false));
        assert!(threshold.record(false));
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_and_clears_connection() {
        let handle = SupervisorHandle::new("tok".into());
        handle.set_status(EngineStatus::Ready { port: 1234 });
        let stopped = handle.stopped_notify.notified();
        handle.shutting_down.store(true, Ordering::Release);
        handle.set_status(EngineStatus::Down);
        handle.stopped_notify.notify_waiters();
        stopped.await;
        handle.shutdown(Duration::from_millis(1)).await;
        handle.shutdown(Duration::from_millis(1)).await;
        assert_eq!(handle.status(), EngineStatus::Down);
        assert_eq!(handle.connection(), None);
    }

    #[test]
    fn busy_sidecar_tolerated_for_a_minute() {
        let mut threshold = FailureThreshold::new(MAX_HEALTH_FAILURES);
        for _ in 0..MAX_HEALTH_FAILURES - 1 {
            assert!(!threshold.record(false));
        }
        assert!(threshold.record(false));
        let worst_case = MAX_HEALTH_FAILURES * (HEALTH_INTERVAL + HEALTH_TIMEOUT).as_secs() as u32;
        assert!(worst_case >= 55);
    }

    #[test]
    fn dynamic_state_helpers_update_status_and_connection() {
        let handle = SupervisorHandle::new("secret-tok".into());
        assert_eq!(handle.status(), EngineStatus::Starting);

        handle.set_downloading(100, 500, 50);
        assert_eq!(
            handle.status(),
            EngineStatus::Downloading {
                bytes_done: 100,
                bytes_total: 500,
                speed_bps: 50,
            }
        );
        assert_eq!(handle.connection(), None);

        handle.set_extracting(2, 10);
        assert_eq!(
            handle.status(),
            EngineStatus::Extracting {
                files_done: 2,
                files_total: 10,
            }
        );
        assert_eq!(handle.connection(), None);

        handle.set_ready(9090);
        assert_eq!(handle.status(), EngineStatus::Ready { port: 9090 });
        assert_eq!(handle.connection(), Some((9090, "secret-tok".into())));

        handle.set_failed("ERR_EXTRACT", "disk full");
        assert_eq!(
            handle.status(),
            EngineStatus::Failed {
                error_code: "ERR_EXTRACT".into(),
                message: "disk full".into(),
            }
        );
        assert_eq!(handle.connection(), None);

        handle.set_not_installed();
        assert_eq!(handle.status(), EngineStatus::NotInstalled);
        assert_eq!(handle.connection(), None);

        handle.set_starting();
        assert_eq!(handle.status(), EngineStatus::Starting);
        assert_eq!(handle.connection(), None);
    }

    #[tokio::test]
    async fn stop_sidecar_noop_when_not_installed_or_down() {
        let handle = SupervisorHandle::new("tok".into());
        handle.set_status(EngineStatus::NotInstalled);
        handle.stop_sidecar(Duration::from_millis(5)).await;
        assert_eq!(handle.status(), EngineStatus::NotInstalled);

        handle.set_status(EngineStatus::Down);
        handle.stop_sidecar(Duration::from_millis(5)).await;
        assert_eq!(handle.status(), EngineStatus::Down);
    }
}
