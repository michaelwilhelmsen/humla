//! macOS permission commands: the microphone, screen recording and system audio
//! recording through the audio-capture sidecar, notifications in this process
//! (`crate::notifications`), plus a deep-link into System Settings.

use std::process::Stdio;
use tauri::AppHandle;

use crate::notifications;

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct PermissionsStatus {
    pub microphone: String,
    pub screen: String,
    /// System Audio Recording, which a process tap needs: `unsupported` before
    /// macOS 14.2, `unknown` from a sidecar that doesn't report it.
    #[serde(rename = "systemAudio", default = "unknown")]
    pub system_audio: String,
    /// Not in the sidecar's answer.
    #[serde(default)]
    pub notifications: String,
}

fn unknown() -> String {
    "unknown".into()
}

/// Where a recording's system audio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemAudioSource {
    ProcessTap,
    ScreenCaptureKit,
    /// The microphone only.
    Off,
}

impl SystemAudioSource {
    /// The sidecar's `--sys-source` value.
    pub(crate) fn arg(self) -> &'static str {
        match self {
            Self::ProcessTap => "tap",
            Self::ScreenCaptureKit => "sck",
            Self::Off => "none",
        }
    }
}

/// The process tap once System Audio Recording is granted, or when it has never
/// been asked and Screen Recording isn't granted either (macOS asks at the first
/// recording). A user who only granted Screen Recording keeps ScreenCaptureKit
/// until they allow system audio, so nobody is asked mid-meeting. A denied tap
/// records silence rather than failing, so it is never started; with Screen
/// Recording also refused there is nothing to record the far side with.
pub(crate) fn system_audio_source(p: &PermissionsStatus) -> SystemAudioSource {
    match (p.system_audio.as_str(), p.screen.as_str()) {
        ("granted", _) => SystemAudioSource::ProcessTap,
        ("not_determined", screen) if screen != "granted" => SystemAudioSource::ProcessTap,
        ("denied", "denied") => SystemAudioSource::Off,
        _ => SystemAudioSource::ScreenCaptureKit,
    }
}

/// What to tell the user when a recording will only capture their microphone.
pub(crate) fn far_side_warning(p: &PermissionsStatus) -> Option<&'static str> {
    match system_audio_source(p) {
        SystemAudioSource::ProcessTap => None,
        SystemAudioSource::Off => Some(
            "System audio not allowed — only your microphone will be captured. Allow it in Settings → Permissions for the full meeting transcript.",
        ),
        SystemAudioSource::ScreenCaptureKit if p.screen != "granted" => Some(
            "Screen Recording not granted — only your microphone will be captured. Grant in Settings → Permissions and restart for the full meeting transcript.",
        ),
        SystemAudioSource::ScreenCaptureKit => None,
    }
}

async fn run_sidecar_cmd(app: &AppHandle, mode: &str) -> Result<String, String> {
    run_sidecar_cmd_for(app, mode, std::time::Duration::from_secs(3)).await
}

async fn run_sidecar_cmd_for(
    app: &AppHandle,
    mode: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let path = super::sidecar_path(app)?;
    let child = tokio::process::Command::new(&path)
        .arg(mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let fut = child.wait_with_output();
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(out)) => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
        Ok(Err(e)) => Err(format!("read: {e}")),
        Err(_) => Err("sidecar timed out".into()),
    }
}

/// The sidecar's answer, without notifications: what `recording_start` checks
/// and chooses its system-audio source from.
pub(crate) async fn capture_permissions(app: &AppHandle) -> Result<PermissionsStatus, String> {
    let stdout = run_sidecar_cmd(app, "status").await?;
    let line = stdout.lines().last().unwrap_or("");
    serde_json::from_str(line).map_err(|e| format!("parse: {e}: {line}"))
}

/// A failed sidecar reads as `unknown` for its two, so the notifications
/// answer still arrives.
#[tauri::command]
pub async fn permissions_status(app: AppHandle) -> Result<PermissionsStatus, String> {
    let mut status = capture_permissions(&app).await.unwrap_or_else(|_| PermissionsStatus {
        microphone: unknown(),
        screen: unknown(),
        system_audio: unknown(),
        ..Default::default()
    });
    status.notifications = notifications::status().await.label().to_string();
    Ok(status)
}

