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
