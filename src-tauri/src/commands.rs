use crate::db::{self, Note, NotePatch};
use crate::diarize;
use crate::languages;
use crate::openai;
use crate::local_whisper;
use crate::presets;
use crate::wav;
use crate::recording::{ChunkRecord, ChunkSource, DiagnosticPayload, ErrorPayload, Inflight, Phase, RecordingStatus, SidecarEvent, StreamDeltaPayload, SummaryPayload, TranscriptPayload};
use crate::AppState;
use futures_util::StreamExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const DEFAULT_LANGUAGE: &str = "no";
const DEFAULT_TRANSCRIBE_PROVIDER: &str = "openai";
const DEFAULT_TRANSCRIBE_MODEL: &str = "whisper-1";
const DEFAULT_WHISPER_PRESET: &str = "quality";
// Default ON: Apple Silicon Macs have working Metal and the speedup is
// huge (~10× over BLAS). When `use_gpu` is true and Metal init fails at
// runtime, whisper.cpp logs the failure and falls back to BLAS — but the
// failed compile is noisy and adds startup time. Users on machines where
// Metal is broken (e.g. macOS Metal compiler rejecting the bundled
// shader) can flip this off in Settings to skip the failed init entirely.
const DEFAULT_LOCAL_WHISPER_USE_GPU: &str = "true";
// Final pass: re-transcribe the whole recording from the saved full WAV
// after stop, instead of trusting the live chunked output. Default ON for
// new installs because it's the higher-quality path; the user can turn it
// off in Settings if they're on a slow machine or want immediate transcripts.
// Local provider only at the moment (cloud gets a no-op).
const DEFAULT_FINAL_PASS: &str = "true";
const DEFAULT_SUMMARY_MODEL: &str = "gpt-5.4-mini";
// Ollama's default port + OpenAI-compat path. Any user running LM Studio,
// llama-server, or vLLM will override this in Settings.
const DEFAULT_LOCAL_LLM_BASE_URL: &str = "http://localhost:11434/v1";
const DEFAULT_POLISH_PROMPT: &str = "You are correcting a raw speech-to-text transcript produced by Whisper. The transcript is already mostly correct. Your job is conservative cleanup, not rewriting.

Apply ONLY these changes:
- Fix typos where the intended word is unambiguous from context.
- Repair words cut at chunk boundaries (e.g. 'mistred' → 'mistenkte') only when context strongly supports the correction.
- Add missing punctuation (commas, periods, question marks) where a sentence is clearly complete and unambiguous.
- Use the user's notes (when provided) and the custom vocabulary (when provided) to spell proper nouns and domain terms correctly.

NEVER:
- Add or remove line breaks, paragraph breaks, or whitespace structure. Preserve the input's exact line layout.
- Split sentences that are joined or merge sentences that are split. Leave the existing sentence boundaries alone.
- Rephrase, 'improve', shorten, or smooth over the speaker's actual words. Preserve their voice — clumsy phrasing stays clumsy.
- Remove filler ('uh', 'um', 'liksom', 'ikke sant', 'altså'). The user wants their actual speech, not a cleaned-up version. They can edit if they want filler gone.
- Add headings, bullet lists, markdown, bolding, italics, or any other formatting markers.
- Add facts, names, numbers, or claims that are not present in the raw transcript.
- Translate the transcript or change its language.

SPEAKER LABELS:
- Lines may begin with a speaker label followed by ': ' — e.g. 'Speaker 1: ', 'Speaker 2: ', the special 'You: ' (which marks the user's own side on calls), or a custom name the user has assigned like 'Michael: ' or 'Anna: '. Preserve these labels EXACTLY as they appear: same text, same colon-space, same position at the start of the line.
- NEVER move text between speakers, merge consecutive turns from different speakers, split a single turn across multiple speakers, or invent new speakers.
- The number of lines beginning with a label-followed-by-colon must equal the input. The order of speakers must be identical.

When uncertain whether a word is a mishearing, leave it as-is. Doing nothing is always safer than guessing.

Output ONLY the corrected transcript text. Preserve the input's line structure exactly — same number of lines, same line breaks, same paragraph layout. No commentary, no preamble.";
const DEFAULT_SUMMARY_PROMPT: &str = "Du lager møtenotater fra en automatisk transkribert samtale.\n\nKilder du får:\n- [Notater] — det brukeren skrev under møtet (autoritativ kilde for navn, tall og beslutninger).\n- [Transkripsjon] — automatisk generert fra lyden, kan inneholde feil.\n\nNår transkripsjon og notater er i konflikt, stol på notatene.\n\nSkriv på norsk i Markdown. Inkluder kun seksjoner som er reelt relevante — ikke skriv \"Ingen identifisert\".\n\n- **Sammendrag** — 2–4 setninger som fanger essensen.\n- **Beslutninger** — kun reelle beslutninger som ble tatt.\n- **Handlingspunkter** — på formen \"Beskrivelse — Ansvarlig (frist når oppgitt)\".\n- **Åpne spørsmål** — uavklarte ting som krever oppfølging.\n\nVær konkret og kort. Ikke gjenta deg selv. Ikke finn på detaljer som ikke står i kilden.";
const API_KEY: &str = "__openai_api_key__";

fn read_secret(state: &State<AppState>, key: &str) -> Result<Option<String>, String> {
    let conn = state.db.lock();
    db::get_setting(&conn, key).map_err(err).map(|opt| {
        opt.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        })
    })
}

fn err<E: std::fmt::Display>(e: E) -> String { e.to_string() }

#[tauri::command]
pub fn notes_list(state: State<AppState>) -> Result<Vec<Note>, String> {
    let conn = state.db.lock();
    db::list_notes(&conn).map_err(err)
}

#[tauri::command]
pub fn notes_get(state: State<AppState>, id: String) -> Result<Note, String> {
    let conn = state.db.lock();
    db::get_note(&conn, &id).map_err(err)
}

#[tauri::command]
pub fn notes_create(state: State<AppState>) -> Result<Note, String> {
    let conn = state.db.lock();
    // New notes inherit the user's defaults for language + summary preset.
    // Both are overridable per-note from the note view; pre-feature notes
    // (empty language) fall back at transcribe / summary time.
    let default_language = db::get_setting(&conn, "language")
        .map_err(err)?
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
    let default_preset = db::get_setting(&conn, "default_summary_preset")
        .map_err(err)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "meeting".to_string());
    db::create_note(&conn, &default_language, &default_preset).map_err(err)
}

#[tauri::command]
pub fn app_data_dir(app: AppHandle) -> Result<String, String> {
    let path = app.path().app_data_dir().map_err(err)?;
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "non-utf8 path".to_string())
}

#[tauri::command]
pub fn notes_update(state: State<AppState>, id: String, patch: NotePatch) -> Result<(), String> {
    let conn = state.db.lock();
    db::update_note(&conn, &id, &patch).map_err(err)
}

#[tauri::command]
pub fn notes_delete(state: State<AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::delete_note(&conn, &id).map_err(err)
}

#[tauri::command]
pub fn notes_move(
    state: State<AppState>,
    id: String,
    folder_id: Option<String>,
) -> Result<(), String> {
    let conn = state.db.lock();
    db::move_note(&conn, &id, folder_id.as_deref()).map_err(err)
}

#[tauri::command]
pub fn folders_list(state: State<AppState>) -> Result<Vec<db::Folder>, String> {
    let conn = state.db.lock();
    db::list_folders(&conn).map_err(err)
}

#[tauri::command]
pub fn folders_create(state: State<AppState>, name: String) -> Result<db::Folder, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".into());
    }
    let conn = state.db.lock();
    db::create_folder(&conn, trimmed).map_err(err)
}

#[tauri::command]
pub fn folders_rename(state: State<AppState>, id: String, name: String) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".into());
    }
    let conn = state.db.lock();
    db::rename_folder(&conn, &id, trimmed).map_err(err)
}

#[tauri::command]
pub fn folders_delete(state: State<AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::delete_folder(&conn, &id).map_err(err)
}

#[tauri::command]
pub fn settings_get(state: State<AppState>, key: String) -> Result<Option<String>, String> {
    let conn = state.db.lock();
    db::get_setting(&conn, &key).map_err(err)
}

#[tauri::command]
pub fn settings_set(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::set_setting(&conn, &key, &value).map_err(err)
}

#[tauri::command]
pub fn summary_prompts_list(
    state: State<AppState>,
) -> Result<Vec<db::SummaryPrompt>, String> {
    let conn = state.db.lock();
    db::list_summary_prompts(&conn).map_err(err)
}

#[tauri::command]
pub fn summary_prompts_create(
    state: State<AppState>,
    name: String,
    content: String,
) -> Result<db::SummaryPrompt, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Prompt name cannot be empty".into());
    }
    let conn = state.db.lock();
    db::create_summary_prompt(&conn, trimmed_name, &content).map_err(err)
}

#[tauri::command]
pub fn summary_prompts_update(
    state: State<AppState>,
    id: String,
    name: String,
    content: String,
) -> Result<db::SummaryPrompt, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Prompt name cannot be empty".into());
    }
    let conn = state.db.lock();
    db::update_summary_prompt(&conn, &id, trimmed_name, &content).map_err(err)
}

#[tauri::command]
pub fn summary_prompts_delete(state: State<AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::delete_summary_prompt(&conn, &id).map_err(err)
}

#[tauri::command]
pub fn api_key_get(state: State<AppState>) -> Result<Option<String>, String> {
    Ok(read_secret(&state, API_KEY)?.map(|_| "stored".to_string()))
}

#[tauri::command]
pub fn api_key_set(state: State<AppState>, key: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::set_setting(&conn, API_KEY, key.trim()).map_err(err)
}

#[derive(serde::Serialize)]
pub struct TestResult {
    ok: bool,
    status: u16,
    error: Option<String>,
}

#[tauri::command]
pub async fn api_key_test(state: State<'_, AppState>) -> Result<TestResult, String> {
    let key = read_secret(&state, API_KEY)?.ok_or_else(|| "No API key stored".to_string())?;

    let r = openai::client()
        .get(format!("{}/models", openai::BASE))
        .bearer_auth(&key)
        .send()
        .await
        .map_err(|e| format!("network: {e}"))?;

    let status = r.status();
    if status.is_success() {
        return Ok(TestResult { ok: true, status: status.as_u16(), error: None });
    }
    let body = r.text().await.unwrap_or_default();
    let snippet: String = body.chars().take(300).collect();
    Ok(TestResult { ok: false, status: status.as_u16(), error: Some(snippet) })
}

// ---- Speaker diarization model management ---------------------------------

#[tauri::command]
pub async fn diarize_status(app: AppHandle) -> Result<diarize::ModelStatus, String> {
    diarize::status(&app).await.map_err(err)
}

#[tauri::command]
pub async fn diarize_download(app: AppHandle) -> Result<(), String> {
    diarize::download(&app).await.map_err(err)
}

#[tauri::command]
pub async fn diarize_delete(app: AppHandle) -> Result<(), String> {
    diarize::delete(&app).await.map_err(err)
}

// ---- Local Whisper model management ----------------------------------------

fn local_model_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(err)?.join("models");
    Ok(dir)
}

fn model_path_for(app: &AppHandle, info: &local_whisper::ModelInfo) -> Result<PathBuf, String> {
    Ok(local_model_dir(app)?.join(info.filename))
}

/// Resolve the model file to use for a recording.
///
/// Resolution order:
///   1. Language addon. If a `LanguageAddon { language }` model matches
///      the recording's language and is downloaded on disk, use it. NB
///      Whisper Large takes this slot for Norwegian audio; future
///      language-specialised models drop in via the same registry hook.
///      Skips on "auto" — we can't know the language pre-decode.
///   2. Active primary. The user's selected `local_whisper_model`,
///      restricted to `Primary`-kind entries (so a stale setting can't
///      promote an addon to active).
///   3. Default primary. Fallback when the selection is empty, unknown,
///      or points at a non-Primary entry.
///
/// Returns the resolved path even when the file doesn't exist on disk —
/// that's how the caller's "not downloaded" error surfaces with a real
/// path the user can recognise.
fn local_whisper_use_gpu_setting(state: &State<AppState>) -> bool {
    let conn = state.db.lock();
    db::get_setting(&conn, "local_whisper_use_gpu")
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_LOCAL_WHISPER_USE_GPU.to_string())
        != "false"
}

