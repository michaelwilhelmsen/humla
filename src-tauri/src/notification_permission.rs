//! Whether macOS lets Humla post notifications. The permission belongs to the
//! app bundle, so it is read and requested in this process.

use std::ptr::NonNull;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::NSError;
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSettings,
    UNUserNotificationCenter,
};
use parking_lot::Mutex;

/// How long a request waits for the user to answer macOS's prompt.
const ANSWER_WAIT: Duration = Duration::from_secs(300);
const STATUS_WAIT: Duration = Duration::from_secs(2);

/// macOS's current answer.
pub(crate) async fn status() -> &'static str {
    if !bundled() {
        return "unknown";
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Scoped so nothing that isn't `Send` is held across the `.await`.
    {
        let tx = Mutex::new(Some(tx));
        let handler = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: macOS hands the handler a live settings object.
            let answer = status_label(unsafe { settings.as_ref() }.authorizationStatus());
            if let Some(tx) = tx.lock().take() {
                let _ = tx.send(answer);
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&handler);
    }
    tokio::time::timeout(STATUS_WAIT, rx)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or("unknown")
}

/// Ask macOS, which shows its prompt only while nothing has been answered, and
/// wait up to [`ANSWER_WAIT`] for the answer.
pub(crate) async fn request() {
    if !bundled() {
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let tx = Mutex::new(Some(tx));
        let handler = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
            if let Some(tx) = tx.lock().take() {
                let _ = tx.send(());
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &handler,
            );
    }
    let _ = tokio::time::timeout(ANSWER_WAIT, rx).await;
}

fn bundled() -> bool {
    std::env::current_exe().map(|exe| in_app_bundle(&exe)).unwrap_or(false)
}

/// macOS's answer in the vocabulary of `PermissionStatus` (src/lib/ipc.ts).
fn status_label(status: UNAuthorizationStatus) -> &'static str {
    match status {
        UNAuthorizationStatus::NotDetermined => "not_determined",
        UNAuthorizationStatus::Denied => "denied",
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => "granted",
        _ => "unknown",
    }
}

/// UserNotifications raises for a process that isn't inside an app bundle,
/// which a `tauri dev` binary isn't.
fn in_app_bundle(exe: &std::path::Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_answers_map_to_the_permission_vocabulary() {
        assert_eq!(status_label(UNAuthorizationStatus::NotDetermined), "not_determined");
        assert_eq!(status_label(UNAuthorizationStatus::Denied), "denied");
        assert_eq!(status_label(UNAuthorizationStatus::Authorized), "granted");
    }

    #[test]
    fn every_kind_of_yes_reads_as_granted() {
        assert_eq!(status_label(UNAuthorizationStatus::Provisional), "granted");
        assert_eq!(status_label(UNAuthorizationStatus::Ephemeral), "granted");
    }

    #[test]
    fn an_answer_this_build_doesnt_know_reads_as_unknown() {
        assert_eq!(status_label(UNAuthorizationStatus(99)), "unknown");
    }

    #[test]
    fn only_a_bundled_app_asks_macos() {
        use std::path::Path;
        assert!(in_app_bundle(Path::new("/Applications/Humla.app/Contents/MacOS/humla")));
        assert!(!in_app_bundle(Path::new(
            "/Users/me/humla/src-tauri/target/debug/humla"
        )));
    }
}
