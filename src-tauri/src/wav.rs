// Minimal WAV reader for the chunk files produced by audio-capture.
// AVAudioFile may insert FLLR/fact padding chunks, so we walk the RIFF
// structure rather than assuming a fixed 44-byte header.

pub fn data_range(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let payload = i + 8;
        if id == b"data" {
            let end = payload.saturating_add(size).min(bytes.len());
            return Some((payload, end - payload));
        }
        i = payload + size + (size & 1);
    }
    None
}

// RMS amplitude in [0, 1] for 16-bit PCM little-endian audio.
pub async fn rms(path: &std::path::Path) -> anyhow::Result<f32> {
    let bytes = tokio::fs::read(path).await?;
    let (off, len) = data_range(&bytes).ok_or_else(|| anyhow::anyhow!("no data chunk"))?;
    let data = &bytes[off..off + len];
    let n = data.len() / 2;
    if n == 0 {
        return Ok(0.0);
    }
    let mut sum_sq: f64 = 0.0;
    for i in 0..n {
        let s = i16::from_le_bytes([data[i * 2], data[i * 2 + 1]]) as f64 / 32768.0;
        sum_sq += s * s;
    }
    Ok((sum_sq / n as f64).sqrt() as f32)
}

// Whole-file 16-bit PCM → f32 in [-1, 1]. The chunk WAVs are already
// 16 kHz mono, which is exactly what whisper.cpp expects.
pub async fn read_f32_mono_16k(path: &std::path::Path) -> anyhow::Result<Vec<f32>> {
    let bytes = tokio::fs::read(path).await?;
    let (off, len) = data_range(&bytes).ok_or_else(|| anyhow::anyhow!("no data chunk"))?;
    let data = &bytes[off..off + len];
    let n = data.len() / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = i16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
        out.push(s as f32 / 32768.0);
    }
    Ok(out)
}