fn local_model_path(app: &AppHandle, language: &str) -> Result<PathBuf, String> {
    let dir = local_model_dir(app)?;
    if let Some(addon) = local_whisper::addon_for_language(language) {
        let p = dir.join(addon.filename);
        if p.exists() {
            return Ok(p);
        }
    }
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let id = db::get_setting(&conn, "local_whisper_model")
        .map_err(err)?
        .unwrap_or_default();
    drop(conn);
    let info = local_whisper::find_model(&id)
        .filter(|m| m.kind == local_whisper::ModelKind::Primary)
        .unwrap_or_else(local_whisper::default_model);
    let path = dir.join(info.filename);
    if path.exists() {
        return Ok(path);
    }
    Ok(dir.join(local_whisper::default_model().filename))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWhisperModelStatus {
    id: String,
    label: String,
    description: String,
    filename: String,
    size_bytes_hint: u64,
    /// "primary" or "addon". Frontend renders addons in a separate group
    /// without the active-model radio button — they auto-apply via
    /// addon_language instead of being user-selectable.
    kind: &'static str,
    /// Set for `kind == "addon"`. The recording language that triggers
    /// this model. None for primaries.
    addon_language: Option<String>,
    downloaded: bool,
    size_bytes: Option<u64>,
    path: Option<String>,
}

#[tauri::command]
pub fn local_whisper_models(app: AppHandle) -> Result<Vec<LocalWhisperModelStatus>, String> {
    let dir = local_model_dir(&app)?;
    let mut out = Vec::with_capacity(local_whisper::models().len());
    for info in local_whisper::models() {
        let path = dir.join(info.filename);
        let downloaded = path.exists();
        let size_bytes = if downloaded {
            std::fs::metadata(&path).ok().map(|m| m.len())
        } else {
            None
        };
        let (kind, addon_language) = match info.kind {
            local_whisper::ModelKind::Primary => ("primary", None),
            local_whisper::ModelKind::LanguageAddon { language } => {
                ("addon", Some(language.to_string()))
            }
        };
        out.push(LocalWhisperModelStatus {
            id: info.id.to_string(),
            label: info.label.to_string(),
            description: info.description.to_string(),
            filename: info.filename.to_string(),
            size_bytes_hint: info.size_bytes_hint,
            kind,
            addon_language,
            downloaded,
            size_bytes,
            path: if downloaded { path.to_str().map(|s| s.to_string()) } else { None },
        });
    }
    Ok(out)
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    model_id: String,
    received: u64,
    total: Option<u64>,
}

#[tauri::command]
pub async fn local_whisper_download(app: AppHandle, model_id: String) -> Result<(), String> {
    let info = local_whisper::find_model(&model_id)
        .ok_or_else(|| format!("unknown model id: {model_id}"))?;
    let dir = local_model_dir(&app)?;
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("mkdir: {e}"))?;
    let final_path = dir.join(info.filename);
    // Download to a temp file in the same dir, then rename atomically so a
    // crash mid-download never leaves a half-written model in place.
    let tmp_path = dir.join(format!("{}.partial", info.filename));

    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| format!("client: {e}"))?
        .get(info.url)
        .send()
        .await
        .map_err(|e| format!("network: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {}: HTTP {}", info.url, resp.status()));
    }
    let total = resp.content_length();
    let _ = app.emit("local_whisper_progress", DownloadProgress {
        model_id: info.id.to_string(),
        received: 0,
        total,
    });

    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| format!("create tmp: {e}"))?;
    let mut received: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("stream: {e}"))?;
        file.write_all(&bytes).await.map_err(|e| format!("write: {e}"))?;
        received += bytes.len() as u64;
        // Throttle progress events to ~10/sec; UI doesn't need every chunk.
        if last_emit.elapsed() >= std::time::Duration::from_millis(100) {
            let _ = app.emit("local_whisper_progress", DownloadProgress {
                model_id: info.id.to_string(),
                received,
                total,
            });
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().await.map_err(|e| format!("flush: {e}"))?;
    drop(file);
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(|e| format!("rename: {e}"))?;
    let _ = app.emit("local_whisper_progress", DownloadProgress {
        model_id: info.id.to_string(),
        received,
        total,
    });
    Ok(())
}

#[tauri::command]
pub async fn local_whisper_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> Result<(), String> {
    let info = local_whisper::find_model(&model_id)
        .ok_or_else(|| format!("unknown model id: {model_id}"))?;
    let path = model_path_for(&app, info)?;
    // Drop the loaded model from RAM first when it's the one being deleted,
    // so we're not holding the file. SharedContext keys by path, so it's
    // safe to call unconditionally; worst case the next transcribe reloads
    // a model that didn't actually need to be evicted.
    local_whisper::unload(&state.whisper);
    if path.exists() {
        tokio::fs::remove_file(&path).await.map_err(|e| format!("remove: {e}"))?;
    }
    Ok(())
}

// ---- Local LLM (OpenAI-compatible HTTP server) ----------------------------

