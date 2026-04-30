use crate::db::{self, Note, NotePatch};
use crate::diarize;
use crate::openai;
use crate::local_whisper;
use crate::presets;
use crate::wav;
use crate::recording::{ChunkRecord, DiagnosticPayload, ErrorPayload, Inflight, Phase, RecordingStatus, SidecarEvent, SummaryPayload, TranscriptPayload};
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
- Lines may begin with a speaker label followed by ': ' — e.g. 'Speaker 1: ', 'Speaker 2: ', or a custom name the user has assigned like 'Michael: ' or 'Anna: '. Preserve these labels EXACTLY as they appear: same text, same colon-space, same position at the start of the line.
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
    // New notes inherit the current global language as their default. The
    // user can change this per-note from the note view; existing notes
    // pre-feature have an empty language and fall back to the global at
    // transcribe / summary time.
    let default_language = db::get_setting(&conn, "language")
        .map_err(err)?
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
    db::create_note(&conn, &default_language).map_err(err)
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
pub async fn diarize_download(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    diarize::download(&app).await.map_err(err)?;
    // Once the model is on disk, prewarm the streaming sidecar so it's
    // already running for the user's next recording instead of a fresh
    // 1–2 s spin-up at recording_start.
    let stream_arc = state.diarize_stream.clone();
    let app_for_warm = app.clone();
    tokio::spawn(async move {
        diarize::ensure_streaming_running(&app_for_warm, &stream_arc).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn diarize_delete(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    // Shut down the streaming sidecar before wiping the model directory —
    // the live process might be holding model files open via the loaded
    // CoreML context, and we don't want a half-deleted state.
    if let Some(d) = state.diarize_stream.lock().await.take() {
        d.shutdown().await;
    }
    diarize::delete(&app).await.map_err(err)
}

// ---- Local Whisper model management ----------------------------------------

fn local_model_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(err)?.join("models");
    Ok(dir)
}

fn local_model_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(local_model_dir(app)?.join(local_whisper::MODEL_FILE))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWhisperStatus {
    downloaded: bool,
    size_bytes: Option<u64>,
    path: Option<String>,
}

#[tauri::command]
pub fn local_whisper_status(app: AppHandle) -> Result<LocalWhisperStatus, String> {
    let path = local_model_path(&app)?;
    if !path.exists() {
        return Ok(LocalWhisperStatus { downloaded: false, size_bytes: None, path: None });
    }
    let size_bytes = std::fs::metadata(&path).ok().map(|m| m.len());
    Ok(LocalWhisperStatus {
        downloaded: true,
        size_bytes,
        path: path.to_str().map(|s| s.to_string()),
    })
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    received: u64,
    total: Option<u64>,
}

#[tauri::command]
pub async fn local_whisper_download(app: AppHandle) -> Result<(), String> {
    let dir = local_model_dir(&app)?;
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("mkdir: {e}"))?;
    let final_path = dir.join(local_whisper::MODEL_FILE);
    // Download to a temp file in the same dir, then rename atomically so a
    // crash mid-download never leaves a half-written model in place.
    let tmp_path = dir.join(format!("{}.partial", local_whisper::MODEL_FILE));

    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| format!("client: {e}"))?
        .get(local_whisper::MODEL_URL)
        .send()
        .await
        .map_err(|e| format!("network: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {}: HTTP {}", local_whisper::MODEL_URL, resp.status()));
    }
    let total = resp.content_length();
    let _ = app.emit("local_whisper_progress", DownloadProgress { received: 0, total });

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
            let _ = app.emit("local_whisper_progress", DownloadProgress { received, total });
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().await.map_err(|e| format!("flush: {e}"))?;
    drop(file);
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(|e| format!("rename: {e}"))?;
    let _ = app.emit("local_whisper_progress", DownloadProgress { received, total });
    Ok(())
}

