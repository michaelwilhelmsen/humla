//! Just enough WAV to read what Humla writes (16 kHz mono PCM16 from the
//! sidecar and the playback mix) and what anyone else might hand this tool
//! (other rates, float, several channels), and to write PCM16 back.
//!
//! The RIFF walk is the app's (`src-tauri/src/wav.rs`), for the same reason:
//! AVAudioFile pads its header with FLLR chunks. It differs in one way on
//! purpose — a `data` chunk whose declared size disagrees with the file is
//! reported instead of trusted. A file AVAudioFile never closed keeps the size
//! it was created with, which is what a stream rewritten after its writer
//! closed would look like (see `FullRecordingWriter` in the sidecar).

use std::path::Path;

pub struct Wav {
    pub rate: u32,
    /// Channels in the file; `samples` is their average.
    pub channels: u16,
    pub samples: Vec<f32>,
    /// The `data` chunk's declared length differs from what the file holds.
    pub size_mismatch: bool,
}

impl Wav {
    pub fn duration_s(&self) -> f64 {
        self.samples.len() as f64 / self.rate as f64
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

pub fn read(path: &Path) -> Result<Wav, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(bytes: &[u8]) -> Result<Wav, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (tag, channels, rate, bits)
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32_at(bytes, i + 4) as usize;
        let payload = i + 8;
        if id == b"fmt " && payload + 16 <= bytes.len() {
            let mut tag = u16_at(bytes, payload);
            let channels = u16_at(bytes, payload + 2);
            let rate = u32_at(bytes, payload + 4);
            let bits = u16_at(bytes, payload + 14);
            // WAVE_FORMAT_EXTENSIBLE: the real tag leads the sub-format GUID.
            if tag == 0xFFFE && size >= 26 && payload + 26 <= bytes.len() {
                tag = u16_at(bytes, payload + 24);
            }
            fmt = Some((tag, channels, rate, bits));
        }
        if id == b"data" {
            let (tag, channels, rate, bits) = fmt.ok_or("data chunk before fmt chunk")?;
            let available = bytes.len() - payload;
            let size_mismatch = size != available && size + 1 != available;
            let len = if size == 0 || size > available { available } else { size };
            let samples = decode(&bytes[payload..payload + len], tag, channels, bits)?;
            return Ok(Wav { rate, channels, samples, size_mismatch });
        }
        i = payload + size + (size & 1);
    }
    Err("no data chunk".into())
}

fn decode(data: &[u8], tag: u16, channels: u16, bits: u16) -> Result<Vec<f32>, String> {
    let ch = channels.max(1) as usize;
    let width = (bits as usize).div_ceil(8);
    if width == 0 {
        return Err("zero-width samples".into());
    }
    let sample = |b: &[u8]| -> Result<f32, String> {
        Ok(match (tag, bits) {
            (1, 16) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            (1, 8) => (b[0] as f32 - 128.0) / 128.0,
            (3, 32) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (3, 64) => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
            _ => return Err(format!("unsupported WAV format (tag {tag}, {bits} bits)")),
        })
    };
    let frame = width * ch;
    let frames = data.len() / frame;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0f32;
        for c in 0..ch {
            let at = f * frame + c * width;
            acc += sample(&data[at..at + width])?;
        }
        out.push(acc / ch as f32);
    }
    Ok(out)
}

/// Write mono 16-bit PCM, saturating at ±1.
pub fn write_pcm16(path: &Path, rate: u32, samples: &[f32]) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_round_trips() {
        let dir = std::env::temp_dir().join(format!("echo-probe-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let x: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        write_pcm16(&path, 16_000, &x).unwrap();
        let w = read(&path).unwrap();
        assert_eq!((w.rate, w.channels, w.samples.len()), (16_000, 1, 1000));
        assert!(!w.size_mismatch);
        for (a, b) in w.samples.iter().zip(&x) {
            assert!((a - b).abs() < 1.0 / 16_000.0);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_header_that_undercounts_its_data_is_flagged_and_read_in_full() {
        // A file that was never closed keeps the size it was created with.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        for v in [2u16, 16] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 200]);
        let w = parse(&bytes).unwrap();
        assert!(w.size_mismatch);
        assert_eq!(w.samples.len(), 100);
    }
}