// Hit the user-configured local LLM server's /v1/models endpoint and return
// the list of model IDs. Used by Settings to populate the Model dropdown when
// the user picks Local provider. Most servers (Ollama, LM Studio, llama-server,
// vLLM) implement this exact OpenAI-compatible shape.
#[tauri::command]
pub async fn local_llm_list_models(base_url: String) -> Result<Vec<String>, String> {
    openai::list_models(&base_url).await.map_err(err)
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct PermissionsStatus {
    pub microphone: String,
    pub screen: String,
}

async fn run_sidecar_cmd(app: &AppHandle, mode: &str) -> Result<String, String> {
    let path = sidecar_path(app)?;
    let mut child = tokio::process::Command::new(&path)
        .arg(mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let fut = child.wait_with_output();
    match tokio::time::timeout(std::time::Duration::from_secs(3), fut).await {
        Ok(Ok(out)) => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
        Ok(Err(e)) => Err(format!("read: {e}")),
        Err(_) => Err("sidecar timed out".into()),
    }
}

#[tauri::command]
pub async fn permissions_status(app: AppHandle) -> Result<PermissionsStatus, String> {
    let stdout = run_sidecar_cmd(&app, "status").await?;
    let line = stdout.lines().last().unwrap_or("");
    serde_json::from_str(line).map_err(|e| format!("parse: {e}: {line}"))
}

#[tauri::command]
pub async fn permissions_request(app: AppHandle, kind: String) -> Result<PermissionsStatus, String> {
    let mode = match kind.as_str() {
        "microphone" => "request-microphone",
        "screen" => "request-screen",
        _ => return Err("unknown kind".into()),
    };
    let _ = run_sidecar_cmd(&app, mode).await; // result ignored; we re-query
    permissions_status(app).await
}

#[tauri::command]
pub async fn permissions_open_settings(kind: String) -> Result<(), String> {
    let url = match kind.as_str() {
        "microphone" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        "screen" => "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
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

#[tauri::command]
pub fn recording_pause(state: State<AppState>) -> Result<(), String> {
    let s = state.recording.lock();
    let child = s.child.as_ref().ok_or("not recording")?;
    let pid = child.id().ok_or("no pid")? as i32;
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid, libc::SIGUSR1) != 0 {
            return Err(format!("kill: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn recording_resume(state: State<AppState>) -> Result<(), String> {
    let s = state.recording.lock();
    let child = s.child.as_ref().ok_or("not recording")?;
    let pid = child.id().ok_or("no pid")? as i32;
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid, libc::SIGUSR2) != 0 {
            return Err(format!("kill: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn recording_state(state: State<AppState>) -> Result<&'static str, String> {
    let s = state.recording.lock();
    Ok(if s.note_id.is_some() { "recording" } else { "idle" })
}

#[tauri::command]
pub async fn recording_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note_id: String,
) -> Result<(), String> {
    // Self-heal stale sessions before refusing. We get here when the
    // session struct still has note_id set — could be a real recording
    // in progress, or a zombie left behind by a dev reload / app crash
    // that didn't flow through recording_stop.
    //
    // - If the tracked child has already exited → pure garbage, clear it.
    // - If the child is still running but its reader handle is gone (the
    //   stdout pipe was closed without recording_stop running) → orphan,
    //   SIGTERM it and take over.
    // - Only when both child AND reader are alive do we treat it as a
    //   genuine concurrent recording and refuse.
    let stale_child: Option<tokio::process::Child> = {
        let mut s = state.recording.lock();
        if s.note_id.is_some() {
            let child_dead = match s.child.as_mut() {
                Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
                None => true,
            };
            let reader_dead = s.reader.as_ref().map_or(true, |r| r.is_finished());

            if !child_dead && !reader_dead {
                return Err("already recording".into());
            }

            let stale = s.child.take();
            s.note_id = None;
            s.temp_dir = None;
            s.reader = None;
            s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
            stale
        } else {
            None
        }
    };
    if let Some(mut c) = stale_child {
        if let Some(pid) = c.id() {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.wait()).await;
        let _ = c.kill().await;
    }

    // Pre-check the configured provider's prerequisites — without them
    // transcription always fails silently. Resolve the note's language up
    // front too: local_model_path uses it to decide whether a language
    // addon (e.g. NB Whisper for Norwegian) overrides the active primary.
    let (provider, language) = {
        let conn = state.db.lock();
        let p = db::get_setting(&conn, "transcribe_provider")
            .map_err(err)?
            .unwrap_or_else(|| DEFAULT_TRANSCRIBE_PROVIDER.to_string());
        let global = db::get_setting(&conn, "language")
            .map_err(err)?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note_lang = db::get_note(&conn, &note_id)
            .map(|n| n.language)
            .unwrap_or_default();
        let l = if note_lang.trim().is_empty() { global } else { note_lang };
        (p, l)
    };
    let pre_err = match provider.as_str() {
        "local" => {
            let p = local_model_path(&app, &language).map_err(|e| e.to_string())?;
            (!p.exists()).then_some(
                "Local Whisper model not downloaded. Download it in Settings → Transcription.",
            )
        }
        _ => read_secret(&state, API_KEY)?
            .is_none()
            .then_some("OpenAI API key not set. Add one in Settings → API keys."),
    };
    if let Some(msg) = pre_err {
        emit_error(&app, Some(&note_id), msg);
        return Err(msg.to_string());
    }

    // Race a Whisper model load against the sidecar startup so the first
    // chunk doesn't pay the cold-start tax (~1–2 s on Apple Silicon). Fire
    // and forget — by the time VAD rotates the first chunk, the model is
    // already in Metal memory and inference is fast.
    if provider == "local" {
        if let Ok(model_path) = local_model_path(&app, &language) {
            let use_gpu = local_whisper_use_gpu_setting(&state);
            let shared = state.whisper.clone();
            tokio::spawn(async move {
                if let Err(e) = local_whisper::prewarm(shared, model_path, use_gpu).await {
                    eprintln!("whisper prewarm: {e}");
                }
            });
        }
    }

    // Pre-check microphone permission — without it we can't capture anything useful.
    if let Ok(p) = permissions_status(app.clone()).await {
        if p.microphone != "granted" {
            let msg = "Microphone permission required. Open Settings → Permissions to grant.".to_string();
            emit_error(&app, Some(&note_id), &msg);
            return Err(msg);
        }
        if p.screen != "granted" {
            emit_error(&app, Some(&note_id),
                "Screen Recording not granted — only your microphone will be captured. Grant in Settings → Permissions and restart for the full meeting transcript.");
        }
    }

    emit_status(&app, Some(&note_id), Phase::Starting);

    let temp_dir = std::env::temp_dir().join(format!("notes-app-{}", note_id));
    std::fs::create_dir_all(&temp_dir).map_err(err)?;

    let sidecar_path = sidecar_path(&app)?;
    let mut cmd = Command::new(&sidecar_path);
    cmd.arg("--out").arg(&temp_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    // Detach the child into a new session so macOS TCC doesn't tie its
    // microphone / screen-recording authorization to the parent dev binary.
    // Without this, the sidecar inherits the parent's TCC "responsible process"
    // and is silently denied even though its own binary is granted.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // Non-fatal: continue without detaching.
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn audio-capture: {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;

    // Drain stderr in the background so the pipe never fills, and surface
    // anything written there as a recording_error so silent failures aren't.
    {
        let app_err = app.clone();
        let note_id_err = note_id.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                eprintln!("audio-capture stderr: {trimmed}");
                let _ = app_err.emit("recording_error", ErrorPayload {
                    note_id: Some(note_id_err.clone()),
                    message: format!("audio-capture: {trimmed}"),
                });
            }
        });
    }

    // Fresh inflight list for this session so handles from a previous
    // recording can never mix in.
    let inflight: Inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let mut s = state.recording.lock();
        s.note_id = Some(note_id.clone());
        s.child = Some(child);
        s.temp_dir = Some(temp_dir);
        s.inflight = inflight.clone();
        // Wipe any context from a previous recording — proper nouns and
        // sentence fragments from a different conversation would only confuse
        // this session's decoder. Same for the speaker bookkeeping. Per-source
        // trails because the mic and system streams are separate
        // conversations — sharing a trail would pull each Whisper invocation
        // toward the other side's vocabulary and language.
        s.mic_trail.lock().clear();
        s.sys_trail.lock().clear();
        s.chunk_log.lock().clear();
        *s.mic_full_wav_path.lock() = None;
        *s.sys_full_wav_path.lock() = None;
    }

    // Snapshot any existing transcript so diarize_and_apply can prepend it
    // to this session's output. Resuming a recording on a note that already
    // has transcript content adds to it; starting on a blank note produces
    // the snapshot "" and behaves like a fresh recording.
    {
        let state: State<AppState> = app.state();
        let existing = {
            let conn = state.db.lock();
            db::get_note(&conn, &note_id)
                .map(|n| n.transcript)
                .unwrap_or_default()
        };
        *state.recording.lock().transcript_at_start.lock() = existing;
    }

    let app_clone = app.clone();
    let note_id_clone = note_id.clone();
    let inflight_for_reader = inflight.clone();
    let reader_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            match serde_json::from_str::<SidecarEvent>(trimmed) {
                Ok(SidecarEvent::Chunk { source, path, start_ms }) => {
                    let pb = PathBuf::from(path);
                    let app2 = app_clone.clone();
                    let note_id2 = note_id_clone.clone();
                    let h = tokio::spawn(async move {
                        if let Err(e) = transcribe_chunk(app2.clone(), note_id2.clone(), source, pb, start_ms).await {
                            let msg = format!("Transcription failed: {e}");
                            eprintln!("{msg}");
                            let _ = app2.emit("recording_error", ErrorPayload {
                                note_id: Some(note_id2),
                                message: msg,
                            });
                        }
                    });
                    inflight_for_reader.lock().push(h);
                }
                Ok(SidecarEvent::FullRecording { source, path, duration_ms: _ }) => {
                    // Stash the path on the session; the diarization pass on
                    // stop reads them. We don't act here — the recording is
                    // still wrapping up. Each source has its own slot so the
                    // post-stop pass can branch (mic-only → diarize mic;
                    // both present → "You" + diarize sys).
                    let state: tauri::State<AppState> = app_clone.state();
                    let session = state.recording.lock();
                    let slot = match source {
                        ChunkSource::Mic => &session.mic_full_wav_path,
                        ChunkSource::Sys => &session.sys_full_wav_path,
                    };
                    *slot.lock() = Some(PathBuf::from(path));
                }
                Ok(SidecarEvent::Error { message }) => {
                    eprintln!("sidecar error: {message}");
                    let _ = app_clone.emit("recording_error", ErrorPayload {
                        note_id: Some(note_id_clone.clone()),
                        message,
                    });
                }
                Ok(SidecarEvent::Stopped) => break,
                Ok(SidecarEvent::Paused) => emit_status(&app_clone, Some(&note_id_clone), Phase::Paused),
                Ok(SidecarEvent::Resumed) => emit_status(&app_clone, Some(&note_id_clone), Phase::Recording),
                Ok(SidecarEvent::Heartbeat { mic_frames, sys_frames, chunks, mic_peak, sys_peak }) => {
                    let _ = app_clone.emit("recording_diagnostic", DiagnosticPayload {
                        note_id: note_id_clone.clone(),
                        mic_frames,
                        sys_frames,
                        chunks,
                        mic_peak,
                        sys_peak,
                    });
                }
                Err(e) => eprintln!("bad sidecar line: {e} -- {line}"),
            }
        }
        // Reader exited (sidecar closed its pipe). If the session is still
        // marked as recording for THIS note, that means the sidecar died
        // without us asking — i.e. a crash. Clean up and notify the UI so
        // the user isn't pinned in a stale "recording" state.
        let state: tauri::State<AppState> = app_clone.state();
        let was_active = {
            let mut s = state.recording.lock();
            if s.note_id.as_deref() == Some(&note_id_clone) {
                s.note_id = None;
                s.child = None;
                s.temp_dir = None;
                s.reader = None;
                s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
                true
            } else {
                false
            }
        };
        if was_active {
            let _ = app_clone.emit("recording_status", RecordingStatus { note_id: None, phase: Phase::Idle });
            let _ = app_clone.emit("recording_error", ErrorPayload {
                note_id: Some(note_id_clone.clone()),
                message: "Recording stopped unexpectedly. Try again.".to_string(),
            });
        }
    });

    state.recording.lock().reader = Some(reader_handle);

    emit_status(&app, Some(&note_id), Phase::Recording);
    Ok(())
}

#[tauri::command]
pub async fn recording_stop(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (child, note_id, temp_dir, inflight, reader) = {
        let mut s = state.recording.lock();
        let note_id = s.note_id.take().ok_or("not recording")?;
        let child = s.child.take();
        let temp_dir = s.temp_dir.take();
        // The reader holds a clone of this same Arc, so chunks emitted during
        // shutdown still land in the list we drain below. Swap in a fresh
        // list to keep `s` self-consistent for the next session.
        let inflight = std::mem::replace(&mut s.inflight, Arc::new(parking_lot::Mutex::new(Vec::new())));
        let reader = s.reader.take();
        (child, note_id, temp_dir, inflight, reader)
    };

    emit_status(&app, Some(&note_id), Phase::Stopping);

    if let Some(mut child) = child {
        // Send SIGTERM so the Swift sidecar runs its shutdown handler:
        // drains the mixer, finalizes the current WAV file, emits the partial
        // chunk. Then wait up to 3 seconds for it to exit gracefully before
        // falling back to SIGKILL.
        if let Some(pid) = child.id() {
            #[cfg(unix)]
            unsafe { libc::kill(pid as i32, libc::SIGTERM); }
        }
        let waited = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await;
        if waited.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    // Wait for the stdout reader to finish first: it exits when the sidecar
    // closes the pipe, which is guaranteed now that `child.wait()` returned.
    // After this point no more transcribe handles can be pushed to inflight.
    if let Some(r) = reader {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), r).await;
    }

    // Drain in-flight transcribe tasks (incl. any final chunk spawned during
    // the sidecar's shutdown handler). Cap the total drain so a stuck OpenAI
    // call can't pin us in Stopping forever.
    let drain = async {
        loop {
            let next = inflight.lock().pop();
            match next {
                Some(h) => { let _ = h.await; }
                None => break,
            }
        }
    };
    let drain_timed_out =
        tokio::time::timeout(std::time::Duration::from_secs(30), drain).await.is_err();

    // If the drain timed out, abort whatever's still lingering. A stuck
    // network call (e.g. OpenAI 503 retry) would otherwise keep running
    // past recording_stop and try to db::append_transcript onto the
    // post-stop labelled transcript that diarize_and_apply / final_pass_apply
    // produced — wholesale overwriting the speaker structure with raw chunk
    // text. The transcribe_chunk session-active guard is the second line of
    // defence for the one task that was being awaited when the drain future
    // was dropped (it's detached, not reachable from here).
    if drain_timed_out {
        let remaining: Vec<_> = inflight.lock().drain(..).collect();
        eprintln!(
            "recording_stop: drain timed out, aborting {} lingering transcribe(s)",
            remaining.len()
        );
        for h in &remaining {
            h.abort();
        }
        for h in remaining {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), h).await;
        }
    }

    // Spawn the post-stop processing chain in the background:
    //   Stopping → (Retranscribing | Diarizing) → Polishing → Idle
    // Branch on the `final_pass` setting: when enabled (and provider is
    // local), retranscribe the full WAV and rebuild the transcript with
    // segment-level speaker labels. Otherwise apply chunk-level labels
    // to the live transcript via the original diarize-only path. Polish
    // runs in either case as a strict typo-and-punctuation cleanup.
    let app_for_post = app.clone();
    let note_for_post = note_id.clone();
    // Move temp_dir into the post-stop task and clean it up *after* polish
    // completes. The previous design ran a parallel 30s-delay cleanup, which
    // worked when post-stop took ~10–30s (chunked diarize + polish) but
    // races the final pass: re-transcribing a 30-minute recording takes
    // several minutes, the cleanup fires mid-flight, and the diarize sidecar
    // then tries to open a file that's been deleted out from under it
    // (surfaces as a CoreAudio "wht?" / 2003334207 error from FluidAudio's
    // AVAudioFile reader). Sequencing cleanup behind the chain ensures the
    // full WAVs survive for as long as any post-stop step needs them.
    tokio::spawn(async move {
        let use_final_pass = {
            let state: State<AppState> = app_for_post.state();
            let conn = state.db.lock();
            let enabled = db::get_setting(&conn, "final_pass")
                .ok()
                .flatten()
                .unwrap_or_else(|| DEFAULT_FINAL_PASS.to_string());
            let provider = db::get_setting(&conn, "transcribe_provider")
                .ok()
                .flatten()
                .unwrap_or_else(|| DEFAULT_TRANSCRIBE_PROVIDER.to_string());
            enabled == "true" && provider == "local"
        };
        if use_final_pass {
            if let Err(e) = final_pass_apply(app_for_post.clone(), note_for_post.clone()).await {
                // Final pass failure leaves the live chunked transcript in
                // place — the user keeps content. Surface a toast and fall
                // through to chunk-based diarization so they still get
                // speaker labels.
                eprintln!("final_pass_apply: {e}");
                emit_error(
                    &app_for_post,
                    Some(&note_for_post),
                    &format!("Final pass failed (live transcript saved): {e}"),
                );
                if let Err(e2) = diarize_and_apply(app_for_post.clone(), note_for_post.clone()).await {
                    eprintln!("diarize_and_apply (fallback): {e2}");
                }
            }
        } else if let Err(e) = diarize_and_apply(app_for_post.clone(), note_for_post.clone()).await {
            eprintln!("diarize_and_apply: {e}");
            emit_error(
                &app_for_post,
                Some(&note_for_post),
                &format!("Diarization failed (transcript still saved): {e}"),
            );
        }
        if let Err(e) = polish_transcript(app_for_post.clone(), note_for_post.clone()).await {
            eprintln!("polish_transcript: {e}");
            emit_error(
                &app_for_post,
                Some(&note_for_post),
                &format!("Polish failed: {e}"),
            );
        }
        // Now that every step that needs the WAVs has finished, drop the
        // temp dir. Best-effort: a leftover dir is harmless and gets
        // collected by macOS's normal /tmp cleanup eventually.
        if let Some(dir) = temp_dir {
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
        emit_status(&app_for_post, None, Phase::Idle);
    });

    Ok(())
}