#[tauri::command]
pub async fn permissions_request(app: AppHandle, kind: String) -> Result<PermissionsStatus, String> {
    let mode = match kind.as_str() {
        "microphone" => "request-microphone",
        "screen" => "request-screen",
        // macOS's prompt waits for the user's answer.
        "systemAudio" => {
            let _ = run_sidecar_cmd_for(&app, "request-system-audio", std::time::Duration::from_secs(120)).await;
            return permissions_status(app).await;
        }
        "notifications" => {
            notifications::request().await;
            return permissions_status(app).await;
        }
        _ => return Err("unknown kind".into()),
    };
    let _ = run_sidecar_cmd(&app, mode).await; // result ignored; we re-query
    permissions_status(app).await
}

#[tauri::command]
pub async fn permissions_open_settings(kind: String) -> Result<(), String> {
    let url = match kind.as_str() {
        "microphone" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        // System Audio Recording is listed in the same pane.
        "screen" | "systemAudio" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
        "notifications" => "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=no.humla.app",
        _ => return Err("unknown kind".into()),
    };
    tokio::process::Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|e| format!("open: {e}"))?
        .wait()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(screen: &str, system_audio: &str) -> PermissionsStatus {
        PermissionsStatus {
            microphone: "granted".into(),
            screen: screen.into(),
            system_audio: system_audio.into(),
            notifications: String::new(),
        }
    }

    #[test]
    fn a_granted_system_audio_permission_uses_the_tap() {
        for screen in ["granted", "denied", "unknown"] {
            assert_eq!(system_audio_source(&status(screen, "granted")), SystemAudioSource::ProcessTap);
        }
    }

    #[test]
    fn screen_recording_users_keep_screencapturekit_until_they_allow_system_audio() {
        assert_eq!(
            system_audio_source(&status("granted", "not_determined")),
            SystemAudioSource::ScreenCaptureKit
        );
    }

    #[test]
    fn a_new_user_without_either_gets_the_tap_and_its_prompt() {
        for screen in ["denied", "unknown"] {
            assert_eq!(
                system_audio_source(&status(screen, "not_determined")),
                SystemAudioSource::ProcessTap
            );
        }
    }

    #[test]
    fn a_denied_tap_is_never_started() {
        assert_eq!(system_audio_source(&status("granted", "denied")), SystemAudioSource::ScreenCaptureKit);
        assert_eq!(system_audio_source(&status("denied", "denied")), SystemAudioSource::Off);
        // A Screen Recording answer that timed out may still be a grant.
        assert_eq!(system_audio_source(&status("unknown", "denied")), SystemAudioSource::ScreenCaptureKit);
    }

    #[test]
    fn older_macos_and_an_unknown_answer_keep_screencapturekit() {
        for sa in ["unsupported", "unknown", ""] {
            assert_eq!(system_audio_source(&status("granted", sa)), SystemAudioSource::ScreenCaptureKit);
        }
    }

    #[test]
    fn warns_only_when_the_far_side_wont_be_captured() {
        assert_eq!(far_side_warning(&status("granted", "not_determined")), None);
        assert_eq!(far_side_warning(&status("denied", "granted")), None);
        assert_eq!(far_side_warning(&status("denied", "not_determined")), None);
        assert!(far_side_warning(&status("denied", "denied")).unwrap().starts_with("System audio"));
        assert!(far_side_warning(&status("unknown", "denied")).unwrap().starts_with("Screen Recording"));
        assert!(far_side_warning(&status("denied", "unsupported")).unwrap().starts_with("Screen Recording"));
    }

    #[test]
    fn an_older_sidecar_without_system_audio_reads_as_unknown() {
        let p: PermissionsStatus =
            serde_json::from_str(r#"{"microphone":"granted","screen":"granted"}"#).unwrap();
        assert_eq!(p.system_audio, "unknown");
        let p: PermissionsStatus =
            serde_json::from_str(r#"{"microphone":"granted","screen":"granted","systemAudio":"granted"}"#)
                .unwrap();
        assert_eq!(p.system_audio, "granted");
    }
}
