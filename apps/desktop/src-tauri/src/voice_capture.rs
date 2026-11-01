// Provenance (AGENTS.md §5a): cpal 0.18 audio input.
// Source: https://docs.rs/cpal/0.18.1/cpal/ (crate source read at
//   ~/.cargo/registry/.../cpal-0.18.1/src/{lib.rs,traits.rs})
// Checked: 2026-08-08. API facts this module depends on, which differ from cpal 0.15:
//   - DeviceTrait has no name(); the device name is its Display impl, and
//     description() returns the structured DeviceDescription.
//   - SampleRate is a plain u32, not a newtype.
//   - build_input_stream takes StreamConfig by value and a timeout argument.
// Host enumeration happens inside the commands, never at startup: on a machine with no
// sound server, touching the host can block or abort, and the app must still open.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const CAPTURE_SAMPLE_RATE: u32 = 48_000;
pub const HARD_STOP_SECONDS: f64 = 60.0;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInput {
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug)]
pub enum CaptureError {
    NoDevice,
    Unavailable(String),
    PermissionDenied,
    AlreadyRecording,
    NotRecording,
    Io(String),
}

const IMPORT_INSTEAD: &str = "You can import an audio file instead.";

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::NoDevice => write!(
                f,
                "txtCaster could not find a microphone on this computer. {IMPORT_INSTEAD}"
            ),
            CaptureError::PermissionDenied => write!(
                f,
                "txtCaster is not allowed to use the microphone. Grant microphone access in \
                 your system settings, then try again. {IMPORT_INSTEAD}"
            ),
            CaptureError::Unavailable(detail) => write!(
                f,
                "The microphone could not be opened ({detail}). Another program may be using \
                 it. {IMPORT_INSTEAD}"
            ),
            CaptureError::AlreadyRecording => {
                write!(f, "A recording is already running. Stop it first.")
            }
            CaptureError::NotRecording => write!(f, "No recording is running."),
            CaptureError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

fn classify(detail: String) -> CaptureError {
    let lowered = detail.to_ascii_lowercase();
    if lowered.contains("permission") || lowered.contains("denied") || lowered.contains("access") {
        CaptureError::PermissionDenied
    } else {
        CaptureError::Unavailable(detail)
    }
}

pub fn list_inputs() -> Result<Vec<AudioInput>, CaptureError> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|device| device.to_string());
    let devices = host
        .input_devices()
        .map_err(|err| classify(err.to_string()))?;
    let mut inputs: Vec<AudioInput> = Vec::new();
    for device in devices {
        let name = device.to_string();
        if name.is_empty() {
            continue;
        }
        let is_default = Some(&name) == default_name.as_ref();
        if inputs.iter().any(|existing| existing.name == name) {
            continue;
        }
        inputs.push(AudioInput { name, is_default });
    }
    if inputs.is_empty() {
        return Err(CaptureError::NoDevice);
    }
    inputs.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    Ok(inputs)
}

type Writer = Arc<Mutex<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>>>;

struct Recording {
    stream: Option<cpal::Stream>,
    writer: Writer,
    overran: Arc<AtomicBool>,
    path: PathBuf,
    sample_id: String,
    generation: u64,
}

impl Recording {
    fn finish(mut self) -> Result<StoppedRecording, CaptureError> {
        self.stream.take();
        let writer = self
            .writer
            .lock()
            .map_err(|_| CaptureError::Io("recording writer lock poisoned".into()))?
            .take();
        if let Some(handle) = writer {
            handle.finalize().map_err(|err| {
                CaptureError::Io(format!("the recording could not be written: {err}"))
            })?;
        }
        Ok(StoppedRecording {
            sample_id: self.sample_id,
            path: self.path,
            hit_hard_stop: self.overran.load(Ordering::Acquire),
        })
    }