/// Run offline speaker diarization on the just-finished recording and
/// rewrite the transcript with proper labels. Branches on which streams
/// produced content:
///
/// - **Mic only** (in-person meeting, no system audio): diarize the mic
///   full WAV, label chunks `Speaker N:` in first-encounter order. This is
///   the original single-stream path; multiple humans sharing the same mic
///   get separated by community-1's clustering.
/// - **System only** (very rare; mic permission denied or some platform
///   weirdness): diarize the system full WAV, same `Speaker N:` labelling.
/// - **Both present** (remote/hybrid call): diarize the system stream for
///   remote-side speakers, label every mic chunk as `You:` (the user is
///   the only person on the mic side, by definition of channel
///   attribution). Skips diarizing the mic stream entirely — there's no
///   point classifying a stream where every chunk is the same person.
///
/// Resumed recordings prepend the snapshotted prior transcript and offset
/// this session's `Speaker N:` numbers past any existing ones so resumed
/// halves don't collide IDs (`You:` is a fixed label and isn't offset).
/// No-ops gracefully when the diarize model isn't downloaded, when no
/// chunks were captured, or when both streams produced nothing.
async fn diarize_and_apply(app: AppHandle, note_id: String) -> anyhow::Result<()> {
    // Pull session state with cloning so we can drop the parking_lot
    // guards before the long await on the sidecar. Also read the per-note
    // expected_speakers hint here while we're in the DB — passing the
    // resolved value forward keeps the long-await section free of locks.
    let (mic_wav, sys_wav, chunks, snapshot, expected_speakers) = {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let mic = session.mic_full_wav_path.lock().clone();
        let sys = session.sys_full_wav_path.lock().clone();
        let log = session.chunk_log.lock().clone();
        let snap = session.transcript_at_start.lock().clone();
        drop(session);
        let conn = state.db.lock();
        let hint = db::get_note(&conn, &note_id)
            .ok()
            .and_then(|n| n.expected_speakers)
            .filter(|n| *n > 0);
        (mic, sys, log, snap, hint)
    };
    if chunks.is_empty() {
        eprintln!("diarize: no chunks captured, skipping");
        return Ok(());
    }
    // Skip silently when the model isn't downloaded — diarization is
    // optional and the user might not have grabbed the model yet.
    match diarize::status(&app).await {
        Ok(s) if s.downloaded => {}
        _ => {
            eprintln!("diarize: model not downloaded, skipping");
            return Ok(());
        }
    }

    let mic_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Sys);

    emit_status(&app, Some(&note_id), Phase::Diarizing);

    // Decide which WAV to diarize and how to label chunks. The label
    // assignment is a per-chunk closure so the merge step doesn't need to
    // know which mode we're in.
    // The labeller closure crosses a `.await` (the cleanup_full_wav calls
    // below) inside a spawned task, so it has to be `Send`. The captured
    // segments + display map are both Send, so the bound just needs to be
    // declared on the trait object.
    type Labeller = dyn Fn(&ChunkRecord) -> Option<String> + Send;
    let label_for_chunk: Box<Labeller> = match (mic_chunks_present, sys_chunks_present) {
        (true, false) => {
            // In-person mode: diarize the mic stream, every chunk gets a
            // numbered label from its segment. The per-note expected
            // speaker hint applies directly — every speaker is on the mic.
            let Some(wav) = mic_wav.clone() else {
                // Missing mic_full.wav despite mic chunks: typically a
                // SIGKILL of the audio-capture sidecar before its shutdown
                // handler ran. Surface a toast so the user understands why
                // their transcript shows no speaker labels.
                eprintln!("diarize: mic chunks present but mic_full.wav missing, skipping");
                emit_error(
                    &app,
                    Some(&note_id),
                    "Diarization unavailable: the recording sidecar didn't write the full audio file. Transcript saved without speaker labels.",
                );
                return Ok(());
            };
            let segments = diarize::diarize_file(&app, &wav, expected_speakers).await?;
            if segments.is_empty() {
                eprintln!("diarize: no segments returned for mic stream, leaving transcript untagged");
                return Ok(());
            }
            let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
            Box::new(move |c: &ChunkRecord| {
                let sid = assign_speaker(c.start_ms, &segments)?;
                display_map.get(sid).map(|n| format!("Speaker {n}"))
            })
        }
        (false, true) => {
            // Edge case: system-only recording. Same as mic-only but on
            // the other stream. Numbered labels.
            let Some(wav) = sys_wav.clone() else {
                eprintln!("diarize: sys chunks present but sys_full.wav missing, skipping");
                emit_error(
                    &app,
                    Some(&note_id),
                    "Diarization unavailable: the recording sidecar didn't write the full audio file. Transcript saved without speaker labels.",
                );
                return Ok(());
            };
            let segments = diarize::diarize_file(&app, &wav, expected_speakers).await?;
            if segments.is_empty() {
                eprintln!("diarize: no segments returned for sys stream, leaving transcript untagged");
                return Ok(());
            }
            let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
            Box::new(move |c: &ChunkRecord| {
                let sid = assign_speaker(c.start_ms, &segments)?;
                display_map.get(sid).map(|n| format!("Speaker {n}"))
            })
        }
        (true, true) => {
            // Remote/hybrid call: mic = "You" by channel attribution; the
            // system stream gets diarized for remote-side speakers. Skip
            // the mic diarize call entirely — no information to gain when
            // every mic chunk is the same person.
            //
            // The per-note speaker hint is the *total* count the user
            // expects (themselves + remote participants). Subtract one for
            // the user's `You:` label so the diarizer is asked to find the
            // remaining N-1 on the system stream. Floors at 1 — entering
            // a hint of 1 in remote mode is nonsensical (would mean "just
            // me" yet sys has chunks), so treat it as "find 1 remote".
            let sys_speaker_hint = expected_speakers.map(|n| (n - 1).max(1));
            //
            // Three failure modes drop us into the single-speaker fallback
            // labeller (mic = "You", sys = "Speaker 1"):
            //   1. sys_full.wav missing (sidecar SIGKILL'd before close).
            //   2. diarize sidecar errored.
            //   3. diarize returned zero segments.
            //
            // The fallback assigns sys chunks `Speaker 1` rather than
            // returning `None`, because a `None` label causes
            // `build_labelled_transcript` to glue the chunk's text onto the
            // previous label's line — i.e. remote audio would silently
            // merge into the user's `You:` line. Better to surface a single
            // unlabeled-but-distinct speaker than to lose the boundary.
            let single_speaker_fallback = || -> Box<Labeller> {
                Box::new(|c: &ChunkRecord| match c.source {
                    ChunkSource::Mic => Some("You".to_string()),
                    ChunkSource::Sys => Some("Speaker 1".to_string()),
                })
            };
            match sys_wav.clone() {
                None => {
                    eprintln!("diarize: sys chunks present but sys_full.wav missing, falling back to single-speaker labels");
                    emit_error(
                        &app,
                        Some(&note_id),
                        "Diarization unavailable for the remote side; remote speakers grouped under Speaker 1.",
                    );
                    single_speaker_fallback()
                }
                Some(wav) => match diarize::diarize_file(&app, &wav, sys_speaker_hint).await {
                    Err(e) => {
                        eprintln!("diarize: sys diarize failed ({e}), falling back to single-speaker labels");
                        emit_error(
                            &app,
                            Some(&note_id),
                            &format!("Diarization failed for the remote side ({e}); remote speakers grouped under Speaker 1."),
                        );
                        single_speaker_fallback()
                    }
                    Ok(segments) if segments.is_empty() => {
                        eprintln!("diarize: sys diarize returned no segments, falling back to single-speaker labels");
                        single_speaker_fallback()
                    }
                    Ok(segments) => {
                        let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
                        Box::new(move |c: &ChunkRecord| match c.source {
                            ChunkSource::Mic => Some("You".to_string()),
                            ChunkSource::Sys => {
                                let sid = assign_speaker(c.start_ms, &segments)?;
                                display_map.get(sid).map(|n| format!("Speaker {n}"))
                            }
                        })
                    }
                },
            }
        }
        (false, false) => unreachable!("chunks.is_empty() returned earlier"),
    };

    let new_session = build_labelled_transcript(&chunks, label_for_chunk.as_ref());
    let combined = combine_with_snapshot(&snapshot, &new_session);
    if combined.trim().is_empty() {
        return Ok(());
    }
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::set_transcript(&conn, &note_id, &combined)?;
    }
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.clone(),
            text: combined,
        },
    );

    // Free the full.wav files ahead of the temp-dir cleanup. Best-effort.
    if let Some(p) = mic_wav { diarize::cleanup_full_wav(&p).await; }
    if let Some(p) = sys_wav { diarize::cleanup_full_wav(&p).await; }
    Ok(())
}

/// Re-transcribe the saved full WAV(s) end-to-end, then re-label using the
/// offline diarizer's speaker segments and the new whisper segment
/// timestamps. Replaces the live chunked transcript wholesale.
///
/// Why bother when chunked already produced a transcript: the live path
/// invokes Whisper once per VAD-bounded chunk (1.0–15s) with the previous
/// chunks' text fed back as `initial_prompt`. That is necessary for live
/// UX but two failure modes show up in the saved transcript:
///   1. Chunk-boundary cuts. A word straddling a 15s boundary gets sliced
///      and Whisper re-decodes the trailing fragment in the next chunk.
///   2. Loop amplification. A low-SNR chunk decoding "X X X" pollutes the
///      trail; the next chunk sees "X X X" as prior context and decodes
///      more of the same. Repetition collapse can run for the rest of
///      the recording.
/// Re-transcribing the full WAV at stop time gives Whisper its native
/// 30-second sliding window with internal context across the entire
/// recording. Effectively free on local (large-v3-turbo runs ~10× realtime
/// on Apple Silicon, so a 30-minute meeting re-transcribes in ~3 minutes
/// during the existing post-stop window).
///
/// Branches the same way as `diarize_and_apply`:
///   - mic only → diarize mic, label every whisper segment via segment time
///   - sys only → diarize sys, same pattern
///   - both → mic segments labelled `You:` (channel attribution); sys
///     segments diarized for remote-side speakers
///
/// No-ops gracefully when the model isn't downloaded, the user disabled
/// final_pass, the active provider isn't local, or no full WAV survived.
/// Failure leaves the live (chunked) transcript intact so the user never
/// loses content — they just don't get the cleanup pass.
async fn final_pass_apply(app: AppHandle, note_id: String) -> anyhow::Result<()> {
    // Setting + provider gate. Cloud OpenAI isn't supported yet because the
    // verbose_json segment-with-timestamp variant needs a separate request
    // path; planned for a follow-up.
    let (enabled, provider) = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let enabled = db::get_setting(&conn, "final_pass")?
            .unwrap_or_else(|| DEFAULT_FINAL_PASS.to_string());
        let provider = db::get_setting(&conn, "transcribe_provider")?
            .unwrap_or_else(|| DEFAULT_TRANSCRIBE_PROVIDER.to_string());
        (enabled, provider)
    };
    if enabled != "true" || provider != "local" {
        return Ok(());
    }

    // Pull paths + transcribe config in one DB pass so the long awaits
    // below don't hold any locks.
    let (mic_wav, sys_wav, snapshot, expected_speakers, language, whisper_preset, vocabulary) = {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let mic = session.mic_full_wav_path.lock().clone();
        let sys = session.sys_full_wav_path.lock().clone();
        let snap = session.transcript_at_start.lock().clone();
        drop(session);
        let conn = state.db.lock();
        let global_language = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note = db::get_note(&conn, &note_id)?;
        let lang = if note.language.trim().is_empty() {
            global_language
        } else {
            note.language
        };
        let hint = note.expected_speakers.filter(|n| *n > 0);
        let preset = db::get_setting(&conn, "whisper_preset")?
            .unwrap_or_else(|| DEFAULT_WHISPER_PRESET.to_string());
        let vocab = db::get_setting(&conn, "custom_vocabulary")?.unwrap_or_default();
        (mic, sys, snap, hint, lang, preset, vocab)
    };

    if mic_wav.is_none() && sys_wav.is_none() {
        // Sidecar SIGKILL'd before either full WAV got finalized. Live
        // transcript stays as written.
        eprintln!("final_pass: no full WAV present, skipping");
        return Ok(());
    }

    emit_status(&app, Some(&note_id), Phase::Retranscribing);

    let model_path = local_model_path(&app, &language).map_err(|e| anyhow::anyhow!(e))?;
    let (shared, use_gpu) = {
        let state: State<AppState> = app.state();
        (state.whisper.clone(), local_whisper_use_gpu_setting(&state))
    };
    let preset = local_whisper::Preset::from_setting(&whisper_preset);
    // No trail snapshot here — whisper's own 30s sliding window handles
    // context across the full file, and there's no prior chunk to
    // condition on.
    let prompt = build_initial_prompt(&vocabulary, None);

    let mic_segs: Vec<local_whisper::TextSegment> = if let Some(p) = mic_wav.as_ref() {
        local_whisper::transcribe_file_segments(
            shared.clone(),
            model_path.clone(),
            use_gpu,
            &language,
            prompt.as_deref(),
            preset,
            p,
        )
        .await?
    } else {
        Vec::new()
    };
    let sys_segs: Vec<local_whisper::TextSegment> = if let Some(p) = sys_wav.as_ref() {
        local_whisper::transcribe_file_segments(
            shared.clone(),
            model_path.clone(),
            use_gpu,
            &language,
            prompt.as_deref(),
            preset,
            p,
        )
        .await?
    } else {
        Vec::new()
    };

    // Convert whisper segments into ChunkRecord shape so we can reuse
    // build_labelled_transcript and assign_speaker as-is. Same data flow
    // as the chunked path; only the source of timing + text differs.
    let mut chunks: Vec<ChunkRecord> = Vec::with_capacity(mic_segs.len() + sys_segs.len());
    chunks.extend(mic_segs.into_iter().map(|s| ChunkRecord {
        source: ChunkSource::Mic,
        start_ms: s.start_ms,
        text: s.text,
    }));
    chunks.extend(sys_segs.into_iter().map(|s| ChunkRecord {
        source: ChunkSource::Sys,
        start_ms: s.start_ms,
        text: s.text,
    }));

    if chunks.is_empty() {
        eprintln!("final_pass: whisper returned zero segments, skipping");
        return Ok(());
    }

    let mic_present = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys_present = chunks.iter().any(|c| c.source == ChunkSource::Sys);

    // Skip diarization gracefully when the model isn't installed — drop to
    // a single label per stream rather than failing the whole final pass.
    let diarize_available = matches!(diarize::status(&app).await, Ok(s) if s.downloaded);

    type Labeller = dyn Fn(&ChunkRecord) -> Option<String> + Send;
    let label_for_chunk: Box<Labeller> = match (mic_present, sys_present) {
        (true, false) => {
            if diarize_available {
                let Some(wav) = mic_wav.clone() else {
                    eprintln!("final_pass: mic segments present but mic_full.wav missing");
                    return Ok(());
                };
                let segments = diarize::diarize_file(&app, &wav, expected_speakers).await?;
                if segments.is_empty() {
                    Box::new(|_: &ChunkRecord| Some("Speaker 1".to_string()))
                } else {
                    let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
                    Box::new(move |c: &ChunkRecord| {
                        let sid = assign_speaker(c.start_ms, &segments)?;
                        display_map.get(sid).map(|n| format!("Speaker {n}"))
                    })
                }
            } else {
                Box::new(|_: &ChunkRecord| Some("Speaker 1".to_string()))
            }
        }
        (false, true) => {
            if diarize_available {
                let Some(wav) = sys_wav.clone() else {
                    eprintln!("final_pass: sys segments present but sys_full.wav missing");
                    return Ok(());
                };
                let segments = diarize::diarize_file(&app, &wav, expected_speakers).await?;
                if segments.is_empty() {
                    Box::new(|_: &ChunkRecord| Some("Speaker 1".to_string()))
                } else {
                    let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
                    Box::new(move |c: &ChunkRecord| {
                        let sid = assign_speaker(c.start_ms, &segments)?;
                        display_map.get(sid).map(|n| format!("Speaker {n}"))
                    })
                }
            } else {
                Box::new(|_: &ChunkRecord| Some("Speaker 1".to_string()))
            }
        }
        (true, true) => {
            // Remote/hybrid: mic = "You" (channel attribution); sys gets
            // diarized for remote-side speakers. Same shape as the
            // chunked path.
            let sys_speaker_hint = expected_speakers.map(|n| (n - 1).max(1));
            let sys_segments = if diarize_available {
                if let Some(p) = sys_wav.as_ref() {
                    diarize::diarize_file(&app, p, sys_speaker_hint)
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            if sys_segments.is_empty() {
                Box::new(move |c: &ChunkRecord| match c.source {
                    ChunkSource::Mic => Some("You".to_string()),
                    ChunkSource::Sys => Some("Speaker 1".to_string()),
                })
            } else {
                let sys_display_map = build_display_map(&chunks, &sys_segments, ChunkSource::Sys);
                Box::new(move |c: &ChunkRecord| match c.source {
                    ChunkSource::Mic => Some("You".to_string()),
                    ChunkSource::Sys => {
                        let sid = assign_speaker(c.start_ms, &sys_segments)?;
                        sys_display_map.get(sid).map(|n| format!("Speaker {n}"))
                    }
                })
            }
        }
        (false, false) => unreachable!("chunks.is_empty() returned earlier"),
    };

    let new_session = build_labelled_transcript(&chunks, label_for_chunk.as_ref());
    let combined = combine_with_snapshot(&snapshot, &new_session);
    if combined.trim().is_empty() {
        return Ok(());
    }
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::set_transcript(&conn, &note_id, &combined)?;
    }
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.clone(),
            text: combined,
        },
    );

    if let Some(p) = mic_wav {
        diarize::cleanup_full_wav(&p).await;
    }
    if let Some(p) = sys_wav {
        diarize::cleanup_full_wav(&p).await;
    }
    Ok(())
}

