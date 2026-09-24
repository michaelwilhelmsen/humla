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

// A sidecar WAV's 16-bit samples as the file holds them, read a chunk at a time
// so an hour-long stream is never in memory twice. It must find the samples
// `data_range` finds.
pub async fn read_i16_mono_16k(path: &std::path::Path) -> anyhow::Result<Vec<i16>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await?;
    let file_len = file.metadata().await?.len();
    let mut head = [0u8; 12];
    file.read_exact(&mut head).await?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        anyhow::bail!("no data chunk");
    }
    let mut at = 12u64;
    let len = loop {
        if at + 8 > file_len {
            anyhow::bail!("no data chunk");
        }
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk).await?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        if &chunk[0..4] == b"data" {
            break size.min(file_len - at - 8);
        }
        at += 8 + size + (size & 1);
        file.seek(std::io::SeekFrom::Start(at)).await?;
    };
    let mut samples = Vec::with_capacity((len / 2) as usize);
    let mut buf = vec![0u8; 1 << 20];
    let mut left = (len & !1) as usize;
    while left > 0 {
        let n = left.min(buf.len());
        file.read_exact(&mut buf[..n]).await?;
        samples.extend(buf[..n].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        left -= n;
    }
    Ok(samples)
}

/// Write a Float32 sample buffer out as 16 kHz mono 16-bit PCM WAV.
/// Used to materialise `playback.wav` for the in-app player. Saturates
/// at ±1.0 to avoid clip wrap-around when summing the two streams.
pub async fn write_pcm16_mono_16k(
    path: &std::path::Path,
    samples: &[f32],
) -> anyhow::Result<()> {
    let mut out = pcm16_header(samples.len());
    out.reserve(samples.len() * 2);
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let pcm = (clamped * 32767.0) as i16;
        out.extend_from_slice(&pcm.to_le_bytes());
    }
    tokio::fs::write(path, out).await?;
    Ok(())
}

/// Write 16-bit samples out as a 16 kHz mono PCM WAV, a chunk at a time.
pub async fn write_i16_mono_16k(path: &std::path::Path, samples: &[i16]) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(&pcm16_header(samples.len())).await?;
    let mut bytes = Vec::with_capacity(1 << 20);
    for chunk in samples.chunks(1 << 19) {
        bytes.clear();
        bytes.extend(chunk.iter().flat_map(|s| s.to_le_bytes()));
        file.write_all(&bytes).await?;
    }
    // Before the file is handed to another process to read.
    file.flush().await?;
    Ok(())
}

/// The 44-byte header of a 16 kHz mono 16-bit PCM WAV of `samples` samples.
fn pcm16_header(samples: usize) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;
    let byte_rate = SAMPLE_RATE * (CHANNELS as u32) * (BITS as u32 / 8);
    let block_align = CHANNELS * BITS / 8;
    let data_len = (samples * 2) as u32;
    let riff_size = 36u32.saturating_add(data_len);

    let mut out = Vec::with_capacity(44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV laid out as the sidecar's are, JUNK and FLLR chunks around `fmt `,
    /// whose `data` chunk declares `declared` bytes and holds `data`.
    fn sidecar_wav(data: &[u8], declared: u32) -> Vec<u8> {
        let mut out = b"RIFF\0\0\0\0WAVEJUNK".to_vec();
        out.extend_from_slice(&28u32.to_le_bytes());
        out.extend_from_slice(&[0; 28]);
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&16_000u32.to_le_bytes());
        out.extend_from_slice(&32_000u32.to_le_bytes());
        for v in [2u16, 16] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(b"FLLR");
        out.extend_from_slice(&4008u32.to_le_bytes());
        out.extend_from_slice(&[0; 4008]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&declared.to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    #[tokio::test]
    async fn reads_a_sidecar_wav_as_its_16_bit_samples() {
        let dir = tempfile::tempdir().unwrap();
        // Several reads' worth, every value an i16 can hold.
        let samples: Vec<i16> = (0..3 * 65_536 + 123).map(|i| (i % 65_536 - 32_768) as i16).collect();
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let n = data.len() as u32;
        // As written; a stream whose header was never finalized; a header
        // that undercounts; an odd byte left over.
        for (bytes, want) in [
            (sidecar_wav(&data, n), samples.len()),
            (sidecar_wav(&data, u32::MAX), samples.len()),
            (sidecar_wav(&data, n - 1000), samples.len() - 500),
            (sidecar_wav(&data[..data.len() - 1], n), samples.len() - 1),
        ] {
            let path = dir.path().join("mic.wav");
            std::fs::write(&path, bytes).unwrap();
            let read = read_i16_mono_16k(&path).await.unwrap();
            assert_eq!(read.len(), want);
            assert!(read == samples[..want], "the samples as the file holds them");
            let floats = read_f32_mono_16k(&path).await.unwrap();
            assert!(floats.iter().zip(&read).all(|(&f, &s)| f == s as f32 / 32768.0));
        }
        std::fs::write(dir.path().join("not.wav"), b"RIFF\0\0\0\0WAVEfmt ").unwrap();
        assert!(read_i16_mono_16k(&dir.path().join("not.wav")).await.is_err());
    }

    #[tokio::test]
    async fn the_16_bit_writer_writes_what_the_float_writer_does() {
        let dir = tempfile::tempdir().unwrap();
        let floats: Vec<f32> = (0..200_001).map(|i| ((i as f32) * 0.001).sin() * 1.2).collect();
        let ints: Vec<i16> = floats.iter().map(|&v| echo_probe::sample::pcm16(v)).collect();
        let (a, b) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
        write_pcm16_mono_16k(&a, &floats).await.unwrap();
        write_i16_mono_16k(&b, &ints).await.unwrap();
        assert!(std::fs::read(&a).unwrap() == std::fs::read(&b).unwrap());
        assert!(read_i16_mono_16k(&b).await.unwrap() == ints);
    }
}
