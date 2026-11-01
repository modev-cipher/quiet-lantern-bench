//! Every failure path of the updater is a typed variant. `is_silent` marks the
//! ones that must never surface as a notice (contract rows d and f: offline is a
//! no-op, cancel is the user's own act).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// No public key was compiled in: the updater trusts nothing and does nothing.
    Disabled,
    /// DNS, connect, TLS or read timeout: the machine is offline as far as we care.
    Offline(String),
    Http {
        status: u16,
    },
    ManifestTooLarge {
        bytes: usize,
        cap: usize,
    },
    /// Envelope missing, not base64, or the signature does not verify with the pinned key.
    ManifestSignature,
    ManifestMalformed(String),
    BadVersion(String),
    /// Plaintext or non-loopback http, or a redirect to one.
    InsecureUrl(String),
    NoArtifactForPlatform(String),
    Digest {
        expected: String,
        actual: String,
    },
    ArtifactSignature,
    SizeExceeded {
        cap: u64,
    },
    SizeMismatch {
        expected: u64,
        actual: u64,
    },
    Cancelled,
    Io(String),
    Install(String),
    /// A check or install is already running.
    Busy,
    /// A symlink was pre-placed where the staging file would be created.
    StagingSymlink(String),
    /// The staged bytes changed between verification and commit.
    StagedTampered,
    /// Another file was moved over the staging path after verification.
    StagedSubstituted,
}

impl UpdateError {
    /// Silent errors are logged at debug level and change no user-visible state.
    pub fn is_silent(&self) -> bool {
        matches!(self, UpdateError::Offline(_) | UpdateError::Cancelled)
    }

    /// Stable machine code for the UI (the catalog maps it to copy) and the log.
    pub fn code(&self) -> &'static str {
        match self {
            UpdateError::Disabled => "disabled",
            UpdateError::Offline(_) => "offline",
            UpdateError::Http { .. } => "http",
            UpdateError::ManifestTooLarge { .. } => "manifest_too_large",
            UpdateError::ManifestSignature => "manifest_signature",
            UpdateError::ManifestMalformed(_) => "manifest_malformed",
            UpdateError::BadVersion(_) => "bad_version",
            UpdateError::InsecureUrl(_) => "insecure_url",
            UpdateError::NoArtifactForPlatform(_) => "no_artifact",
            UpdateError::Digest { .. } => "digest",
            UpdateError::ArtifactSignature => "artifact_signature",
            UpdateError::SizeExceeded { .. } => "size_exceeded",
            UpdateError::SizeMismatch { .. } => "size_mismatch",
            UpdateError::Cancelled => "cancelled",
            UpdateError::Io(_) => "io",
            UpdateError::Install(_) => "install",
            UpdateError::Busy => "busy",
            UpdateError::StagingSymlink(_) => "staging_symlink",
            UpdateError::StagedTampered => "staged_tampered",
            UpdateError::StagedSubstituted => "staged_substituted",
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Disabled => write!(f, "updater disabled: no public key compiled in"),
            UpdateError::Offline(why) => write!(f, "offline: {why}"),
            UpdateError::Http { status } => write!(f, "manifest fetch returned HTTP {status}"),
            UpdateError::ManifestTooLarge { bytes, cap } => {
                write!(f, "manifest is {bytes} bytes, cap {cap}")
            }
            UpdateError::ManifestSignature => write!(f, "manifest signature did not verify"),
            UpdateError::ManifestMalformed(why) => write!(f, "manifest malformed: {why}"),
            UpdateError::BadVersion(v) => write!(f, "version is not semver: {v}"),
            UpdateError::InsecureUrl(u) => write!(f, "refusing insecure url {u}"),
            UpdateError::NoArtifactForPlatform(p) => write!(f, "no artifact for {p}"),
            UpdateError::Digest { expected, actual } => {
                write!(
                    f,
                    "artifact digest mismatch: expected {expected}, got {actual}"
                )
            }
            UpdateError::ArtifactSignature => write!(f, "artifact signature did not verify"),
            UpdateError::SizeExceeded { cap } => write!(f, "artifact exceeded {cap} bytes"),
            UpdateError::SizeMismatch { expected, actual } => {
                write!(f, "artifact size {actual}, manifest said {expected}")
            }
            UpdateError::Cancelled => write!(f, "cancelled"),
            UpdateError::Io(why) => write!(f, "io: {why}"),
            UpdateError::Install(why) => write!(f, "install handoff failed: {why}"),
            UpdateError::Busy => write!(f, "an update step is already running"),
            UpdateError::StagingSymlink(p) => write!(f, "refusing symlink at staging path {p}"),
            UpdateError::StagedTampered => write!(f, "staged artifact changed after verification"),
            UpdateError::StagedSubstituted => {
                write!(f, "staging path no longer names the verified file")
            }
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}

impl From<reqwest::Error> for UpdateError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_redirect() {
            return UpdateError::InsecureUrl(
                e.url()
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "redirect".into()),
            );
        }
        if e.is_connect() || e.is_timeout() || e.is_request() && e.url().is_some() && !e.is_status()
        {
            UpdateError::Offline(e.without_url().to_string())
        } else if let Some(status) = e.status() {
            UpdateError::Http {
                status: status.as_u16(),
            }
        } else {
            UpdateError::Offline(e.without_url().to_string())
        }
    }
}
