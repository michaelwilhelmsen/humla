// Auto-discovery of local LLM models the user already has installed via
// LM Studio, Ollama, or HuggingFace. Lets users skip the 3–5 GB redownload
// when they already have a compatible Gemma/Qwen GGUF on disk.
//
// Strategy: scan known cache locations for *.gguf files, sniff each header
// (architecture + quantization) via crate::gguf, and report the compatible
// ones. We never load the weights — discovery is cheap.

use crate::gguf;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredLlm {
    pub source: String,
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub architecture: String,
    pub quantization: String,
    pub compatible: bool,
}

pub fn scan_all(home: &Path) -> Vec<DiscoveredLlm> {
    let mut found = Vec::new();
    found.extend(scan_lm_studio(home));
    found.extend(scan_ollama(home));
    found.extend(scan_huggingface(home));
    found
}

fn scan_lm_studio(home: &Path) -> Vec<DiscoveredLlm> {
    let mut out = Vec::new();
    for base in [
        home.join(".cache/lm-studio/models"),
        home.join(".lmstudio/models"),
    ] {
        if !base.exists() {
            continue;
        }
        let base_for_name = base.clone();
        walk_gguf(
            &base,
            "lm-studio",
            move |path| {
                // LM Studio layout: <base>/<publisher>/<model>/<file>.gguf
                let rel = path.strip_prefix(&base_for_name).unwrap_or(path);
                rel.with_extension("").display().to_string()
            },
            &mut out,
        );
    }
    out
}

fn scan_ollama(home: &Path) -> Vec<DiscoveredLlm> {
    // Ollama stores GGUFs as content-addressable blobs, referenced by a
    // manifest layer with mediaType "application/vnd.ollama.image.model".
    // We walk all manifests, find the model layer, resolve to its blob, and
    // sniff that. Tagged models (gemma:4b-instruct-q4_0) end up as the
    // manifest path components.
    let mut out = Vec::new();
    let manifests = home.join(".ollama/models/manifests");
    let blobs = home.join(".ollama/models/blobs");
    if !manifests.exists() || !blobs.exists() {
        return out;
    }
    for entry in walkdir::WalkDir::new(&manifests)
        .max_depth(8)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let manifest_path = entry.path();
        let json = match std::fs::read_to_string(manifest_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let layers = match v.get("layers").and_then(|l| l.as_array()) {
            Some(l) => l,
            None => continue,
        };
        for layer in layers {
            let media = layer
                .get("mediaType")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if media != "application/vnd.ollama.image.model" {
                continue;
            }
            let digest = layer.get("digest").and_then(|d| d.as_str()).unwrap_or("");
            // Digests are "sha256:abc..."; on disk the file is "sha256-abc...".
            let blob_file = digest.replace(':', "-");
            let blob_path = blobs.join(blob_file);
            if !blob_path.exists() {
                continue;
            }
            let name = manifest_path
                .strip_prefix(&manifests)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| manifest_path.display().to_string());
            let size = std::fs::metadata(&blob_path).map(|m| m.len()).unwrap_or(0);
            push_with_sniff(&mut out, "ollama", name, blob_path, size);
        }
    }
    out
}

fn scan_huggingface(home: &Path) -> Vec<DiscoveredLlm> {
    let mut out = Vec::new();
    let base = home.join(".cache/huggingface/hub");
    if !base.exists() {
        return out;
    }
    walk_gguf(
        &base,
        "huggingface",
        |path| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        &mut out,
    );
    out
}

fn walk_gguf(
    root: &Path,
    source: &str,
    name_fn: impl Fn(&Path) -> String,
    out: &mut Vec<DiscoveredLlm>,
) {
    for entry in walkdir::WalkDir::new(root)
        .max_depth(8)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
            continue;
        }
        let name = name_fn(path);
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        push_with_sniff(out, source, name, path.to_path_buf(), size);
    }
}

fn push_with_sniff(
    out: &mut Vec<DiscoveredLlm>,
    source: &str,
    name: String,
    path: PathBuf,
    size: u64,
) {
    let info = match gguf::sniff(&path) {
        Ok(i) => i,
        Err(_) => return, // skip non-GGUF or corrupt files silently
    };
    let compatible = is_compatible(&info.architecture, &info.quantization);
    out.push(DiscoveredLlm {
        source: source.into(),
        name,
        path: path.display().to_string(),
        size_bytes: size,
        architecture: info.architecture,
        quantization: info.quantization,
        compatible,
    });
}

// Public so commands::local_llm_select_existing can validate user-picked paths
// against the same rules the scanner applies.
pub fn is_compatible(arch: &str, quant: &str) -> bool {
    let arch_ok =
        arch.starts_with("gemma") || arch == "qwen2" || arch == "qwen3";
    // Q4_K_M and up; reject Q2/Q3 (quality is unusable for summarization tasks).
    let quant_ok = matches!(
        quant,
        "Q4_0" | "Q4_1" | "Q4_K_S" | "Q4_K_M" | "Q5_K_S" | "Q5_K_M" | "Q6_K" | "Q8_0" | "F16"
    );
    arch_ok && quant_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_matrix() {
        assert!(is_compatible("gemma3", "Q4_K_M"));
        assert!(is_compatible("gemma4", "Q5_K_M"));
        assert!(is_compatible("qwen3", "Q4_0"));
        assert!(is_compatible("qwen2", "Q8_0"));
        assert!(!is_compatible("llama", "Q4_K_M"));
        assert!(!is_compatible("gemma3", "Q2_K"));
        assert!(!is_compatible("phi", "Q4_K_M"));
    }
}
