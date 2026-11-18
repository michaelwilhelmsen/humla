//! GitHub Copilot in Settings (#227): sign-in by device flow, the CLI's
//! whereabouts and download, and the model list. Signing in and the download
//! run in the background and report through `copilot_sign_in` and
//! `copilot_cli_download` events, since either can outlast an invoke.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::copilot::{self, auth, cli};
use crate::AppState;

/// Bumped by every sign-in start, cancel and sign-out, so an older poll stops itself.
static SIGN_IN_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Held while a download runs: the Settings cards and a call that found a CLI
/// on another protocol can all ask for one, and only one runs.
static DOWNLOAD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotStatus {
    /// The GitHub account signed in, if any.
    login: Option<String>,
    cli_path: Option<String>,
    cli_version: Option<String>,
    /// Why the CLI last failed to start, if it did.
    cli_error: Option<String>,
    /// Whether the CLI found is Humla's own download of `download_version`.
    cli_is_download: bool,
    /// The version a download would fetch.
    download_version: &'static str,
}

#[tauri::command]
pub async fn copilot_status(app: AppHandle, state: State<'_, AppState>) -> Result<CopilotStatus, String> {
    let login = copilot::read_credentials(&state.keychain_cache)?.map(|c| c.login);
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let cli_path = state.copilot.cli(&app_data).await;
    let cli_version = match &cli_path {
        Some(p) => cli::version(p).await,
        None => None,
    };
    let cli_is_download = cli_path.as_deref() == Some(cli::managed_path(&app_data).as_path());
    Ok(CopilotStatus {
        login,
        cli_is_download,
        cli_path: cli_path.map(|p| p.display().to_string()),
        cli_version,
        cli_error: state.copilot.start_error(),
        download_version: cli::VERSION,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInCode {
    user_code: String,
    verification_uri: String,
    expires_in: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SignInEvent {
    /// `signed_in` | `denied` | `expired` | `failed`
    status: &'static str,
    login: Option<String>,
    message: Option<String>,
}

/// Start the device flow and poll for its answer in the background.
#[tauri::command]
pub async fn copilot_sign_in_start(app: AppHandle) -> Result<SignInCode, String> {
    let code = auth::request_device_code().await.map_err(|e| match e {
        auth::AuthFailure::Rejected(m) | auth::AuthFailure::Unreachable(m) => m,
    })?;
    let generation = SIGN_IN_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let reply = SignInCode {
        user_code: code.user_code.clone(),
        verification_uri: code.verification_uri.clone(),
        expires_in: code.expires_in,
    };
    tauri::async_runtime::spawn(async move {
        let event = poll_until_done(&app, &code, generation).await;
        if let Some(event) = event {
            let _ = app.emit("copilot_sign_in", event);
        }
    });
    Ok(reply)
}

#[tauri::command]
pub fn copilot_sign_in_cancel() {
    SIGN_IN_GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// `None` when a newer sign-in or a cancel superseded this one.
async fn poll_until_done(app: &AppHandle, code: &auth::DeviceCode, generation: u64) -> Option<SignInEvent> {
    let failed = |message: String| SignInEvent { status: "failed", login: None, message: Some(message) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(1);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        if SIGN_IN_GENERATION.load(Ordering::SeqCst) != generation {
            return None;
        }
        if std::time::Instant::now() > deadline {
            return Some(SignInEvent { status: "expired", login: None, message: None });
        }
        let grant = match auth::poll_device_code(&code.device_code).await {
            Ok(auth::Poll::Pending) | Err(auth::AuthFailure::Unreachable(_)) => continue,
            Ok(auth::Poll::SlowDown(next)) => {
                interval = next.max(interval + 5);
                continue;
            }
            Ok(auth::Poll::Denied) => return Some(SignInEvent { status: "denied", login: None, message: None }),
            Ok(auth::Poll::Expired) => return Some(SignInEvent { status: "expired", login: None, message: None }),
            Err(auth::AuthFailure::Rejected(m)) => return Some(failed(m)),
            Ok(auth::Poll::Granted(grant)) => grant,
        };
        let login = match auth::fetch_login(&grant.access_token).await {
            Ok(login) => login,
            Err(auth::AuthFailure::Rejected(m) | auth::AuthFailure::Unreachable(m)) => return Some(failed(m)),
        };
        let creds = auth::Credentials {
            login: login.clone(),
            access_token: grant.access_token,
            access_expires_at: grant.access_expires_at,
            refresh_token: grant.refresh_token,
            refresh_expires_at: grant.refresh_expires_at,
        };
        // A sign-out or a newer sign-in during the last request wins.
        if SIGN_IN_GENERATION.load(Ordering::SeqCst) != generation {
            return None;
        }
        let state: State<AppState> = app.state();
        if let Err(e) = copilot::write_credentials(&state.keychain_cache, Some(&creds)) {
            return Some(failed(e));
        }
        return Some(SignInEvent { status: "signed_in", login: Some(login), message: None });
    }
}

#[tauri::command]
pub async fn copilot_sign_out(state: State<'_, AppState>) -> Result<(), String> {
    SIGN_IN_GENERATION.fetch_add(1, Ordering::SeqCst);
    state.copilot.stop().await;
    copilot::write_credentials(&state.keychain_cache, None)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadEvent {
    /// `progress` | `done` | `failed`
    status: &'static str,
    fraction: Option<f64>,
    message: Option<String>,
}

/// Download the pinned CLI in the background.
#[tauri::command]
pub async fn copilot_cli_download(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn(async move {
        let _ = fetch_cli(&app).await;
    });
    Ok(())
}

/// Download the pinned CLI unless it is already there, reporting through
/// `copilot_cli_download` events.
async fn fetch_cli(app: &AppHandle) -> anyhow::Result<()> {
    let app_data = app.path().app_data_dir()?;
    let _only_one = DOWNLOAD.lock().await;
    let result = if cli::managed_path(&app_data).is_file() {
        Ok(())
    } else {
        let mut last = -1.0;
        let progress_app = app.clone();
        cli::download(&app_data, move |fraction| {
            if fraction - last >= 0.01 || fraction >= 1.0 {
                last = fraction;
                let _ = progress_app.emit(
                    "copilot_cli_download",
                    DownloadEvent { status: "progress", fraction: Some(fraction), message: None },
                );
            }
        })
        .await
        .map(|_| ())
    };
    let state: State<AppState> = app.state();
    state.copilot.forget_cli().await;
    let event = match &result {
        Ok(()) => DownloadEvent { status: "done", fraction: Some(1.0), message: None },
        Err(e) => DownloadEvent { status: "failed", fraction: None, message: Some(format!("{e:#}")) },
    };
    let _ = app.emit("copilot_cli_download", event);
    result
}

/// A running client signed in as the user. A CLI on another protocol than the
/// SDK's is replaced by the pinned download, and the start tried once more.
pub(crate) async fn copilot_client(app: &AppHandle) -> anyhow::Result<copilot_sdk::Client> {
    let state: State<AppState> = app.state();
    let app_data = app.path().app_data_dir()?;
    match state.copilot.ready_client(&state.keychain_cache, &app_data).await {
        Err(e) => {
            let found = state.copilot.cli(&app_data).await;
            if !copilot::should_replace_cli(&e, found.as_deref(), &cli::managed_path(&app_data)) {
                return Err(e);
            }
            eprintln!("[copilot] the CLI speaks another protocol; downloading {}", cli::VERSION);
            fetch_cli(app).await.context("couldn't download the Copilot CLI Humla supports")?;
            state.copilot.ready_client(&state.keychain_cache, &app_data).await
        }
        ok => ok,
    }
}

#[tauri::command]
pub async fn copilot_models(app: AppHandle) -> Result<Vec<copilot::session::ModelInfo>, String> {
    let client = copilot_client(&app).await.map_err(|e| e.to_string())?;
    copilot::session::list_models(&client).await.map_err(|e| e.to_string())
}
