// Recommended local (Ollama) models, by RAM tier — single source of truth for
// the settings + onboarding pull hints.
//
// Basis: spike #45 (docs/research/spike-45-ollama-tool-calling). gemma4:12b-mlx
// is the strongest local tool-caller (agentic chat) AND a capable summary model,
// and the MLX build is fast on Apple Silicon — but it wants ~24-32 GB of RAM.
// On 16 GB Macs it OOMs, so qwen3.5:4b stays the fallback there (it also suits
// the Qwen-tuned sampling profile in src-tauri/src/openai.rs). Recommend by tier.
export const RECOMMENDED_OLLAMA_MODEL = "gemma4:12b-mlx";
export const RECOMMENDED_OLLAMA_MODEL_16GB = "qwen3.5:4b";

// The local embedding model for semantic chat retrieval (issue #48). Small
// (~600 MB) and the sole local embedder — no fallback. Optional: without it,
// local chat still works keyword-only (semantic search degrades gracefully).
export const EMBEDDING_OLLAMA_MODEL = "embeddinggemma";

/// Whether an Ollama-installed model list already includes a given model,
/// tag-insensitively (Ollama reports names like "embeddinggemma:latest").
export function isModelInstalled(installed: string[] | null | undefined, model: string): boolean {
  if (!installed) return false;
  const base = model.split(":")[0];
  return installed.some((m) => m === model || m.split(":")[0] === base);
}

/// Whether a model is embedding-only and so must NOT appear in a chat/summary
/// (completion) model picker — selecting one makes Ollama's /api/chat 400 with
/// "does not support chat". Matches embeddinggemma plus the common embedding
/// families a user might have pulled (nomic-embed, mxbai-embed, arctic-embed,
/// granite-embedding, bge-*, all-minilm, paraphrase-*).
export function isEmbeddingModel(name: string): boolean {
  const n = name.toLowerCase();
  return (
    /embed/.test(n) ||
    n.startsWith("bge-") ||
    n.startsWith("all-minilm") ||
    n.startsWith("paraphrase-")
  );
}

/// The installed models usable for chat/summary completions — embedding-only
/// models filtered out.
export function completionModels(installed: string[] | null | undefined): string[] {
  return (installed ?? []).filter((m) => !isEmbeddingModel(m));
}
