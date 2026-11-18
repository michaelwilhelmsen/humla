//! Writing a take's playback assets: the mixed, levelled audio encoded as AAC
//! (only with `keep_audio` on) and the `timeline.jsonl` the reader is built from.

use super::capture::session_write_dir;
use super::keep_audio_enabled;

/// Whether a playback write may include audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaybackAudio {
    /// As the `keep_audio` gate says when the write happens.
    FollowGate,
    /// Never: the caller already decided this take keeps no audio.
    Never,
}
use crate::wav;
use tauri::AppHandle;

/// Build the playback-asset bundle for a recording: the mixed playback audio
/// plus a `timeline.jsonl` mapping each speaker turn back to its first-chunk
/// start_ms, so the frontend can render the transcript with chunk spans and
/// highlight whichever one matches the audio's current position.
///
/// The playback audio is gated on `keep_audio` (#24) — it is a full recording
/// of the meeting, so writing it while the setting said "keep nothing" was the
/// dishonesty that issue names. `timeline.jsonl` is written either way: it is
/// text (word timings), and the merged reader and session dividers are built
/// from it whether or not there is anything to play.
pub(super) async fn write_playback_assets(
    app: &AppHandle,
    note_id: &str,
    session_id: &str,
    timeline: String,
    mic_wav: Option<&std::path::Path>,
    sys_wav: Option<&std::path::Path>,
    audio: PlaybackAudio,
) {
    if mic_wav.is_none() && sys_wav.is_none() && timeline.is_empty() {
        return;
    }
    let Some(target) = session_write_dir(app, note_id, session_id).await else {
        eprintln!("playback: session dir unavailable");
        return;
    };

    if audio == PlaybackAudio::FollowGate && keep_audio_enabled(app, note_id) {
        if let Err(e) = build_playback(mic_wav, sys_wav, &target).await {
            eprintln!("playback: build_playback: {e}");
        }
    }

    if !timeline.is_empty() {
        if let Err(e) = tokio::fs::write(target.join("timeline.jsonl"), timeline).await {
            eprintln!("playback: write timeline.jsonl: {e}");
        }
    }
}

/// AAC at this rate is about 0.24 MB a minute for 16 kHz mono speech, against
/// 1.9 MB for the WAV it replaces (#226).
const PLAYBACK_AAC_BITRATE: &str = "32000";

/// Write a take's playback audio into `session_dir`: the per-source full WAVs
/// mixed and levelled (`playback_mix`), encoded as `playback.m4a`. A take
/// holds one playback file, so the other format is removed once this one is
/// in place. If the encoder fails, the take keeps a `playback.wav` instead.
async fn build_playback(
    mic: Option<&std::path::Path>,
    sys: Option<&std::path::Path>,
    session_dir: &std::path::Path,
) -> anyhow::Result<()> {
    if mic.is_none() && sys.is_none() {
        return Ok(());
    }
    let mic_samples = match mic {
        Some(m) => wav::read_f32_mono_16k(m).await?,
        None => Vec::new(),
    };
    let sys_samples = match sys {
        Some(s) => wav::read_f32_mono_16k(s).await?,
        None => Vec::new(),
    };
    let mixed = tokio::task::spawn_blocking(move || {
        let mut mixed = crate::playback_mix::mix(mic_samples, sys_samples);
        crate::playback_mix::level(&mut mixed);
        mixed
    })
    .await?;

    let wav_out = session_dir.join(".playback.wav.partial");
    let m4a_out = session_dir.join(".playback.m4a.partial");
    wav::write_pcm16_mono_16k(&wav_out, &mixed).await?;
    drop(mixed);
    let (keep, from, drop_other) = match encode_aac(&wav_out, &m4a_out).await {
        Ok(()) => ("playback.m4a", &m4a_out, "playback.wav"),
        Err(e) => {
            eprintln!("playback: AAC encode failed, keeping WAV: {e}");
            ("playback.wav", &wav_out, "playback.m4a")
        }
    };
    let result = tokio::fs::rename(from, session_dir.join(keep)).await;
    let _ = tokio::fs::remove_file(&wav_out).await;
    let _ = tokio::fs::remove_file(&m4a_out).await;
    result?;
    let _ = tokio::fs::remove_file(session_dir.join(drop_other)).await;
    Ok(())
}

async fn encode_aac(wav: &std::path::Path, m4a: &std::path::Path) -> anyhow::Result<()> {
    let out = tokio::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac", "-b", PLAYBACK_AAC_BITRATE])
        .arg(wav)
        .arg(m4a)
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("afconvert: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod playback_tests {
    use super::*;

    // Runs `/usr/bin/afconvert`, which only macOS has.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_take_s_playback_is_written_as_aac_and_replaces_its_wav() {
        let dir = tempfile::tempdir().unwrap();
        let mic = dir.path().join("mic-full.wav");
        let tone: Vec<i16> = (0..32_000).map(|i| ((i as f32 * 0.05).sin() * 3_000.0) as i16).collect();
        wav::write_i16_mono_16k(&mic, &tone).await.unwrap();
        let take = dir.path().join("take");
        std::fs::create_dir_all(&take).unwrap();
        std::fs::write(take.join("playback.wav"), b"an older take's playback").unwrap();

        build_playback(Some(&mic), None, &take).await.unwrap();

        let mut names: Vec<String> = std::fs::read_dir(&take)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["playback.m4a"], "one playback file, no partials left");
        let decoded = dir.path().join("decoded.wav");
        let status = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000"])
            .arg(take.join("playback.m4a"))
            .arg(&decoded)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(wav::read_i16_mono_16k(&decoded).await.unwrap().len(), tone.len());
    }

    #[tokio::test]
    async fn no_streams_writes_no_playback() {
        let dir = tempfile::tempdir().unwrap();
        build_playback(None, None, dir.path()).await.unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