/// Walk the chunks of a given source in order, assigning each a 1-indexed
/// display number based on the speaker_id its start_ms maps to. The map
/// is built up-front so the per-chunk label closure is allocation-free
/// and produces identical numbers on repeated lookups for the same
/// speaker_id.
fn build_display_map(
    chunks: &[ChunkRecord],
    segments: &[diarize::Segment],
    source: ChunkSource,
) -> std::collections::HashMap<String, u32> {
    let mut map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for chunk in chunks.iter().filter(|c| c.source == source) {
        if let Some(sid) = assign_speaker(chunk.start_ms, segments) {
            if !map.contains_key(sid) {
                let n = (map.len() as u32) + 1;
                map.insert(sid.to_string(), n);
            }
        }
    }
    map
}

/// Stitch the prior transcript snapshot to a freshly diarized session.
/// When the snapshot is empty, the new text wins outright. When both have
/// content, this offsets the new session's `Speaker N:` numbers past the
/// highest one already in the snapshot (so a resume doesn't collide
/// "Speaker 1" from session 1 with a different "Speaker 1" from session 2)
/// and joins them with a newline.
fn combine_with_snapshot(snapshot: &str, new_session: &str) -> String {
    let snap_trimmed = snapshot.trim_end();
    if snap_trimmed.is_empty() {
        return new_session.to_string();
    }
    let new_trimmed = new_session.trim();
    if new_trimmed.is_empty() {
        return snap_trimmed.to_string();
    }
    let offset = max_speaker_number(snap_trimmed);
    let offset_new = if offset > 0 {
        offset_speaker_numbers(new_trimmed, offset)
    } else {
        new_trimmed.to_string()
    };
    format!("{snap_trimmed}\n{offset_new}")
}

/// Highest N appearing in any line that starts with `Speaker N:`. Returns
/// 0 when none are found — useful for "should we offset?" checks.
fn max_speaker_number(text: &str) -> u32 {
    let mut max = 0u32;
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Speaker ") {
            // Read digits up to the colon; "Speaker 12: foo" → 12.
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                if n > max {
                    max = n;
                }
            }
        }
    }
    max
}

/// Rewrite `Speaker N:` line prefixes by adding `offset` to every N. Only
/// touches the literal pattern we emit ourselves (`^Speaker \d+: `), so
/// renamed speakers ("Michael:", "Wilma:") stay untouched and free text
/// that happens to contain "Speaker 1" mid-sentence isn't rewritten.
fn offset_speaker_numbers(text: &str, offset: u32) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(rest) = line.strip_prefix("Speaker ") {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after_digits = &rest[digits.len()..];
            if !digits.is_empty() && after_digits.starts_with(": ") {
                if let Ok(n) = digits.parse::<u32>() {
                    out.push_str(&format!("Speaker {}{}", n + offset, after_digits));
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

/// Map a chunk's start time to a speaker_id by checking which segment
/// contains it; falls back to the closest segment by edge distance when
/// the chunk lands in a gap (silence between turns, or before/after the
/// segmented region). Returns None only when `segments` is empty.
fn assign_speaker<'a>(chunk_start_ms: u64, segments: &'a [diarize::Segment]) -> Option<&'a str> {
    for seg in segments {
        if chunk_start_ms >= seg.start_ms && chunk_start_ms < seg.end_ms {
            return Some(&seg.speaker_id);
        }
    }
    segments
        .iter()
        .min_by_key(|s| {
            if chunk_start_ms < s.start_ms {
                s.start_ms - chunk_start_ms
            } else {
                chunk_start_ms.saturating_sub(s.end_ms)
            }
        })
        .map(|s| s.speaker_id.as_str())
}

/// Rebuild the transcript by walking chunks in chronological order and
/// emitting each one prefixed with its assigned label. Same-label runs
/// get a single space between chunks (continuation); label changes get
/// a newline + new prefix. Chunks the labeller declines to label
/// (returns `None`) get joined to whatever came before them with a
/// space, no prefix change — typically only happens when diarize
/// produces zero segments and we're degrading gracefully.
///
/// Chronological ordering uses `(start_ms, source)`. Mic and system
/// chunks each carry start_ms relative to their own stream's first
/// frame — close to but not exactly the same as global wall time
/// (the streams start within a few hundred ms of each other). The
/// tie-break preferring `Mic` reflects the typical UX assumption that
/// the user speaks first; in practice the imprecision is well below
/// the threshold a reader would notice.
fn build_labelled_transcript(
    chunks: &[ChunkRecord],
    label_for_chunk: &(dyn Fn(&ChunkRecord) -> Option<String> + Send),
) -> String {
    let mut sorted: Vec<&ChunkRecord> = chunks.iter().collect();
    sorted.sort_by_key(|c| {
        let source_rank = match c.source {
            ChunkSource::Mic => 0,
            ChunkSource::Sys => 1,
        };
        (c.start_ms, source_rank)
    });

    let mut output = String::new();
    let mut last_label: Option<String> = None;

    for chunk in sorted {
        let trimmed = chunk.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        match label_for_chunk(chunk) {
            Some(label) => {
                if last_label.as_deref() != Some(label.as_str()) {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&format!("{label}: "));
                    last_label = Some(label);
                } else {
                    output.push(' ');
                }
            }
            None => {
                if !output.is_empty() {
                    output.push(' ');
                }
            }
        }
        output.push_str(trimmed);
    }
    output
}