    fn abandon(mut self) {
        self.stream.take();
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.finalize();
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedRecording {
    pub sample_id: String,
    pub path: PathBuf,
    pub hit_hard_stop: bool,
}

#[derive(Default)]
pub struct Recorder {
    active: Mutex<Option<Recording>>,
    finished: Mutex<Option<StoppedRecording>>,
    generations: AtomicU64,
}

// SAFETY: cpal::Stream is not Sync on every backend, but the only pointer to it lives inside
// this Mutex and is created, used and dropped through &self methods that take the lock, so no
// two threads ever touch the stream concurrently.
unsafe impl Send for Recorder {}
unsafe impl Sync for Recorder {}

fn pick_device(name: Option<&str>) -> Result<cpal::Device, CaptureError> {
    let host = cpal::default_host();
    match name {
        Some(wanted) => {
            let devices = host
                .input_devices()
                .map_err(|err| classify(err.to_string()))?;
            for device in devices {
                if device.to_string() == wanted {
                    return Ok(device);
                }
            }
            Err(CaptureError::NoDevice)
        }
        None => host.default_input_device().ok_or(CaptureError::NoDevice),
    }
}

impl Recorder {
    pub fn is_recording(&self) -> bool {
        self.active
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    /// Releases the device if the recording identified by `generation` is still live, keeping
    /// whatever was captured so the user can still use it. The generation check is what stops
    /// a watchdog armed for one take from halting the next one.
    pub fn halt(&self, generation: u64) {
        let Ok(mut slot) = self.active.lock() else {
            return;
        };
        if slot.as_ref().map(|r| r.generation) != Some(generation) {
            return;
        }
        let Some(recording) = slot.take() else {
            return;
        };
        drop(slot);
        if let Ok(stopped) = recording.finish() {
            if let Ok(mut done) = self.finished.lock() {
                *done = Some(stopped);
            }
        }
    }

    /// Drops a take the watchdog ended but nobody ever collected. Arming the next recording
    /// is the point at which it becomes unreachable: `stop` answers from this slot first, so
    /// leaving it filled would hand the previous take back in place of the new one.
    fn discard_abandoned(&self) {
        if let Ok(mut done) = self.finished.lock() {
            if let Some(stopped) = done.take() {
                let _ = std::fs::remove_file(&stopped.path);
            }
        }
    }

    #[cfg(test)]
    fn arm_for_test(&self, path: &Path, sample_id: &str, generation: u64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let writer = hound::WavWriter::create(path, spec).expect("writer");
        let mut slot = self.active.lock().expect("lock");
        assert!(slot.is_none(), "arming over a live recording");
        self.discard_abandoned();
        *slot = Some(Recording {
            stream: None,
            writer: Arc::new(Mutex::new(Some(writer))),
            overran: Arc::new(AtomicBool::new(true)),
            path: path.to_path_buf(),
            sample_id: sample_id.to_string(),
            generation,
        });
    }

    pub fn start(
        self: &Arc<Self>,
        target: &Path,
        sample_id: &str,
        device_name: Option<&str>,
    ) -> Result<(), CaptureError> {
        let mut slot = self
            .active
            .lock()
            .map_err(|_| CaptureError::Io("recorder lock poisoned".into()))?;
        if slot.is_some() {
            return Err(CaptureError::AlreadyRecording);
        }
        self.discard_abandoned();
        let device = pick_device(device_name)?;
        let config = device
            .default_input_config()
            .map_err(|err| classify(err.to_string()))?;
        let channels = config.channels().max(1);
        let sample_rate = config.sample_rate();
        let sample_format = config.sample_format();

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                CaptureError::Io(format!("could not create {}: {err}", parent.display()))
            })?;
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(target, spec)
            .map_err(|err| CaptureError::Io(format!("could not open the recording file: {err}")))?;
        let writer = Arc::new(Mutex::new(Some(writer)));
        let overran = Arc::new(AtomicBool::new(false));
        let limit = (f64::from(sample_rate) * HARD_STOP_SECONDS) as u64;

        let sink = writer.clone();
        let overran_sink = overran.clone();
        let mut written: u64 = 0;
        let mut write_mono = move |mono: &[i16]| {
            let Ok(mut guard) = sink.lock() else {
                return;
            };
            let Some(handle) = guard.as_mut() else {
                return;
            };
            for sample in mono {
                if written >= limit {
                    overran_sink.store(true, Ordering::Release);
                    return;
                }
                if handle.write_sample(*sample).is_err() {
                    return;
                }
                written += 1;
            }
        };

        let err_fn = |err| tracing::warn!("voice capture stream error: {err}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                config.into(),
                move |data: &[f32], _: &_| {
                    let mono = downmix_f32(data, channels);
                    write_mono(&mono);
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &_| {
                    let mono = downmix_i16(data, channels);
                    write_mono(&mono);
                },
                err_fn,
                None,
            ),
            other => {
                return Err(CaptureError::Unavailable(format!(
                    "unsupported sample format {other:?}"
                )))
            }
        }
        .map_err(|err| classify(err.to_string()))?;
        stream.play().map_err(|err| classify(err.to_string()))?;

        let generation = self.generations.fetch_add(1, Ordering::AcqRel) + 1;
        *slot = Some(Recording {
            stream: Some(stream),
            writer,
            overran,
            path: target.to_path_buf(),
            sample_id: sample_id.to_string(),
            generation,
        });
        drop(slot);

        // The device must be released even if the webview never sends stop or cancel: a
        // dialog the user closed must not leave the microphone open for the life of the
        // process. This is the backstop; the UI still cancels on unmount.
        let watchdog = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(HARD_STOP_SECONDS + 1.0));
            watchdog.halt(generation);
        });
        Ok(())
    }

    pub fn stop(&self) -> Result<StoppedRecording, CaptureError> {
        if let Ok(mut done) = self.finished.lock() {
            if let Some(stopped) = done.take() {
                return Ok(stopped);
            }
        }
        let mut slot = self
            .active
            .lock()
            .map_err(|_| CaptureError::Io("recorder lock poisoned".into()))?;
        let recording = slot.take().ok_or(CaptureError::NotRecording)?;
        drop(slot);
        recording.finish()
    }

    pub fn cancel(&self) {
        if let Ok(mut done) = self.finished.lock() {
            if let Some(stopped) = done.take() {
                let _ = std::fs::remove_file(&stopped.path);
            }
        }
        let Ok(mut slot) = self.active.lock() else {
            return;
        };
        let Some(recording) = slot.take() else {
            return;
        };
        drop(slot);
        recording.abandon();
    }
}