#[tauri::command]
pub async fn local_whisper_delete(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let path = local_model_path(&app)?;
    // Drop the loaded model from RAM first so we're not holding the file.
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
    // transcription always fails silently.
    let provider = {
        let conn = state.db.lock();
        db::get_setting(&conn, "transcribe_provider")
            .map_err(err)?
            .unwrap_or_else(|| DEFAULT_TRANSCRIBE_PROVIDER.to_string())
    };
    let pre_err = match provider.as_str() {
        "local" => {
            let p = local_model_path(&app).map_err(|e| e.to_string())?;
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
        if let Ok(model_path) = local_model_path(&app) {
            let shared = state.whisper.clone();
            tokio::spawn(async move {
                if let Err(e) = local_whisper::prewarm(shared, model_path).await {
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
        // this session's decoder. Same for the speaker bookkeeping.
        s.trail.lock().clear();
        s.chunk_log.lock().clear();
        *s.full_wav_path.lock() = None;
        s.speaker_display.lock().clear();
        *s.last_speaker.lock() = None;
    }

    // Live diarization: the streaming sidecar is spawned at app launch
    // (and after a successful model download), so usually it's already
    // warm by the time we get here. Reset its speaker memory so a fresh
    // recording doesn't get matched against the previous meeting's
    // voices. If the sidecar isn't running yet (model wasn't downloaded
    // at app start, or the prewarm task is still in flight), spin it up
    // now in the background — first few chunks may land unprefixed but
    // subsequent ones tag correctly.
    {
        let state: State<AppState> = app.state();
        let stream_arc = state.diarize_stream.clone();
        let app_for_diarize = app.clone();
        tokio::spawn(async move {
            let mut guard = stream_arc.lock().await;
            if let Some(d) = guard.as_mut() {
                if let Err(e) = d.reset().await {
                    eprintln!("streaming diarize reset: {e}");
                }
            } else {
                drop(guard);
                diarize::ensure_streaming_running(&app_for_diarize, &stream_arc).await;
            }
        });
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
                Ok(SidecarEvent::Chunk { path, start_ms }) => {
                    let pb = PathBuf::from(path);
                    let app2 = app_clone.clone();
                    let note_id2 = note_id_clone.clone();
                    let h = tokio::spawn(async move {
                        if let Err(e) = transcribe_chunk(app2.clone(), note_id2.clone(), pb, start_ms).await {
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
                Ok(SidecarEvent::FullRecording { path, duration_ms: _ }) => {
                    // Stash the path on the session; the diarization pass on
                    // stop reads it. We don't act on it here — the recording
                    // is still wrapping up.
                    let state: tauri::State<AppState> = app_clone.state();
                    *state.recording.lock().full_wav_path.lock() = Some(PathBuf::from(path));
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
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), drain).await;

    // The streaming diarize sidecar is NOT torn down here — it stays
    // alive across recordings so the next recording's chunks tag
    // immediately. recording_start sends a `reset` command to wipe the
    // SpeakerManager between sessions. The sidecar exits when the app
    // does (parent-death watchdog OR drop semantics).

    // Spawn the post-stop processing chain in the background:
    //   Stopping → Polishing → Idle
    // Diarization happens live during recording; polish picks up the
    // already-tagged transcript and gets a strict "preserve speaker
    // labels" rule.
    let app_for_post = app.clone();
    let note_for_post = note_id.clone();
    tokio::spawn(async move {
        if let Err(e) = polish_transcript(app_for_post.clone(), note_for_post.clone()).await {
            eprintln!("polish_transcript: {e}");
            emit_error(
                &app_for_post,
                Some(&note_for_post),
                &format!("Polish failed: {e}"),
            );
        }
        emit_status(&app_for_post, None, Phase::Idle);
    });

    if let Some(dir) = temp_dir {
        // Best-effort cleanup later; keep until summary is in the DB.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let _ = tokio::fs::remove_dir_all(dir).await;
        });
    }
    Ok(())
}

async fn transcribe_chunk(
    app: AppHandle,
    note_id: String,
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
    // and a snapshot of the last ~150 committed words from this session.
    // Combined, this carries sentence continuity, proper-noun spelling, and
    // a non-empty prior — the single best mitigation for Whisper's
    // silence/short-clip hallucinations.
    let trail_snapshot = {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let trail = session.trail.lock();
        trail.as_prompt()
    };
    let prompt = build_initial_prompt(&cfg.vocabulary, trail_snapshot);

    let text = match cfg.provider.as_str() {
        "local" => {
            let model_path = local_model_path(&app).map_err(|e| anyhow::anyhow!(e))?;
            let shared = {
                let state: State<AppState> = app.state();
                state.whisper.clone()
            };
            let preset = local_whisper::Preset::from_setting(&cfg.whisper_preset);
            local_whisper::transcribe_file(
                shared,
                model_path,
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
    // Whisper was trained on closed-caption data and frequently appends
    // subtitle attribution ("Undertekster av Ai-Media", "Subtitles by Amara",
    // "Thanks for watching") at the end of real speech. Trim those tails.
    let text = strip_attribution_tail(&text);
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Ok(());
    }

    // Live diarization: ask the streaming sidecar (if running) to classify
    // this chunk's speaker. Returns None if the model isn't downloaded, the
    // sidecar isn't running, or classification failed — we just append
    // without a speaker prefix in those cases.
    let speaker_id: Option<String> = {
        let state: State<AppState> = app.state();
        let stream_arc = state.diarize_stream.clone();
        let mut guard = stream_arc.lock().await;
        match guard.as_mut() {
            Some(diarizer) => match diarizer.classify(&path).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("diarize classify: {e}");
                    None
                }
            },
            None => None,
        }
    };

    // Decide what to append + which separator to use against the existing
    // transcript. db::append_transcript skips the separator entirely when
    // the existing transcript is empty, so we don't have to special-case
    // the very first chunk.
    //
    //   - Speaker change (or no prior speaker): "Speaker N: <chunk>" + "\n"
    //   - Same speaker as last: "<chunk>" + " "
    //   - No speaker info available: "<chunk>" + " "
    let state: State<AppState> = app.state();
    let (formatted_text, separator) = {
        let session = state.recording.lock();
        let mut display_map = session.speaker_display.lock();
        let mut last_speaker = session.last_speaker.lock();

        if let Some(sid) = speaker_id.as_ref() {
            let display_n = if let Some(n) = display_map.get(sid).copied() {
                n
            } else {
                let next = (display_map.len() as u32) + 1;
                display_map.insert(sid.clone(), next);
                next
            };
            let speaker_changed = last_speaker.as_deref() != Some(sid.as_str());
            *last_speaker = Some(sid.clone());
            if speaker_changed {
                (format!("Speaker {display_n}: {trimmed}"), "\n".to_string())
            } else {
                (trimmed.clone(), " ".to_string())
            }
        } else {
            // No classification — append with a space and don't prefix.
            // Don't update last_speaker so the next classified chunk can
            // still emit a "Speaker N:" prefix correctly.
            (trimmed.clone(), " ".to_string())
        }
    };

    let updated_transcript = {
        let conn = state.db.lock();
        db::append_transcript(&conn, &note_id, &formatted_text, &separator)?
    };
    {
        let session = state.recording.lock();
        session.trail.lock().push(&trimmed);
        session.chunk_log.lock().push(ChunkRecord {
            start_ms,
            text: trimmed.clone(),
        });
    }
    // Emit the FULL updated transcript via transcript_replaced so the
    // frontend's store gets the speaker prefix + separator we just wrote
    // verbatim. The legacy transcript_appended event used a hardcoded
    // space separator on the frontend side, which would clobber our
    // newline turns.
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
    let provider = if note.summary_provider.trim().is_empty() {
        db::get_setting(conn, "summary_provider")
            .ok()
            .flatten()
            .unwrap_or_else(|| "openai".into())
    } else {
        note.summary_provider.clone()
    };

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
            Ok(ResolvedProvider {
                base_url,
                api_key: "humla-local".into(),
                model,
            })
        }
        _ => {
            let api_key = db::get_setting(conn, API_KEY)?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("OpenAI API key not set"))?;
            let model = db::get_setting(conn, "summary_model")?
                .unwrap_or_else(|| DEFAULT_SUMMARY_MODEL.to_string());
            Ok(ResolvedProvider {
                base_url: openai::BASE.into(),
                api_key,
                model,
            })
        }
    }
}

#[tauri::command]
pub async fn summarize_note(app: AppHandle, note_id: String) -> Result<(), String> {
    // Reflect the in-flight summary in the recording status so the UI can
    // show a spinner. Use the existing Summarizing phase.
    emit_status(&app, Some(&note_id), Phase::Summarizing);
    let result = run_summary(app.clone(), note_id.clone()).await;
    emit_status(&app, None, Phase::Idle);
    result.map_err(|e| e.to_string())
}

// Polish a freshly-recorded transcript via a chat-completion pass. Whisper's
// raw output is usually correct in substance but littered with typos,
// chunk-boundary mid-word cuts ("mistenkte" → "mistred"), and missing
// punctuation. The user's notes + custom vocabulary are passed as context so
// the model spells proper nouns and domain terms correctly.
//
// Skips silently when there's no transcript, no OpenAI API key, or the
// transcript was modified between the snapshot read and the polished write
// (the user started another recording on the same note while polish was in
// flight) — the latter check prevents losing freshly-appended chunks.
async fn polish_transcript(app: AppHandle, note_id: String) -> anyhow::Result<()> {
    let (provider, transcript_snapshot, body, vocabulary) = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let n = db::get_note(&conn, &note_id)?;
        if n.transcript.trim().is_empty() {
            return Ok(()); // nothing to polish
        }
        // No-provider conditions (no API key, no local model) silently skip
        // — polish is a best-effort step, not something to error the recording out for.
        let provider = match resolve_provider(&conn, &n) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        // Skip polish on local providers. Polish regenerates the entire
        // transcript with edits, so on a 30-min meeting it's ~6,000 output
        // tokens at ~30 tok/s = several minutes per call. That blocks the
        // Ollama queue and makes the user-triggered Summarize wait behind
        // it. Cloud OpenAI is fast enough that this isn't an issue.
        // Whisper turbo's raw output is high-quality enough that skipping
        // polish on local is an acceptable trade for not waiting.
        if provider.base_url != openai::BASE {
            return Ok(());
        }
        let vocab = db::get_setting(&conn, "custom_vocabulary")?.unwrap_or_default();
        (provider, n.transcript.clone(), n.body.clone(), vocab)
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
        DEFAULT_POLISH_PROMPT,
        &user_message,
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
    // Resolve the prompt for this note: a named preset, or the user's custom
    // prompt from Settings if preset == "custom".
    let prompt = if note.summary_preset == "custom" {
        custom_prompt
    } else {
        presets::prompt(&note.summary_preset, &language)
    };
    let body_text = html_to_text(&note.body);
    let user_message = match (body_text.is_empty(), note.transcript.trim().is_empty()) {
        (true, _) => format!("[Transkripsjon]\n{}", note.transcript),
        (false, true) => format!("[Notater]\n{body_text}"),
        (false, false) => format!("[Notater]\n{body_text}\n\n[Transkripsjon]\n{}", note.transcript),
    };
    // Hard language directive in case the prompt was authored in a different
    // language than the user has now chosen.
    let full_prompt = format!("{prompt}\n\n{}", language_directive(&language));
    let summary = openai::summarize_with_base(
        &provider.base_url,
        &provider.api_key,
        &provider.model,
        &full_prompt,
        &user_message,
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
fn language_directive(lang: &str) -> &'static str {
    match lang {
        "no" => "VIKTIG: Skriv hele svaret på norsk.",
        "sv" => "VIKTIGT: Skriv hela svaret på svenska.",
        "da" => "VIGTIGT: Skriv hele svaret på dansk.",
        "auto" => "Respond in the same language as the user's notes.",
        _ => "IMPORTANT: Write the entire response in English.",
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