async fn transcribe_chunk(
    app: AppHandle,
    note_id: String,
    source: ChunkSource,
    path: PathBuf,
    start_ms: u64,
) -> anyhow::Result<()> {
    let cfg = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let provider = db::get_setting(&conn, "transcribe_provider")?
            .unwrap_or_else(|| DEFAULT_TRANSCRIBE_PROVIDER.to_string());
        // Per-note language wins over the global. Empty (pre-feature notes
        // and the "use default" sentinel) falls back to the global setting.
        let global_language = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note_language = db::get_note(&conn, &note_id)
            .map(|n| n.language)
            .unwrap_or_default();
        let language = if note_language.trim().is_empty() {
            global_language
        } else {
            note_language
        };
        // Cloud providers need a key; local Whisper does not.
        let api_key = match provider.as_str() {
            "local" => String::new(),
            _ => db::get_setting(&conn, API_KEY)?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("no OpenAI API key"))?,
        };
        let openai_model = db::get_setting(&conn, "transcribe_model")?
            .unwrap_or_else(|| DEFAULT_TRANSCRIBE_MODEL.to_string());
        let whisper_preset = db::get_setting(&conn, "whisper_preset")?
            .unwrap_or_else(|| DEFAULT_WHISPER_PRESET.to_string());
        let vocabulary = db::get_setting(&conn, "custom_vocabulary")?
            .unwrap_or_default();
        TranscribeCfg {
            provider,
            api_key,
            language,
            openai_model,
            whisper_preset,
            vocabulary,
        }
    };

    // Skip near-silent chunks. Whisper and gpt-4o-transcribe both hallucinate
    // confident text (often in the wrong language) when fed silence. The WAV
    // chunks are 16kHz mono 16-bit PCM little-endian — read the data section
    // and compute RMS in [0, 1].
    if let Ok(rms) = wav::rms(&path).await {
        // Threshold tuned empirically: pure silence ~0.0001, room tone ~0.001,
        // soft speech ~0.01+. 0.003 cuts silence/room without clipping speech.
        if rms < 0.003 {
            return Ok(());
        }
    }

    // Serialize transcription per session: each chunk's initial_prompt must
    // see the *committed* trail of every prior chunk. With parallel
    // transcribes, two back-to-back chunks both grab the same stale snapshot
    // and the trail's quality benefit collapses. Sequential trades a little
    // throughput (chunks queue if inference is slow) for accurate context.
    let gate = {
        let state: State<AppState> = app.state();
        state.transcribe_gate.clone()
    };
    let _guard = gate.lock().await;

    // Whisper's `initial_prompt` slot conditions decoding on prior context.
    // We compose two parts: the user's custom vocabulary (proper-noun bias)
    // and a snapshot of the last ~150 committed words from THIS source's
    // stream. Per-source trails because the mic and system streams are
    // separate conversations — sharing one trail would pull a mic chunk's
    // decode toward remote-side vocabulary (or vice versa) and cause
    // language drift on bilingual calls.
    let trail_snapshot = {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let trail = match source {
            ChunkSource::Mic => session.mic_trail.lock(),
            ChunkSource::Sys => session.sys_trail.lock(),
        };
        trail.as_prompt()
    };
    let prompt = build_initial_prompt(&cfg.vocabulary, trail_snapshot);

    let text = match cfg.provider.as_str() {
        "local" => {
            let model_path = local_model_path(&app, &cfg.language)
                .map_err(|e| anyhow::anyhow!(e))?;
            let (shared, use_gpu) = {
                let state: State<AppState> = app.state();
                (state.whisper.clone(), local_whisper_use_gpu_setting(&state))
            };
            let preset = local_whisper::Preset::from_setting(&cfg.whisper_preset);
            local_whisper::transcribe_file(
                shared,
                model_path,
                use_gpu,
                &cfg.language,
                prompt.as_deref(),
                preset,
                &path,
            )
            .await?
        }
        _ => {
            openai::transcribe_file(
                &cfg.api_key,
                &cfg.openai_model,
                Some(&cfg.language),
                prompt.as_deref(),
                &path,
            )
            .await?
        }
    };
    if is_likely_hallucination(&text, &cfg.language) {
        return Ok(());
    }
    // Drop chunks dominated by N-gram repetition (Whisper collapse). Letting
    // them land in the transcript is bad on its own, but worse: the trail-
    // prompt feeds the loop forward into the next chunk's `initial_prompt`
    // and the loop self-sustains for the rest of the recording.
    if is_repetition_collapse(&text) {
        eprintln!("transcribe: dropping repetition-collapsed chunk");
        return Ok(());
    }
    // Whisper was trained on closed-caption data and frequently appends
    // subtitle attribution ("Undertekster av Ai-Media", "Subtitles by Amara",
    // "Thanks for watching") at the end of real speech. Trim those tails.
    let text = strip_attribution_tail(&text);
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Ok(());
    }

    // Speaker prefixes are added by the offline diarization pass on
    // recording_stop, not here. Per-chunk live diarization performed
    // poorly on long recordings (clustering drifts as speaker memory
    // accumulates), so chunks are appended as plain text and the full
    // transcript is rewritten with proper labels after stop, when
    // FluidAudio can cluster across the entire audio at once and we can
    // assign "You" to mic chunks vs diarized speakers to system chunks.
    //
    // The live-display transcript appends in arrival order regardless of
    // source. Mic and sys chunks may interleave slightly out of strict
    // wall-clock order during recording, but `diarize_and_apply` rebuilds
    // the transcript from the chunk log sorted by (source, start_ms) at
    // stop time, so the saved transcript ends up properly ordered.
    let state: State<AppState> = app.state();
    // Session-active guard. The provider call above (whisper / openai) can
    // take long enough that recording_stop fires while we're still awaiting
    // it. If the session has been cleared (note_id taken in recording_stop)
    // or replaced (user started a new recording), this chunk's text would
    // append onto a transcript the post-stop chain has already rewritten,
    // pasting raw text past the labelled output. Bail instead.
    {
        let session = state.recording.lock();
        if session.note_id.as_deref() != Some(&note_id) {
            eprintln!("transcribe: session no longer active for note, dropping chunk");
            return Ok(());
        }
    }
    let updated_transcript = {
        let conn = state.db.lock();
        db::append_transcript(&conn, &note_id, &trimmed, " ")?
    };
    {
        let session = state.recording.lock();
        let mut trail = match source {
            ChunkSource::Mic => session.mic_trail.lock(),
            ChunkSource::Sys => session.sys_trail.lock(),
        };
        trail.push(&trimmed);
        session.chunk_log.lock().push(ChunkRecord {
            source,
            start_ms,
            text: trimmed.clone(),
        });
    }
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.clone(),
            text: updated_transcript,
        },
    );
    Ok(())
}

struct TranscribeCfg {
    provider: String,
    api_key: String,
    language: String,
    openai_model: String,
    whisper_preset: String,
    vocabulary: String,
}

// Resolved provider for a single polish or summary call. Both cloud OpenAI
// and any local OpenAI-compatible server (Ollama, LM Studio, llama-server,
// vLLM) flow through this same shape — the only difference is `base_url`.
struct ResolvedProvider {
    base_url: String,
    api_key: String,
    model: String,
    // Ollama-only knob: enable thinking mode (Qwen 3+ <think>...</think>
    // chain-of-thought). Default off; flipping it on lets users A/B the
    // quality difference in dev. Ignored for cloud + non-Ollama servers.
    think: bool,
}

// Decide whether this note's polish/summary call should hit cloud OpenAI or
// a local OpenAI-compatible server. Note-level override beats the global
// setting; default is openai.
//
// For local: reads `local_llm_base_url` and `local_llm_model` from settings.
// `api_key` is forwarded as-is — local servers typically ignore it but
// Ollama requires a non-empty bearer string, so we send a sentinel.
fn resolve_provider(
    conn: &rusqlite::Connection,
    note: &Note,
) -> anyhow::Result<ResolvedProvider> {
    let note_override = note.summary_provider.trim();
    let provider = if note_override.is_empty() {
        db::get_setting(conn, "summary_provider")
            .ok()
            .flatten()
            .unwrap_or_else(|| "openai".into())
    } else {
        note_override.to_string()
    };
    eprintln!(
        "[llm] resolve_provider: note={} note_override={:?} effective={}",
        note.id, note_override, provider
    );

    match provider.as_str() {
        "local" => {
            let base_url = db::get_setting(conn, "local_llm_base_url")?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_LOCAL_LLM_BASE_URL.to_string());
            let model = db::get_setting(conn, "local_llm_model")?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!(
                    "local LLM model not configured — pick one in Settings"
                ))?;
            let think = db::get_setting(conn, "local_llm_think")?
                .map(|s| s.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            eprintln!("[llm] resolved local: url={base_url} model={model} think={think}");
            Ok(ResolvedProvider {
                base_url,
                api_key: "humla-local".into(),
                model,
                think,
            })
        }
        _ => {
            let api_key = db::get_setting(conn, API_KEY)?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("OpenAI API key not set"))?;
            let model = db::get_setting(conn, "summary_model")?
                .unwrap_or_else(|| DEFAULT_SUMMARY_MODEL.to_string());
            eprintln!("[llm] resolved openai: model={model}");
            Ok(ResolvedProvider {
                base_url: openai::BASE.into(),
                api_key,
                model,
                think: false,
            })
        }
    }
}

#[tauri::command]
pub async fn summarize_note(app: AppHandle, note_id: String) -> Result<(), String> {
    eprintln!("[llm] summarize_note invoked for note={note_id}");
    // Reflect the in-flight summary in the recording status so the UI can
    // show a spinner. Use the existing Summarizing phase.
    emit_status(&app, Some(&note_id), Phase::Summarizing);
    let result = run_summary(app.clone(), note_id.clone()).await;
    emit_status(&app, None, Phase::Idle);
    match &result {
        Ok(()) => eprintln!("[llm] summarize_note succeeded"),
        Err(e) => eprintln!("[llm] summarize_note failed: {e:#}"),
    }
    result.map_err(|e| e.to_string())
}

/// User-triggered polish. **Cloud-only by design** — polish is a fast,
/// cheap cleanup that takes seconds on OpenAI but several minutes on a
/// local Qwen 3.5:9B (often with thinking-mode loops on top). The per-note
/// `summary_provider` override applies to *summaries*; polish always uses
/// the OpenAI cloud provider regardless of that setting. Errors clearly
/// when no OpenAI API key is configured.
#[tauri::command]
pub async fn polish_note(app: AppHandle, note_id: String) -> Result<(), String> {
    let provider = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let n = db::get_note(&conn, &note_id).map_err(err)?;
        if n.transcript.trim().is_empty() {
            return Err("Nothing to polish — transcript is empty.".to_string());
        }
        let api_key = db::get_setting(&conn, API_KEY)
            .map_err(err)?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "Polish requires an OpenAI API key. Add one in Settings to enable polish."
                    .to_string()
            })?;
        let model = db::get_setting(&conn, "summary_model")
            .map_err(err)?
            .unwrap_or_else(|| DEFAULT_SUMMARY_MODEL.to_string());
        ResolvedProvider {
            base_url: openai::BASE.into(),
            api_key,
            model,
            think: false,
        }
    };

    let app_for_task = app.clone();
    let note_for_task = note_id.clone();
    tokio::spawn(async move {
        if let Err(e) =
            polish_transcript_inner(app_for_task.clone(), note_for_task.clone(), provider).await
        {
            eprintln!("manual polish failed: {e:#}");
            emit_error(
                &app_for_task,
                Some(&note_for_task),
                &format!("Polish failed: {e}"),
            );
        }
        emit_status(&app_for_task, None, Phase::Idle);
    });
    Ok(())
}

// Auto-polish entry point used by recording_stop. Resolves the configured
// summary provider and skips on local — local polish would block the user's
// subsequent Summarize click for several minutes. Manual polish_note builds
// its own cloud-only provider and calls polish_transcript_inner directly.
async fn polish_transcript(app: AppHandle, note_id: String) -> anyhow::Result<()> {
    let provider = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let n = match db::get_note(&conn, &note_id) {
            Ok(n) => n,
            Err(_) => return Ok(()),
        };
        if n.transcript.trim().is_empty() {
            return Ok(());
        }
        match resolve_provider(&conn, &n) {
            Ok(p) if p.base_url == openai::BASE => p,
            Ok(_) => {
                eprintln!("[llm] auto-polish skipped (local provider): note={note_id}");
                return Ok(());
            }
            Err(_) => return Ok(()),
        }
    };
    polish_transcript_inner(app, note_id, provider).await
}

// Polish a freshly-recorded transcript via a chat-completion pass. Whisper's
// raw output is usually correct in substance but littered with typos,
// chunk-boundary mid-word cuts ("mistenkte" → "mistred"), and missing
// punctuation. The user's notes + custom vocabulary are passed as context so
// the model spells proper nouns and domain terms correctly.
//
// Provider is supplied by the caller so polish_note (manual) can force
// OpenAI cloud while polish_transcript (auto) follows the configured
// summary provider. Skips silently when the transcript was modified
// between the snapshot read and the polished write — the user started
// another recording on the same note while polish was in flight, and we
// don't want to clobber freshly-appended chunks.
async fn polish_transcript_inner(
    app: AppHandle,
    note_id: String,
    provider: ResolvedProvider,
) -> anyhow::Result<()> {
    let (transcript_snapshot, body, vocabulary) = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let n = db::get_note(&conn, &note_id)?;
        if n.transcript.trim().is_empty() {
            return Ok(()); // nothing to polish
        }
        let vocab = db::get_setting(&conn, "custom_vocabulary")?.unwrap_or_default();
        (n.transcript.clone(), n.body.clone(), vocab)
    };

    let body_text = html_to_text(&body);
    let vocab_section = if vocabulary.trim().is_empty() {
        String::new()
    } else {
        format!("[Vocabulary]\n{}\n\n", vocabulary.trim())
    };
    let notes_section = if body_text.trim().is_empty() {
        String::new()
    } else {
        format!("[Notes]\n{}\n\n", body_text.trim())
    };
    let user_message =
        format!("{vocab_section}{notes_section}[Raw transcript]\n{transcript_snapshot}");

    emit_status(&app, Some(&note_id), Phase::Polishing);
    let polished = openai::summarize_with_base(
        &provider.base_url,
        &provider.api_key,
        &provider.model,
        provider.think,
        DEFAULT_POLISH_PROMPT,
        &user_message,
        |_| {}, // polish never runs on local, no streaming UI
    )
    .await?;
    let polished = polished.trim().to_string();
    if polished.is_empty() {
        return Ok(());
    }

    // Concurrency guard: if the transcript changed under us (user started a
    // new recording on the same note before polish finished), keep their
    // raw additions instead of clobbering with the snapshot's polished
    // version.
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let current = db::get_note(&conn, &note_id)?;
        if current.transcript != transcript_snapshot {
            return Ok(());
        }
        db::update_note(
            &conn,
            &note_id,
            &NotePatch {
                transcript: Some(polished.clone()),
                ..Default::default()
            },
        )?;
    }

    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id,
            text: polished,
        },
    );
    Ok(())
}

