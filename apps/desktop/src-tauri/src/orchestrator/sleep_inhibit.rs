//! Keeps the machine awake for the lifetime of an overnight cast.
//!
//! Ownership: the guard is a local of `run_job` (worker.rs). Cancel returns
//! from it, shutdown aborts the future, a panic unwinds it — every exit path
//! drops the guard, and dropping it is the release. The OS lock itself lives
//! on a dedicated thread because the Windows call is thread-bound.
use std::sync::mpsc;

/// The platform side: take the lock, hold it until `until` closes, release.
/// `Err` = lock unavailable (no session bus, headless VM); never a panic.
pub trait InhibitBackend: Send + 'static {
    fn hold(&self, reason: &str, until: mpsc::Receiver<()>) -> Result<(), String>;
}

/// Linux: `org.freedesktop.ScreenSaver` + logind `Inhibit(idle|sleep)`;
/// Windows: `SetThreadExecutionState`; macOS: IOKit power assertions.
pub struct KeepAwakeBackend;

impl InhibitBackend for KeepAwakeBackend {
    fn hold(&self, reason: &str, until: mpsc::Receiver<()>) -> Result<(), String> {
        let _guard = keepawake::Builder::default()
            .idle(true)
            .sleep(true)
            .reason(reason)
            .app_name("txtCaster")
            .app_reverse_domain("io.github.meedoomostafa.txtcaster")
            .create()
            .map_err(|error| error.to_string())?;
        let _ = until.recv();
        Ok(())
    }
}

pub struct SleepInhibitor {
    _release: mpsc::Sender<()>,
}

impl SleepInhibitor {
    pub fn acquire(reason: &str) -> Option<SleepInhibitor> {
        Self::acquire_with(KeepAwakeBackend, reason)
    }

    pub fn acquire_with<B: InhibitBackend>(backend: B, reason: &str) -> Option<SleepInhibitor> {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let reason = reason.to_string();
        std::thread::Builder::new()
            .name("sleep-inhibit".into())
            .spawn(move || {
                if let Err(error) = backend.hold(&reason, release_rx) {
                    tracing::warn!("sleep inhibit unavailable: {error}");
                }
            })
            .ok()?;
        Some(SleepInhibitor {
            _release: release_tx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{InhibitBackend, SleepInhibitor};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    /// Records the lock lifecycle the way the OS would see it.
    struct Recording {
        acquired: Arc<AtomicUsize>,
        released: Arc<AtomicUsize>,
        fail: bool,
    }

    impl InhibitBackend for Recording {
        fn hold(&self, _reason: &str, until: mpsc::Receiver<()>) -> Result<(), String> {
            if self.fail {
                return Err("no session bus".into());
            }
            self.acquired.fetch_add(1, Ordering::SeqCst);
            let _ = until.recv();
            self.released.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn recording(fail: bool) -> (Recording, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let acquired = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let backend = Recording {
            acquired: acquired.clone(),
            released: released.clone(),
            fail,
        };
        (backend, acquired, released)
    }

    fn wait_for(counter: &AtomicUsize, want: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while counter.load(Ordering::SeqCst) != want {
            assert!(Instant::now() < deadline, "counter never reached {want}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn drop_releases_the_lock() {
        let (backend, acquired, released) = recording(false);
        let inhibitor = SleepInhibitor::acquire_with(backend, "test run");
        assert!(inhibitor.is_some());
        wait_for(&acquired, 1);
        assert_eq!(released.load(Ordering::SeqCst), 0, "held while alive");
        drop(inhibitor);
        wait_for(&released, 1);
    }

    #[test]
    fn a_panic_in_the_holding_scope_releases_the_lock() {
        let (backend, acquired, released) = recording(false);
        let outcome = std::panic::catch_unwind(move || {
            let _guard = SleepInhibitor::acquire_with(backend, "test run");
            wait_for(&acquired, 1);
            panic!("stage blew up");
        });
        assert!(outcome.is_err());
        wait_for(&released, 1);
    }

    #[test]
    fn aborting_the_holding_future_releases_the_lock() {
        // Mirrors `worker.abort()` on shutdown: the run_job future is dropped
        // mid-await and its `_sleep_inhibit` local must go with it.
        let (backend, acquired, released) = recording(false);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.spawn(async move {
            let _guard = SleepInhibitor::acquire_with(backend, "test run");
            std::future::pending::<()>().await;
        });
        wait_for(&acquired, 1);
        handle.abort();
        wait_for(&released, 1);
    }

    #[test]
    fn a_backend_failure_never_panics_and_never_counts_as_held() {
        let (backend, acquired, released) = recording(true);
        let inhibitor = SleepInhibitor::acquire_with(backend, "test run");
        std::thread::sleep(Duration::from_millis(50));
        drop(inhibitor);
        assert_eq!(acquired.load(Ordering::SeqCst), 0);
        assert_eq!(released.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn acquire_never_panics_and_drop_is_clean() {
        let inhibitor = SleepInhibitor::acquire("test run");
        drop(inhibitor);
    }

    /// Real-call smoke (Linux, needs logind): `cargo test -- --ignored systemd`.
    #[test]
    #[ignore]
    fn the_real_lock_is_visible_to_systemd_inhibit_and_gone_after_drop() {
        fn listed() -> bool {
            let out = std::process::Command::new("systemd-inhibit")
                .arg("--list")
                .output()
                .expect("systemd-inhibit present");
            String::from_utf8_lossy(&out.stdout).contains("txtCaster")
        }
        let inhibitor = SleepInhibitor::acquire("smoke: overnight cast");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !listed() {
            assert!(Instant::now() < deadline, "lock never appeared");
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(inhibitor);
        let deadline = Instant::now() + Duration::from_secs(5);
        while listed() {
            assert!(Instant::now() < deadline, "lock never released");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
