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
        samples.extend(buf[..n].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes([b[0], b[1]])));
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

/// Where a 16 kHz mono 16-bit PCM WAV keeps its samples: the `data` chunk's
/// byte offset and its length in whole samples' bytes, clamped to the file as
/// `read_i16_mono_16k` clamps it. Any other format is refused, because callers
/// copy these bytes out as they are rather than decoding them.
pub fn pcm16_mono_16k_data(path: &std::path::Path) -> anyhow::Result<(u64, u64)> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut head = [0u8; 12];
    file.read_exact(&mut head)?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        anyhow::bail!("not a WAV file");
    }
    let mut format_ok = false;
    let mut at = 12u64;
    loop {
        if at + 8 > file_len {
            anyhow::bail!("no data chunk");
        }
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        match &chunk[0..4] {
            b"fmt " if size >= 16 => {
                let mut fmt = [0u8; 16];
                file.read_exact(&mut fmt)?;
                let tag = u16::from_le_bytes([fmt[0], fmt[1]]);
                let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                // 0xFFFE is WAVE_FORMAT_EXTENSIBLE, which AVAudioFile may write
                // for the same 16-bit integer samples.
                format_ok = (tag == 1 || tag == 0xFFFE) && channels == 1 && rate == 16_000 && bits == 16;
            }
            b"data" => {
                if !format_ok {
                    anyhow::bail!("not 16 kHz mono 16-bit PCM");
                }
                let payload = at + 8;
                return Ok((payload, size.min(file_len - payload) & !1));
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
        file.seek(std::io::SeekFrom::Start(at))?;
    }
}

/// Join 16 kHz mono 16-bit PCM WAVs end to end into one at `out`, copying
/// each file's samples through without decoding them, so an hour of audio is
/// never in memory. Returns the samples written.
pub fn concat_pcm16_mono_16k(
    inputs: &[std::path::PathBuf],
    out: &std::path::Path,
) -> anyhow::Result<u64> {
    use std::io::{Read, Seek, Write};
    let ranges = inputs
        .iter()
        .map(|p| pcm16_mono_16k_data(p).map(|r| (p, r)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let bytes: u64 = ranges.iter().map(|(_, (_, len))| len).sum();
    // A WAV's sizes are 32-bit: about 37 hours at this rate.
    if bytes + 36 > u32::MAX as u64 {
        anyhow::bail!("too much audio for one WAV file");
    }
    let mut writer = std::io::BufWriter::new(std::fs::File::create(out)?);
    writer.write_all(&pcm16_header((bytes / 2) as usize))?;
    for (path, (offset, len)) in ranges {
        let mut file = std::fs::File::open(path)?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        let copied = std::io::copy(&mut file.take(len), &mut writer)?;
        if copied != len {
            anyhow::bail!("{} ended early", path.display());
        }
    }
    writer.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(bytes / 2)
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

    #[tokio::test]
    async fn joins_takes_end_to_end_as_the_files_hold_them() {
        let dir = tempfile::tempdir().unwrap();
        let first: Vec<i16> = (0..70_001).map(|i| (i % 65_536 - 32_768) as i16).collect();
        let second: Vec<i16> = (0..1_234).map(|i| (i * 7) as i16).collect();
        let (a, b, out) = (dir.path().join("a.wav"), dir.path().join("b.wav"), dir.path().join("out.wav"));
        // One as the sidecar writes them, header never finalized, one as
        // `write_i16_mono_16k` does.
        let data: Vec<u8> = first.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&a, sidecar_wav(&data, u32::MAX)).unwrap();
        write_i16_mono_16k(&b, &second).await.unwrap();

        let written = concat_pcm16_mono_16k(&[a.clone(), b.clone()], &out).unwrap();
        assert_eq!(written, (first.len() + second.len()) as u64);
        let joined = read_i16_mono_16k(&out).await.unwrap();
        assert!(joined == [first, second].concat());
        // A plain 44-byte header the player and any other app can read.
        assert_eq!(pcm16_mono_16k_data(&out).unwrap(), (44, written * 2));
    }

    #[test]
    fn refuses_to_join_a_wav_of_another_format() {
        let dir = tempfile::tempdir().unwrap();
        let mut stereo = sidecar_wav(&[0; 8], 8);
        // `fmt ` sits after RIFF/WAVE (12) and the 36-byte JUNK chunk; its
        // channel count is 2 bytes past its 8-byte header and format tag.
        let channels = 12 + 36 + 8 + 2;
        stereo[channels..channels + 2].copy_from_slice(&2u16.to_le_bytes());
        let path = dir.path().join("stereo.wav");
        std::fs::write(&path, stereo).unwrap();
        assert!(pcm16_mono_16k_data(&path).is_err());
        assert!(concat_pcm16_mono_16k(&[path], &dir.path().join("out.wav")).is_err());
        std::fs::write(dir.path().join("not.wav"), b"RIFF\0\0\0\0WAVEfmt ").unwrap();
        assert!(pcm16_mono_16k_data(&dir.path().join("not.wav")).is_err());
    }
}