async fn run_summary(app: AppHandle, note_id: String) -> anyhow::Result<()> {
    let (provider, custom_prompt, language, note) = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let n = db::get_note(&conn, &note_id)?;
        let p_resolved = resolve_provider(&conn, &n)?;
        let p = db::get_setting(&conn, "summary_prompt")?
            .unwrap_or_else(|| DEFAULT_SUMMARY_PROMPT.to_string());
        let global_lang = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        // Same fallback rule as transcription: note language wins, empty
        // means "follow the global default".
        let lang = if n.language.trim().is_empty() {
            global_lang
        } else {
            n.language.clone()
        };
        (p_resolved, p, lang, n)
    };
    if note.transcript.trim().is_empty() && note.body.trim().is_empty() {
        return Ok(());
    }
    // Resolve the prompt for this note. Three cases, in priority order:
    //   1. `custom:<id>` — a user-defined prompt row. Look it up; if the
    //      row was deleted out from under us, fall back to the legacy
    //      single-prompt setting so the summary still runs.
    //   2. `"custom"` — the legacy single-prompt sentinel from before
    //      the summary_prompts table. Reads the `summary_prompt` setting.
    //      Old notes that didn't get migrated land here.
    //   3. Built-in preset value ("meeting", "lecture", etc.) —
    //      language-aware via presets::prompt.
    let prompt = if let Some(id) = note.summary_preset.strip_prefix("custom:") {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        match db::get_summary_prompt(&conn, id) {
            Ok(p) => p.content,
            Err(_) => custom_prompt,
        }
    } else if note.summary_preset == "custom" {
        custom_prompt
    } else {
        presets::prompt(&note.summary_preset, &language)
    };
    let body_text = html_to_text(&note.body);
    // Always send both labels even when one side is empty. Sending only
    // [Transkripsjon] while the system prompt references [Notater] sends
    // thinking models down a rabbit hole second-guessing whether the notes
    // are missing or hidden — a real failure mode we observed in dev where
    // Qwen 3.5 spent thousands of reasoning tokens on it. Explicit "(ingen)"
    // tells the model the field is genuinely absent.
    let notes_block = if body_text.trim().is_empty() {
        "[Notater]\n(ingen)".to_string()
    } else {
        format!("[Notater]\n{body_text}")
    };
    let transcript_block = if note.transcript.trim().is_empty() {
        "[Transkripsjon]\n(ingen)".to_string()
    } else {
        format!("[Transkripsjon]\n{}", note.transcript)
    };
    let user_message = format!("{notes_block}\n\n{transcript_block}");
    // Hard language directive in case the prompt was authored in a different
    // language than the user has now chosen.
    let full_prompt = format!("{prompt}\n\n{}", language_directive(&language));
    // Stream thinking + content deltas to the frontend so the user sees
    // the model working in real time. Especially valuable when think=true
    // on Qwen 3.5+ — without live feedback users wait minutes wondering if
    // it's stuck.
    let app_for_stream = app.clone();
    let note_for_stream = note_id.clone();
    let summary = openai::summarize_with_base(
        &provider.base_url,
        &provider.api_key,
        &provider.model,
        provider.think,
        &full_prompt,
        &user_message,
        move |chunk| match chunk {
            openai::StreamChunk::Thinking(t) => {
                let _ = app_for_stream.emit(
                    "summary_thinking_delta",
                    StreamDeltaPayload {
                        note_id: note_for_stream.clone(),
                        delta: t.to_string(),
                    },
                );
            }
            openai::StreamChunk::Content(c) => {
                let _ = app_for_stream.emit(
                    "summary_content_delta",
                    StreamDeltaPayload {
                        note_id: note_for_stream.clone(),
                        delta: c.to_string(),
                    },
                );
            }
        },
    )
    .await?;
    let state: State<AppState> = app.state();
    {
        let conn = state.db.lock();
        db::update_note(&conn, &note_id, &NotePatch {
            summary: Some(summary.clone()),
            ..Default::default()
        })?;
    }
    let _ = app.emit("summary_ready", SummaryPayload { note_id, summary });
    Ok(())
}

fn emit_status(app: &AppHandle, note_id: Option<&str>, phase: Phase) {
    let _ = app.emit("recording_status", RecordingStatus {
        note_id: note_id.map(|s| s.to_string()),
        phase,
    });
}

fn emit_error(app: &AppHandle, note_id: Option<&str>, message: &str) {
    let _ = app.emit("recording_error", ErrorPayload {
        note_id: note_id.map(|s| s.to_string()),
        message: message.to_string(),
    });
}

fn sidecar_path(_app: &AppHandle) -> Result<PathBuf, String> {
    // 1) Production / `tauri build`: Tauri copies external binaries next to
    //    the main executable inside the .app bundle's MacOS folder, with the
    //    triple suffix stripped. So look for ../MacOS/audio-capture first.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidates = [
                dir.join("audio-capture"),
                dir.join("audio-capture-aarch64-apple-darwin"),
                dir.join("audio-capture-x86_64-apple-darwin"),
            ];
            for c in candidates {
                if c.exists() {
                    return Ok(c);
                }
            }
        }
    }

    // 2) Dev (`tauri dev`): the binary lives under src-tauri/binaries/.
    if let Ok(cwd) = std::env::current_dir() {
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let p = cwd.join(format!("src-tauri/binaries/audio-capture-{triple}"));
            if p.exists() { return Ok(p); }
            let p = cwd.join(format!("binaries/audio-capture-{triple}"));
            if p.exists() { return Ok(p); }
        }
    }

    Err("audio-capture sidecar not found".into())
}

// Strip Tiptap-emitted HTML to plain text, preserving paragraph and list
// structure so the summarizer sees the user's note shape. Not a full HTML
// parser — only handles the small set of tags Tiptap produces.
fn html_to_text(html: &str) -> String {
    let s = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n")
        .replace("</li>", "\n")
        .replace("</h1>", "\n")
        .replace("</h2>", "\n")
        .replace("</h3>", "\n")
        .replace("</h4>", "\n")
        .replace("</blockquote>", "\n")
        .replace("<li>", "- ");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    // Collapse runs of 3+ newlines down to 2 so paragraph breaks survive
    // but the model doesn't see endless whitespace.
    let mut collapsed = String::with_capacity(out.len());
    let mut nl = 0;
    for c in out.chars() {
        if c == '\n' {
            nl += 1;
            if nl <= 2 {
                collapsed.push(c);
            }
        } else {
            nl = 0;
            collapsed.push(c);
        }
    }
    collapsed.trim().to_string()
}

// Hard directive appended to the summary system prompt. Enforces output
// language regardless of which language the user wrote their prompt in.
fn language_directive(lang: &str) -> String {
    // Native imperatives for the common Nordic codes — the model picks up
    // the target language faster from a directive written in that language
    // than from "Write in Norwegian." in English. Everything else falls back
    // to a generated "IMPORTANT: Write the entire response in {Name}." using
    // the English language name from the shared lookup.
    match lang {
        "no" => "VIKTIG: Skriv hele svaret på norsk.".to_string(),
        "sv" => "VIKTIGT: Skriv hela svaret på svenska.".to_string(),
        "da" => "VIGTIGT: Skriv hele svaret på dansk.".to_string(),
        "auto" => "Respond in the same language as the user's notes.".to_string(),
        other => format!(
            "IMPORTANT: Write the entire response in {}.",
            languages::english_name(other)
        ),
    }
}

// Trim and dedupe the user's custom vocabulary into a free-text prompt for
// Whisper-family models. Whisper treats the prompt as the previous turn it
// continues from, so a comma-separated list of names/jargon biases decoding
// toward those tokens. Returns None when the vocabulary is empty.
fn vocabulary_prompt(raw: &str) -> Option<String> {
    let items: Vec<&str> = raw
        .split(|c: char| c == ',' || c == '\n')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(items.join(", "))
}

// Compose the `initial_prompt` for a Whisper-family transcription call out of
// the user's static vocabulary and the rolling tail of committed transcript
// from the current session. Either part may be empty; if both are, the
// caller should pass `None`.
//
// Budget note: Whisper's prompt context is ~224 tokens. Vocabulary is
// typically <50 tokens; the trail is bounded to 150 words (~200 tokens).
// Slight overflow is tolerated — whisper.cpp truncates internally.
fn build_initial_prompt(vocabulary: &str, trail: Option<String>) -> Option<String> {
    let vocab = vocabulary_prompt(vocabulary);
    match (vocab, trail) {
        (None, None) => None,
        (Some(v), None) => Some(v),
        (None, Some(t)) => Some(t),
        (Some(v), Some(t)) => Some(format!("{v}\n\n{t}")),
    }
}

// Whisper's training data contained millions of subtitle files, so it
// regularly appends "Subtitles by …" / "Undertekster av …" / "Thanks for
// watching" at the end of real speech. If we see one of these markers
// anywhere in the text, strip it back to the preceding sentence boundary.
fn strip_attribution_tail(text: &str) -> String {
    // Triggers are ASCII so to_ascii_lowercase keeps byte offsets aligned
    // with the original string for slicing.
    let lower = text.to_ascii_lowercase();
    const TRIGGERS: &[&str] = &[
        // Norwegian/Scandinavian subtitle credits. Whisper memorised whole
        // sign-off phrases from broadcast subtitles, so each verb form needs
        // its own trigger — past-participle ("tekstet"), gerund ("teksting"),
        // and noun form ("tekster") all show up in the wild.
        "undertekster av",
        "undertekstet av",
        "tekstet av",
        "tekster av",
        "teksting av",
        "norske tekster",
        "oversatt av",
        "oversettelse av",
        // English subtitle credits
        "subtitles by",
        "subtitled by",
        "captions by",
        "captioning by",
        "closed captions",
        "translation by",
        "translated by",
        "transcribed by",
        "amara.org",
        "ai-media",
        // YouTube-style sign-offs
        "thanks for watching",
        "thank you for watching",
        "subscribe to",
        "like and subscribe",
        "see you next time",
        "see you in the next",
    ];
    let mut cut: Option<usize> = None;
    for trigger in TRIGGERS {
        if let Some(pos) = lower.rfind(trigger) {
            // Back up to the nearest sentence boundary before the trigger so
            // we drop the whole offending phrase, not just the trigger word.
            let start = text[..pos]
                .rfind(|c: char| matches!(c, '.' | '!' | '?' | '\n'))
                .map(|p| p + 1)
                .unwrap_or(pos);
            cut = Some(cut.map_or(start, |c| c.min(start)));
        }
    }
    match cut {
        Some(c) => text[..c].trim_end().to_string(),
        None => text.to_string(),
    }
}

// Whisper produces a small set of stock English phrases when fed silence
// regardless of the `language` parameter. Drop them when:
//   - the chunk is short (≤120 chars, typical of a hallucinated standalone
//     phrase) AND
//   - the chosen target language is not English (so we don't eat a real
//     English meeting that happens to say "thanks for watching this demo").
// We err on the side of keeping content; the silence gate above is the
// primary defense.
fn is_likely_hallucination(text: &str, language: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    if language == "en" || t.len() > 120 {
        return false;
    }
    let lower = t.to_lowercase();
    const FRAGMENTS: &[&str] = &[
        "thanks for watching",
        "thank you for watching",
        "subscribe to",
        "subtitles by",
        "subtitled by",
        "amara.org",
        "transcribed by",
    ];
    FRAGMENTS.iter().any(|f| lower.contains(f))
}