pub fn downmix_f32(data: &[f32], channels: u16) -> Vec<i16> {
    let channels = usize::from(channels.max(1));
    data.chunks(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().copied().sum();
            let mean = sum / frame.len() as f32;
            (mean.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
        })
        .collect()
}

pub fn downmix_i16(data: &[i16], channels: u16) -> Vec<i16> {
    let channels = usize::from(channels.max(1));
    data.chunks(channels)
        .map(|frame| {
            let sum: i32 = frame.iter().map(|s| i32::from(*s)).sum();
            (sum / frame.len() as i32) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("txtcaster-rec-{}", crate::storage::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_watchdog_releases_the_device_and_keeps_what_was_captured() {
        let dir = tempdir();
        let recorder = Recorder::default();
        let path = dir.join("take.wav");
        recorder.arm_for_test(&path, "sample-1", 7);
        assert!(recorder.is_recording());

        recorder.halt(7);

        assert!(
            !recorder.is_recording(),
            "the microphone was never released"
        );
        let stopped = recorder.stop().expect("the capture survives the watchdog");
        assert_eq!(stopped.sample_id, "sample-1");
        assert!(stopped.hit_hard_stop);
        assert!(
            path.is_file(),
            "the audio the user recorded was thrown away"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_watchdog_armed_for_an_earlier_take_never_halts_the_next_one() {
        let dir = tempdir();
        let recorder = Recorder::default();
        recorder.arm_for_test(&dir.join("second.wav"), "sample-2", 9);

        recorder.halt(8);

        assert!(
            recorder.is_recording(),
            "a stale watchdog cut the recording the user had just started"
        );
        assert_eq!(recorder.stop().expect("stop").sample_id, "sample-2");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_take_the_watchdog_ended_never_answers_for_the_next_one() {
        let dir = tempdir();
        let recorder = Recorder::default();
        let abandoned = dir.join("first.wav");
        recorder.arm_for_test(&abandoned, "sample-1", 1);
        recorder.halt(1);

        recorder.arm_for_test(&dir.join("second.wav"), "sample-2", 2);

        assert_eq!(
            recorder.stop().expect("stop").sample_id,
            "sample-2",
            "an abandoned take was handed back in place of the one just recorded"
        );
        assert!(!abandoned.exists(), "the abandoned take was left on disk");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn halting_twice_is_harmless() {
        let dir = tempdir();
        let recorder = Recorder::default();
        recorder.arm_for_test(&dir.join("take.wav"), "sample-1", 1);
        recorder.halt(1);
        recorder.halt(1);
        assert_eq!(recorder.stop().expect("stop").sample_id, "sample-1");
        assert!(matches!(recorder.stop(), Err(CaptureError::NotRecording)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelling_after_the_watchdog_still_removes_the_file() {
        let dir = tempdir();
        let recorder = Recorder::default();
        let path = dir.join("take.wav");
        recorder.arm_for_test(&path, "sample-1", 1);
        recorder.halt(1);
        recorder.cancel();
        assert!(!path.exists(), "an abandoned recording was left on disk");
        assert!(matches!(recorder.stop(), Err(CaptureError::NotRecording)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelling_a_live_recording_removes_its_file() {
        let dir = tempdir();
        let recorder = Recorder::default();
        let path = dir.join("take.wav");
        recorder.arm_for_test(&path, "sample-1", 1);
        recorder.cancel();
        assert!(!recorder.is_recording());
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stopping_without_starting_says_so() {
        let recorder = Recorder::default();
        assert!(!recorder.is_recording());
        assert!(matches!(recorder.stop(), Err(CaptureError::NotRecording)));
    }

    #[test]
    fn cancelling_without_starting_is_a_no_op() {
        Recorder::default().cancel();
    }

    #[test]
    fn every_capture_failure_offers_the_import_route_out() {
        for error in [
            CaptureError::NoDevice,
            CaptureError::PermissionDenied,
            CaptureError::Unavailable("device busy".into()),
        ] {
            let message = error.to_string();
            assert!(
                message.contains("import an audio file instead"),
                "a dead end with no way forward: {message}"
            );
            assert!(
                !message.contains("cpal") && !message.contains("Err"),
                "library jargon leaked into user copy: {message}"
            );
        }
    }

    #[test]
    fn a_denied_device_is_reported_as_permission_not_as_a_generic_fault() {
        assert!(matches!(
            classify("Permission denied (os error 13)".into()),
            CaptureError::PermissionDenied
        ));
        assert!(matches!(
            classify("Access is denied".into()),
            CaptureError::PermissionDenied
        ));
        assert!(matches!(
            classify("device or resource busy".into()),
            CaptureError::Unavailable(_)
        ));
    }

    #[test]
    fn downmix_averages_the_channels_it_is_given() {
        assert_eq!(downmix_i16(&[100, 300, -100, -300], 2), vec![200, -200]);
        assert_eq!(downmix_i16(&[100, 200], 1), vec![100, 200]);
        let mono = downmix_f32(&[1.0, 0.0], 2);
        assert_eq!(mono.len(), 1);
        assert!((mono[0] - i16::MAX / 2).abs() <= 1);
    }

    #[test]
    fn downmix_clamps_rather_than_wrapping_on_a_hot_signal() {
        assert_eq!(downmix_f32(&[9.0], 1), vec![i16::MAX]);
        assert_eq!(downmix_f32(&[-9.0], 1), vec![-i16::MAX]);
    }

    #[test]
    fn a_ragged_final_frame_does_not_divide_by_zero() {
        assert_eq!(downmix_i16(&[100, 200, 300], 2), vec![150, 300]);
        assert!(downmix_i16(&[], 2).is_empty());
    }
}
