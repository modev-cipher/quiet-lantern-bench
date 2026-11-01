//! The device seam. Everything above this trait is device-free and testable
//! without a sound server, which is what makes the VA1 acceptance split possible:
//! components are proven in CI with synthetic buffers, the real-device loopback is
//! the manual rig.

use std::fmt;

/// Format of one direction of a device stream. Mono is not assumed: hosts routinely
/// refuse a 1-channel output, so the backend reports what it opened and the
/// [`PlaybackSource`] seam stays mono while the backend fans out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl StreamFormat {
    pub const fn mono(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: 1,
        }
    }

    pub const fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    NoDevice,
    UnsupportedFormat(String),
    Unavailable(String),
    PermissionDenied,
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no audio device is available"),
            AudioError::UnsupportedFormat(detail) => {
                write!(f, "the device cannot use this audio format ({detail})")
            }
            AudioError::Unavailable(detail) => {
                write!(f, "the audio device could not be opened ({detail})")
            }
            AudioError::PermissionDenied => {
                write!(f, "microphone access is not permitted")
            }
        }
    }
}

impl std::error::Error for AudioError {}

/// Called from the OS capture callback with mono PCM16 at the device rate.
///
/// Implementations must not block, allocate, lock, or await (VOICE_AGENT §6).
pub trait CaptureSink: Send {
    fn on_capture(&mut self, frames: &[i16]);
}

/// Called from the OS playback callback to fill mono PCM16 at the device rate.
///
/// Returns the number of frames actually written; the backend zero-fills the rest.
/// Same realtime constraints as [`CaptureSink`].
pub trait PlaybackSource: Send {
    fn fill_playback(&mut self, out: &mut [i16]) -> usize;
}

/// A running device stream. Dropping it closes the stream.
pub trait AudioStream: Send {
    fn play(&self) -> Result<(), AudioError>;
    fn pause(&self) -> Result<(), AudioError>;
}

/// The whole cpal surface VA1 depends on, behind one trait.
pub trait AudioBackend: Send + Sync {
    fn default_input_format(&self) -> Result<StreamFormat, AudioError>;
    fn default_output_format(&self) -> Result<StreamFormat, AudioError>;
    fn open_input(
        &self,
        format: StreamFormat,
        sink: Box<dyn CaptureSink>,
    ) -> Result<Box<dyn AudioStream>, AudioError>;
    fn open_output(
        &self,
        format: StreamFormat,
        source: Box<dyn PlaybackSource>,
    ) -> Result<Box<dyn AudioStream>, AudioError>;
}