/// Detect a chunk whose output is dominated by N-gram repetition — Whisper's
/// well-known low-SNR failure mode where one phrase decodes ≥3 consecutive
/// times. The contaminated chunk should be dropped; if it lands in the
/// transcript, the trail-prompt mechanism then feeds the loop into the next
/// chunk and the recording's tail becomes unrecoverable.
///
/// Heuristic: scan phrase lengths 1..=7. For each, look for the longest
/// run of consecutive identical (case-insensitive, punctuation-stripped)
/// occurrences. Flag the chunk when:
///   - some phrase repeats ≥4 times in a row, OR
///   - some phrase repeats ≥3 times AND covers ≥60% of the chunk's words.
///
/// The double rule keeps "yes yes yes" or "ja ja ja" mid-conversation from
/// being dropped — a 3-rep tiny chunk is plausibly real speech, but a 3-rep
/// run dominating a longer chunk is collapse.
fn is_repetition_collapse(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let n = words.len();
    if n < 6 {
        return false;
    }
    for phrase_len in 1..=7 {
        if n < phrase_len * 3 {
            continue;
        }
        let mut start = 0;
        while start + phrase_len <= n {
            let mut reps = 1;
            let mut pos = start + phrase_len;
            while pos + phrase_len <= n
                && words[pos..pos + phrase_len] == words[start..start + phrase_len]
            {
                reps += 1;
                pos += phrase_len;
            }
            if reps >= 4 {
                return true;
            }
            if reps >= 3 && (phrase_len * reps) * 5 >= n * 3 {
                return true;
            }
            start = pos.max(start + 1);
        }
    }
    false
}

#[cfg(test)]
mod diarize_tests {
    use super::*;
    use crate::diarize::Segment;

    fn seg(start_ms: u64, end_ms: u64, sid: &str) -> Segment {
        Segment { start_ms, end_ms, speaker_id: sid.to_string() }
    }

    fn mic(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord { source: ChunkSource::Mic, start_ms, text: text.to_string() }
    }

    fn sys(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord { source: ChunkSource::Sys, start_ms, text: text.to_string() }
    }

    /// Build the same labeller `diarize_and_apply` would build in the
    /// mic-only branch: every chunk gets `Speaker N:` from its segment.
    /// Pulled into a test helper so we can exercise `build_labelled_transcript`
    /// without mocking the sidecar.
    fn mic_only_labeller(
        chunks: Vec<ChunkRecord>,
        segments: Vec<Segment>,
    ) -> String {
        let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
        let labeller = move |c: &ChunkRecord| {
            let sid = assign_speaker(c.start_ms, &segments)?;
            display_map.get(sid).map(|n| format!("Speaker {n}"))
        };
        build_labelled_transcript(&chunks, &labeller)
    }

    #[test]
    fn assign_speaker_inside_segment() {
        let segs = vec![seg(0, 5000, "A"), seg(5000, 10000, "B")];
        assert_eq!(assign_speaker(2500, &segs), Some("A"));
        assert_eq!(assign_speaker(5000, &segs), Some("B"));
        assert_eq!(assign_speaker(9999, &segs), Some("B"));
    }

    #[test]
    fn assign_speaker_in_gap_uses_closest() {
        // Gap from 5000-7000. Chunk at 5500 is closer to A (gap edge 5000)
        // than to B (gap edge 7000), so falls back to A.
        let segs = vec![seg(0, 5000, "A"), seg(7000, 10000, "B")];
        assert_eq!(assign_speaker(5500, &segs), Some("A"));
        assert_eq!(assign_speaker(6800, &segs), Some("B"));
    }

    #[test]
    fn assign_speaker_before_first_segment() {
        let segs = vec![seg(2000, 5000, "A")];
        assert_eq!(assign_speaker(500, &segs), Some("A"));
    }

    #[test]
    fn assign_speaker_empty_segments() {
        let segs: Vec<Segment> = vec![];
        assert_eq!(assign_speaker(1000, &segs), None);
    }

    #[test]
    fn build_transcript_empty_chunks() {
        assert_eq!(mic_only_labeller(vec![], vec![seg(0, 1000, "A")]), "");
    }

    #[test]
    fn build_transcript_single_speaker_runs() {
        // Three chunks all from speaker A — no newline, single-space joins.
        let chunks = vec![mic(0, "hello"), mic(2000, "world"), mic(5000, "again")];
        assert_eq!(
            mic_only_labeller(chunks, vec![seg(0, 10000, "A")]),
            "Speaker 1: hello world again"
        );
    }

    #[test]
    fn build_transcript_speaker_switch_inserts_newline_and_prefix() {
        let chunks = vec![
            mic(0, "first turn"),
            mic(3500, "second turn"),
            mic(7000, "third turn"),
        ];
        let segs = vec![seg(0, 3000, "A"), seg(3000, 6000, "B"), seg(6000, 9000, "A")];
        // Display numbers assigned in first-encounter order: A=1, B=2.
        // A returns later → "Speaker 1:" again, not a new number.
        assert_eq!(
            mic_only_labeller(chunks, segs),
            "Speaker 1: first turn\nSpeaker 2: second turn\nSpeaker 1: third turn"
        );
    }

    #[test]
    fn build_transcript_skips_empty_chunks() {
        let chunks = vec![mic(0, "real text"), mic(1000, "   "), mic(2000, "more")];
        assert_eq!(
            mic_only_labeller(chunks, vec![seg(0, 5000, "A")]),
            "Speaker 1: real text more"
        );
    }

    #[test]
    fn build_transcript_remote_call_mic_is_you_sys_is_diarized() {
        // Remote-call shape: mic chunks get fixed "You" label; sys chunks
        // get diarized. Ordering by (start_ms, source) interleaves them.
        let chunks = vec![
            mic(0, "hi there"),
            sys(500, "hello"),
            mic(2500, "how are you"),
            sys(4000, "doing well"),
        ];
        let sys_segs = vec![seg(0, 10000, "REMOTE_A")];
        let display_map = build_display_map(&chunks, &sys_segs, ChunkSource::Sys);
        let labeller = move |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => assign_speaker(c.start_ms, &sys_segs)
                .and_then(|sid| display_map.get(sid).map(|n| format!("Speaker {n}"))),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &labeller),
            "You: hi there\nSpeaker 1: hello\nYou: how are you\nSpeaker 1: doing well"
        );
    }

    #[test]
    fn hybrid_fallback_keeps_sys_chunks_distinct_from_mic() {
        // Reproduces the silent-merge bug: in the (mic+sys) branch when
        // diarize is unavailable for the sys stream, sys chunks must NOT
        // get a None label — that would glue their text onto the previous
        // `You:` line, hiding remote speech inside the user's transcript.
        // The single-speaker fallback labels them `Speaker 1` so the
        // boundary survives.
        let chunks = vec![
            mic(0, "ok thanks"),
            sys(500, "you got it"),
            mic(2000, "see you tomorrow"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &labeller),
            "You: ok thanks\nSpeaker 1: you got it\nYou: see you tomorrow"
        );
    }

    #[test]
    fn label_returning_none_glues_to_previous_label_dont_use_for_distinct_speakers() {
        // Documents the underlying behavior the fallback above protects
        // against. With the buggy labeller (sys → None) the remote text
        // appears inside the user's `You:` line — silent data loss for
        // the reader. Locked into a test so a future "simplification" of
        // the fallback that goes back to None gets caught here.
        let chunks = vec![
            mic(0, "ok thanks"),
            sys(500, "you got it"),
        ];
        let buggy = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => None,
        };
        let result = build_labelled_transcript(&chunks, &buggy);
        // This is the pathological output we DO NOT want from the
        // production code; it's only here as a tripwire on the helper.
        assert_eq!(result, "You: ok thanks you got it");
    }

    #[test]
    fn build_transcript_orders_by_start_ms_with_mic_priority_on_tie() {
        // Mic and sys chunks at the same start_ms — mic is emitted first.
        // Reflects the typical UX assumption that the user speaks before
        // they hear a response, and stabilises ordering on tie.
        let chunks = vec![
            sys(0, "from sys"),
            mic(0, "from mic"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &labeller),
            "You: from mic\nSpeaker 1: from sys"
        );
    }

    #[test]
    fn max_speaker_number_finds_highest() {
        let text = "Speaker 1: hi\nSpeaker 2: hello\nSpeaker 1: again";
        assert_eq!(max_speaker_number(text), 2);
    }

    #[test]
    fn max_speaker_number_zero_when_no_labels() {
        assert_eq!(max_speaker_number("just plain text"), 0);
        assert_eq!(max_speaker_number("Michael: hi\nWilma: hello"), 0);
        assert_eq!(max_speaker_number(""), 0);
    }

    #[test]
    fn max_speaker_number_handles_multi_digit() {
        let text = "Speaker 1: hi\nSpeaker 12: hello";
        assert_eq!(max_speaker_number(text), 12);
    }

    #[test]
    fn offset_speaker_numbers_adds_offset() {
        let text = "Speaker 1: hi\nSpeaker 2: hello";
        assert_eq!(
            offset_speaker_numbers(text, 3),
            "Speaker 4: hi\nSpeaker 5: hello"
        );
    }

    #[test]
    fn offset_speaker_numbers_preserves_renamed() {
        // Only literal "Speaker N:" prefixes get rewritten; renamed lines
        // and free-text mentions stay untouched.
        let text = "Michael: hi\nWilma: hello\nSpeaker 1 was great";
        assert_eq!(offset_speaker_numbers(text, 5), text);
    }

    #[test]
    fn combine_with_empty_snapshot_passes_through() {
        let new = "Speaker 1: hi\nSpeaker 2: hello";
        assert_eq!(combine_with_snapshot("", new), new);
        assert_eq!(combine_with_snapshot("   \n  ", new), new);
    }

    #[test]
    fn combine_with_empty_new_returns_snapshot() {
        let snap = "Michael: prior content";
        assert_eq!(combine_with_snapshot(snap, ""), snap);
    }

    #[test]
    fn combine_offsets_new_session_speakers() {
        // Snapshot has Speaker 1 + Speaker 2; new session also numbers
        // from 1 — should be bumped to 3 + 4 to avoid collision.
        let snap = "Speaker 1: prior A\nSpeaker 2: prior B";
        let new = "Speaker 1: new A\nSpeaker 2: new B";
        assert_eq!(
            combine_with_snapshot(snap, new),
            "Speaker 1: prior A\nSpeaker 2: prior B\nSpeaker 3: new A\nSpeaker 4: new B"
        );
    }

    #[test]
    fn combine_no_offset_when_snapshot_uses_renamed() {
        // Snapshot only has renamed labels (no "Speaker N:") — offset is 0,
        // new session keeps its original numbering.
        let snap = "Michael: prior\nWilma: prior";
        let new = "Speaker 1: new";
        assert_eq!(
            combine_with_snapshot(snap, new),
            "Michael: prior\nWilma: prior\nSpeaker 1: new"
        );
    }
}

#[cfg(test)]
mod repetition_tests {
    use super::*;

    #[test]
    fn collapse_detects_long_phrase_loop() {
        // The exact pattern from the user-reported screenshot: "Er det en
        // bok?" repeated dozens of times.
        let s = "Er det en bok? ".repeat(20);
        assert!(is_repetition_collapse(&s));
    }

    #[test]
    fn collapse_detects_single_word_loop() {
        let s = "yes yes yes yes yes yes yes yes";
        assert!(is_repetition_collapse(s));
    }

    #[test]
    fn collapse_passes_normal_speech() {
        // Real-world Norwegian sample with no repetition collapse.
        let s = "Vi har en avtale i morgen klokken ti om prosjektet vi diskuterte forrige uke.";
        assert!(!is_repetition_collapse(s));
    }

    #[test]
    fn collapse_passes_natural_three_rep_short() {
        // Three short reps in a 6-word total chunk are below the dominance
        // threshold (need ≥4 reps OR ≥60% coverage). 6 words, 3 reps × 1
        // word = 50% coverage — passes.
        let s = "ja ja ja det stemmer mhm";
        assert!(!is_repetition_collapse(s));
    }

    #[test]
    fn collapse_detects_partial_loop_dominating_chunk() {
        // 12-word chunk, 4 reps of a 2-word phrase = 8 words = 66% coverage.
        // Should be flagged.
        let s = "noe annet skjedde okay test test test test okay noe annet";
        assert!(is_repetition_collapse(s));
    }

    #[test]
    fn collapse_handles_punctuation_and_case() {
        // Same phrase but mixed case + punctuation differences should still
        // be matched as identical reps.
        let s = "Er det en bok? er det en bok! Er Det En Bok? er det en bok.";
        assert!(is_repetition_collapse(s));
    }
}
