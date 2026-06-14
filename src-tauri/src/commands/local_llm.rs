//! Local LLM (OpenAI-compatible HTTP server) command. Surfaced to the
//! frontend via `commands::local_llm_list_models` (re-exported from the
//! parent module).

use super::err;
use crate::openai;

// Hit the user-configured local LLM server's /v1/models endpoint and return
// the list of model IDs. Used by Settings to populate the Model dropdown when
// the user picks Local provider. Most servers (Ollama, LM Studio, llama-server,
// vLLM) implement this exact OpenAI-compatible shape.
#[tauri::command]
pub async fn local_llm_list_models(base_url: String) -> Result<Vec<String>, String> {
    openai::list_models(&base_url).await.map_err(err)
}
