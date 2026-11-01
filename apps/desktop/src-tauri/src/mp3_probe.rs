use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SCAN_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp3Probe {
    pub duration_ms: Option<u64>,
    pub seek_exact: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

#[derive(Debug, Clone, Copy)]
struct Header {
    version: Version,
    bitrate_kbps: u32,
    sample_rate: u32,
    mono: bool,
}

impl Header {
    fn samples_per_frame(self) -> u64 {
        if self.version == Version::Mpeg1 {
            1152
        } else {
            576
        }
    }

    fn side_info_len(self) -> usize {
        match (self.version == Version::Mpeg1, self.mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        }
    }
}

fn parse_header(bytes: &[u8]) -> Option<Header> {
    let [b0, b1, b2, b3] = *bytes.get(..4)?.first_chunk::<4>()?;
    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let version = match (b1 >> 3) & 0b11 {
        0 => Version::Mpeg25,
        2 => Version::Mpeg2,
        3 => Version::Mpeg1,
        _ => return None,
    };
    if (b1 >> 1) & 0b11 != 0b01 {
        return None;
    }
    let bitrate_index = usize::from(b2 >> 4);
    let rate_index = usize::from((b2 >> 2) & 0b11);
    const MPEG1_KBPS: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const MPEG2_KBPS: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let bitrate_kbps = *match version {
        Version::Mpeg1 => MPEG1_KBPS.get(bitrate_index),
        _ => MPEG2_KBPS.get(bitrate_index),
    }?;
    let sample_rate = *match version {
        Version::Mpeg1 => [44_100, 48_000, 32_000].get(rate_index),
        Version::Mpeg2 => [22_050, 24_000, 16_000].get(rate_index),
        Version::Mpeg25 => [11_025, 12_000, 8_000].get(rate_index),
    }?;
    if bitrate_kbps == 0 {
        return None;
    }
    Some(Header {
        version,
        bitrate_kbps,
        sample_rate,
        mono: (b3 >> 6) == 0b11,
    })
}

fn frame_len(header: Header, raw: [u8; 4]) -> usize {
    let padding = usize::from((raw[2] >> 1) & 1);
    let coefficient: u32 = if header.version == Version::Mpeg1 {
        144
    } else {
        72
    };
    (coefficient * header.bitrate_kbps * 1000 / header.sample_rate) as usize + padding
}

fn untagged_bitrate_varies(stream: &[u8], start: usize) -> bool {
    let mut at = start;
    let mut first: Option<u32> = None;
    while let Some(raw) = stream.get(at..at + 4) {
        let Some(header) = parse_header(raw) else {
            break;
        };
        match first {
            None => first = Some(header.bitrate_kbps),
            Some(rate) if rate != header.bitrate_kbps => return true,
            Some(_) => {}
        }
        let len = frame_len(header, [raw[0], raw[1], raw[2], raw[3]]);
        if len < 4 {
            break;
        }
        at += len;
    }
    false
}

fn id3v2_len(bytes: &[u8]) -> usize {
    match bytes.get(..10) {
        Some(head) if head.starts_with(b"ID3") => {
            let size = head[6..10]
                .iter()
                .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
            let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
            10 + size + footer
        }
        _ => 0,
    }
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let chunk = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
}

fn duration_from_frames(frames: u32, header: Header) -> u64 {
    u64::from(frames) * header.samples_per_frame() * 1000 / u64::from(header.sample_rate)
}

pub fn probe_frames(stream: &[u8]) -> Option<Mp3Probe> {
    let start =
        (0..stream.len().saturating_sub(4)).find(|&at| parse_header(&stream[at..]).is_some())?;
    let frame = &stream[start..];
    let header = parse_header(frame)?;
    let tag_at = 4 + header.side_info_len();
    match frame.get(tag_at..tag_at + 4) {
        Some(tag) if tag == b"Xing" || tag == b"Info" => {
            let flags = be_u32(frame, tag_at + 4)?;
            let duration_ms = if flags & 0x1 != 0 {
                be_u32(frame, tag_at + 8).map(|frames| duration_from_frames(frames, header))
            } else {
                None
            };
            return Some(Mp3Probe {
                duration_ms,
                seek_exact: Some(tag == b"Info"),
            });
        }
        _ => {}
    }
    if frame.get(36..40) == Some(b"VBRI") {
        return Some(Mp3Probe {
            duration_ms: be_u32(frame, 36 + 14).map(|frames| duration_from_frames(frames, header)),
            seek_exact: Some(false),
        });
    }
    untagged_bitrate_varies(stream, start).then_some(Mp3Probe {
        duration_ms: None,
        seek_exact: Some(false),
    })
}

pub fn probe_file(path: &Path) -> Option<Mp3Probe> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; 10];
    let read = file.read(&mut head).ok()?;
    let skip = id3v2_len(&head[..read]) as u64;
    file.seek(SeekFrom::Start(skip)).ok()?;
    let mut stream = Vec::with_capacity(SCAN_BYTES);
    file.take(SCAN_BYTES as u64).read_to_end(&mut stream).ok()?;
    probe_frames(&stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MPEG1_128K_44K_STEREO: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];
    const MPEG1_128K_44K_MONO: [u8; 4] = [0xFF, 0xFB, 0x90, 0xC0];
    const MPEG1_160K_44K_STEREO: [u8; 4] = [0xFF, 0xFB, 0xA0, 0x00];

    fn frame_with_tag(header: [u8; 4], side_info: usize, tag: &[u8; 4], frames: u32) -> Vec<u8> {
        let mut bytes = header.to_vec();
        bytes.extend(std::iter::repeat_n(0, side_info));
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&frames.to_be_bytes());
        bytes.resize(417, 0);
        bytes
    }

    #[test]
    fn info_tag_marks_a_constant_bitrate_cast_with_exact_seeking() {
        let frame = frame_with_tag(MPEG1_128K_44K_STEREO, 32, b"Info", 3_828);
        let probe = probe_frames(&frame).unwrap();
        assert_eq!(probe.seek_exact, Some(true));
        assert_eq!(probe.duration_ms, Some(3_828 * 1152 * 1000 / 44_100));
    }

    #[test]
    fn xing_tag_marks_a_variable_bitrate_cast_whose_seeks_drift() {
        let frame = frame_with_tag(MPEG1_128K_44K_MONO, 17, b"Xing", 1_000);
        let probe = probe_frames(&frame).unwrap();
        assert_eq!(probe.seek_exact, Some(false));
        assert_eq!(probe.duration_ms, Some(1_000 * 1152 * 1000 / 44_100));
    }

    fn bare_frame(header: [u8; 4]) -> Vec<u8> {
        let parsed = parse_header(&header).unwrap();
        let mut bytes = header.to_vec();
        bytes.resize(frame_len(parsed, header), 0);
        bytes
    }

    #[test]
    fn untagged_stream_with_one_bitrate_is_left_unknown_rather_than_assumed_constant() {
        let mut stream = Vec::new();
        for _ in 0..6 {
            stream.extend(bare_frame(MPEG1_128K_44K_STEREO));
        }
        assert_eq!(probe_frames(&stream), None);
    }

    #[test]
    fn untagged_stream_whose_bitrate_changes_is_variable_and_its_length_unknown() {
        let mut stream = bare_frame(MPEG1_128K_44K_STEREO);
        stream.extend(bare_frame(MPEG1_160K_44K_STEREO));
        stream.extend(bare_frame(MPEG1_128K_44K_STEREO));
        assert_eq!(
            probe_frames(&stream),
            Some(Mp3Probe {
                duration_ms: None,
                seek_exact: Some(false)
            })
        );
    }

    #[test]
    fn leading_junk_is_skipped_to_the_first_frame() {
        let mut stream = vec![0u8; 7];
        stream.extend(frame_with_tag(MPEG1_128K_44K_STEREO, 32, b"Info", 10));
        assert_eq!(probe_frames(&stream).unwrap().seek_exact, Some(true));
    }

    #[test]
    fn id3v2_length_reads_its_syncsafe_size() {
        let head = [b'I', b'D', b'3', 4, 0, 0, 0, 0, 0x02, 0x01];
        assert_eq!(id3v2_len(&head), 10 + 257);
        assert_eq!(id3v2_len(b"not an id3 tag"), 0);
    }

    #[test]
    fn non_audio_bytes_probe_to_nothing() {
        assert_eq!(probe_frames(b"RIFF....WAVEfmt "), None);
        assert_eq!(probe_frames(&[]), None);
    }

    #[test]
    fn probe_file_reads_past_an_id3_tag() {
        let dir = std::env::temp_dir().join(format!("mp3-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tagged.mp3");
        let mut bytes = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 20];
        bytes.extend(std::iter::repeat_n(0, 20));
        bytes.extend(frame_with_tag(MPEG1_128K_44K_STEREO, 32, b"Info", 50));
        std::fs::write(&path, &bytes).unwrap();
        let probe = probe_file(&path);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            probe,
            Some(Mp3Probe {
                duration_ms: Some(50 * 1152 * 1000 / 44_100),
                seek_exact: Some(true)
            })
        );
    }
}
