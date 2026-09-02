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

// Ask a local server for one embedding and report its dimensionality (#179).
// The model listing can't answer this: mlx_lm.server lists a model and has no
// `/v1/embeddings` route at all, and a name that isn't the loaded embedder is a
// 400 the listing looks fine through. So probe with a real call and let the
// server's own error be the message.
#[tauri::command]
pub async fn local_llm_embed_probe(base_url: String, model: String) -> Result<usize, String> {
    let vectors = openai::openai_embed(&base_url, None, &model, &["humla".to_string()])
        .await
        .map_err(err)?;
    vectors.first().map(|v| v.len()).ok_or_else(|| "the server returned no vector".to_string())
}
