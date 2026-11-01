//! check → apply manifest; install → download, verify, hand off.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use super::download::{stage_download, sweep_staging, Progress};
use super::install::{self as installer, InstallPlan};
use super::manifest::{self, Manifest, Offer, MANIFEST_MAX_BYTES};
use super::{now_unix, Handoff, Phase, Snapshot, UpdateError, Updater};

impl Updater {
    async fn fetch_manifest(&self) -> Result<Manifest, UpdateError> {
        let (url, key) = match (&self.config.manifest_url, &self.config.pubkey) {
            (Some(u), Some(k)) => (u.clone(), k),
            _ => return Err(UpdateError::Disabled),
        };
        let response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(super::state::HTTP_TIMEOUT_SECS))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(UpdateError::Http {
                status: status.as_u16(),
            });
        }
        let mut body: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len() + chunk.len() > MANIFEST_MAX_BYTES {
                return Err(UpdateError::ManifestTooLarge {
                    bytes: body.len() + chunk.len(),
                    cap: MANIFEST_MAX_BYTES,
                });
            }
            body.extend_from_slice(&chunk);
        }
        manifest::verify_and_parse(&body, key)
    }

    /// One check. Never raises: every outcome lands in the snapshot.
    pub async fn check(&self) -> Snapshot {
        if !self.config.enabled() || self.busy.swap(true, Ordering::SeqCst) {
            return self.snapshot();
        }
        let previous = self.snapshot().phase;
        let fallback = match previous {
            Phase::Available | Phase::UpToDate | Phase::Paused => previous,
            _ => Phase::Idle,
        };
        self.update(|i| i.snapshot.phase = Phase::Checking);
        match self.fetch_manifest().await {
            Ok(m) => self.apply_manifest(m),
            // A check that got no manifest changed nothing and tried no install: an origin
            // with nothing published yet, or down, reads like being offline.
            Err(UpdateError::Http { status }) => {
                tracing::warn!("updater: manifest fetch returned HTTP {status}");
                self.update(|i| {
                    i.snapshot.phase = fallback;
                    i.snapshot.last_attempt_offline = true;
                });
            }
            Err(e) => self.fail(&e, fallback),
        }
        self.busy.store(false, Ordering::SeqCst);
        self.snapshot()
    }

    fn settle(&self, phase: Phase, manifest: Option<Manifest>) {
        let checked = now_unix();
        self.update(|i| {
            let s = &mut i.snapshot;
            s.phase = phase;
            s.last_checked_unix = Some(checked);
            s.last_attempt_offline = false;
            s.error_code = None;
            s.error_message = None;
            if let Some(m) = &manifest {
                s.dismissed = s.dismissed && s.available_version.as_deref() == Some(&m.version);
                s.available_version = Some(m.version.clone());
                s.notes = m.notes.clone();
                s.release_url = m.release_url.clone();
                let kind = m
                    .platforms
                    .get(&manifest::platform_key())
                    .and_then(|a| a.kind.as_deref());
                s.install_kind = Some(
                    match installer::plan(kind, self.config.appimage.as_deref()) {
                        InstallPlan::OpenReleasePage => "open",
                        _ => "restart",
                    },
                );
            } else {
                s.dismissed = false;
                s.available_version = None;
                s.notes = None;
                s.release_url = None;
                s.install_kind = None;
            }
            i.manifest = manifest;
        });
    }

    fn apply_manifest(&self, m: Manifest) {
        if m.pause_updates {
            if !self.paused_logged.swap(true, Ordering::SeqCst) {
                tracing::info!("updater: publisher paused updates; nothing is shown");
            }
            return self.settle(Phase::Paused, None);
        }
        match manifest::compare(&self.config.current_version, &m.version) {
            Err(e) => self.fail(&e, Phase::Idle),
            Ok(Offer::Same) => self.settle(Phase::UpToDate, None),
            Ok(Offer::Older) => {
                tracing::info!(
                    "updater: origin offers {}, not newer than {}; refused",
                    m.version,
                    self.config.current_version
                );
                self.settle(Phase::UpToDate, None);
            }
            Ok(Offer::Newer) => {
                let has_artifact = m.platforms.contains_key(&manifest::platform_key());
                if !has_artifact && m.release_url.is_none() {
                    tracing::warn!(
                        "updater: {} offers nothing for {}; treated as up to date",
                        m.version,
                        manifest::platform_key()
                    );
                    return self.settle(Phase::UpToDate, None);
                }
                self.settle(Phase::Available, Some(m));
            }
        }
    }

    /// One click: download, verify, hand off. `Ok` means the caller must now
    /// exit or restart the process; any `Err` leaves the current install intact.
    pub async fn install(&self) -> Result<Handoff, UpdateError> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(UpdateError::Busy);
        }
        let result = self.install_inner().await;
        self.busy.store(false, Ordering::SeqCst);
        match &result {
            Err(UpdateError::Busy) => {}
            Err(e) => self.fail(e, Phase::Available),
            Ok(_) => {}
        }
        result
    }

    async fn install_inner(&self) -> Result<Handoff, UpdateError> {
        let key = self.config.pubkey.clone().ok_or(UpdateError::Disabled)?;
        let (manifest, phase) = {
            let i = self.inner.lock().unwrap();
            (i.manifest.clone(), i.snapshot.phase)
        };
        let manifest =
            manifest.ok_or_else(|| UpdateError::Install("no update is available".into()))?;
        if phase != Phase::Available {
            return Err(UpdateError::Busy);
        }
        let platform = manifest::platform_key();
        let artifact = manifest
            .platforms
            .get(&platform)
            .cloned()
            .ok_or_else(|| UpdateError::NoArtifactForPlatform(platform.clone()))?;
        let plan = installer::plan(artifact.kind.as_deref(), self.config.appimage.as_deref());
        if plan == InstallPlan::OpenReleasePage {
            return Err(UpdateError::Install(
                "this install cannot self-replace".into(),
            ));
        }
        self.cancel.store(false, Ordering::SeqCst);
        sweep_staging(&self.config.downloads_dir).await;
        let staging = installer::staging_path(
            &plan,
            &self.config.downloads_dir,
            &manifest.version,
            &platform,
        )
        .ok_or_else(|| UpdateError::Install("this install cannot self-replace".into()))?;
        self.update(|i| {
            i.snapshot.phase = Phase::Downloading;
            i.snapshot.received = 0;
            i.snapshot.total = artifact.size;
        });
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let progress = |p: Progress| {
            let done = p.received >= p.total;
            if done || last_emit.elapsed() >= Duration::from_millis(200) {
                last_emit = Instant::now();
                self.update(|i| {
                    i.snapshot.received = p.received;
                    i.snapshot.total = p.total;
                    i.snapshot.phase = if done {
                        Phase::Verifying
                    } else {
                        Phase::Downloading
                    };
                });
            }
        };
        let staged = stage_download(
            &self.client,
            &artifact,
            &staging,
            &key,
            &self.cancel,
            progress,
        )
        .await?;
        self.update(|i| i.snapshot.phase = Phase::Installing);
        let handoff = installer::perform(&plan, staged)?;
        tracing::info!(
            "updater: {} verified and handed off ({handoff:?})",
            manifest.version
        );
        Ok(handoff)
    }
}
