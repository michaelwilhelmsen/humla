//! Tagged-union config for STT providers. Replaces the flat
//! `transcribe_provider` + `transcribe_model` + `whisper_preset` settings
//! triple. Stored in the `settings` table as JSON under key
//! `transcribe_config`. On first read, if the key is missing, we synthesise
//! it from the legacy keys (see `from_legacy_settings`).

use crate::providers::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider")]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAi(OpenAiConfig),
    #[serde(rename = "local")]
    Local(LocalWhisperConfig),
    #[serde(rename = "deepgram")]
    Deepgram(DeepgramConfig),
    #[serde(rename = "groq")]
    Groq(GroqConfig),
}

impl ProviderConfig {
    pub fn provider(&self) -> ProviderId {
        match self {
            ProviderConfig::OpenAi(_) => ProviderId::OpenAi,
            ProviderConfig::Local(_) => ProviderId::Local,
            ProviderConfig::Deepgram(_) => ProviderId::Deepgram,
            ProviderConfig::Groq(_) => ProviderId::Groq,
        }
    }

    pub fn provider_id(&self) -> &'static str {
        self.provider().as_str()
    }

    pub fn model(&self) -> &str {
        match self {
            ProviderConfig::OpenAi(c) => &c.model,
            ProviderConfig::Local(c) => &c.model_id,
            ProviderConfig::Deepgram(c) => &c.model,
            ProviderConfig::Groq(c) => &c.model,
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        match self {
            ProviderConfig::OpenAi(c) => c.base_url.as_deref(),
            ProviderConfig::Local(_) => None,
            ProviderConfig::Deepgram(c) => c.base_url.as_deref(),
            // Groq's URL is fixed; if a user wanted to point at a self-
            // hosted Groq-compat server they'd switch to the OpenAI
            // provider with a custom base_url.
            ProviderConfig::Groq(_) => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalWhisperConfig {
    pub model_id: String,
    pub preset: String,
    pub use_gpu: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeepgramConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroqConfig {
    pub model: String,
}

/// Top-level transcription configuration. Wraps a default `ProviderConfig`
/// plus a map of per-language overrides keyed by ISO 639-1 code (matching
/// the `language` field on Note and the global `language` setting).
///
/// Resolution order at recording time:
///   1. If the recording's language matches a `per_language` key, use that.
///   2. Otherwise use `default`.
///
/// The "auto" pseudo-language never matches a `per_language` entry —
/// resolves to `default`. This mirrors the v0.23 `addon_for_language`
/// behaviour, which returned None for "auto".
///
/// `BTreeMap` (not `HashMap`) is intentional: stable JSON key order makes
/// settings diffs readable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscribeConfig {
    pub default: ProviderConfig,
    #[serde(default)]
    pub per_language: BTreeMap<String, ProviderConfig>,
}

impl TranscribeConfig {
    /// Pick the active `ProviderConfig` for the given recording language.
    /// `language` should be an ISO 639-1 code or "auto"; anything else
    /// just falls through to the default (no error case worth surfacing).
    pub fn resolve(&self, language: &str) -> &ProviderConfig {
        if language == "auto" {
            return &self.default;
        }
        self.per_language.get(language).unwrap_or(&self.default)
    }

    /// Sensible default for fresh installs and for recovering from a
    /// corrupt `transcribe_config` row. Matches the bare-default
    /// produced by `from_legacy_settings(None, …)` in v0.23, wrapped.
    pub fn default_fallback() -> Self {
        Self {
            default: from_legacy_settings(None, None, None, None, None),
            per_language: BTreeMap::new(),
        }
    }
}

/// Build a `ProviderConfig` from the legacy flat settings shape. Used at
/// migration time when `transcribe_config` is absent. None of the legacy
/// keys are required to exist — defaults match what the old `transcribe_chunk`
/// fallback chain produced.
pub fn from_legacy_settings(
    transcribe_provider: Option<&str>,
    transcribe_model: Option<&str>,
    whisper_model_id: Option<&str>,
    whisper_preset: Option<&str>,
    whisper_use_gpu: Option<bool>,
) -> ProviderConfig {
    match transcribe_provider.unwrap_or("openai") {
        "local" => ProviderConfig::Local(LocalWhisperConfig {
            model_id: whisper_model_id.unwrap_or("large-v3-turbo-q5").to_string(),
            preset: whisper_preset.unwrap_or("quality").to_string(),
            use_gpu: whisper_use_gpu.unwrap_or(true),
        }),
        _ => ProviderConfig::OpenAi(OpenAiConfig {
            model: transcribe_model.unwrap_or("whisper-1").to_string(),
            base_url: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_round_trips_through_json() {
        let cfg = ProviderConfig::OpenAi(OpenAiConfig {
            model: "whisper-1".to_string(),
            base_url: None,
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains(r#""provider":"openai""#));
    }

    #[test]
    fn local_round_trips_through_json() {
        let cfg = ProviderConfig::Local(LocalWhisperConfig {
            model_id: "large-v3-turbo-q5".to_string(),
            preset: "quality".to_string(),
            use_gpu: true,
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains(r#""provider":"local""#));
    }

    #[test]
    fn legacy_migration_openai_defaults() {
        let cfg = from_legacy_settings(None, None, None, None, None);
        assert_eq!(
            cfg,
            ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            })
        );
    }

    #[test]
    fn legacy_migration_keeps_user_openai_model() {
        let cfg = from_legacy_settings(Some("openai"), Some("whisper-1"), None, None, None);
        assert_eq!(cfg.model(), "whisper-1");
        assert_eq!(cfg.provider_id(), "openai");
    }

    #[test]
    fn legacy_migration_local_inherits_preset_and_gpu() {
        let cfg = from_legacy_settings(
            Some("local"),
            None,
            Some("medium-q5"),
            Some("balanced"),
            Some(false),
        );
        match cfg {
            ProviderConfig::Local(c) => {
                assert_eq!(c.model_id, "medium-q5");
                assert_eq!(c.preset, "balanced");
                assert!(!c.use_gpu);
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn deepgram_round_trips_through_json() {
        let cfg = ProviderConfig::Deepgram(DeepgramConfig {
            model: "nova-3".to_string(),
            base_url: None,
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains(r#""provider":"deepgram""#));
        assert_eq!(cfg.model(), "nova-3");
    }

    #[test]
    fn groq_round_trips_through_json() {
        let cfg = ProviderConfig::Groq(GroqConfig {
            model: "whisper-large-v3-turbo".to_string(),
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains(r#""provider":"groq""#));
    }

    #[test]
    fn provider_id_matches_serde_tag() {
        let cfgs = [
            ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            ProviderConfig::Local(LocalWhisperConfig {
                model_id: "large-v3-turbo-q5".to_string(),
                preset: "quality".to_string(),
                use_gpu: true,
            }),
        ];
        for cfg in cfgs {
            let json = serde_json::to_string(&cfg).unwrap();
            assert!(json.contains(&format!(r#""provider":"{}""#, cfg.provider_id())));
        }
    }

    #[test]
    fn transcribe_config_round_trips_through_json() {
        let mut per = BTreeMap::new();
        per.insert(
            "no".to_string(),
            ProviderConfig::Local(LocalWhisperConfig {
                model_id: "nb-whisper-large-q5".to_string(),
                preset: "quality".to_string(),
                use_gpu: true,
            }),
        );
        per.insert(
            "en".to_string(),
            ProviderConfig::Deepgram(DeepgramConfig {
                model: "nova-3".to_string(),
                base_url: None,
            }),
        );
        let cfg = TranscribeConfig {
            default: ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            per_language: per,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: TranscribeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        // BTreeMap key order in JSON: "en" before "no" alphabetically.
        let en_pos = json.find(r#""en":"#).unwrap();
        let no_pos = json.find(r#""no":"#).unwrap();
        assert!(en_pos < no_pos, "BTreeMap should serialise keys in order");
    }

    #[test]
    fn transcribe_config_with_empty_per_language_round_trips() {
        let cfg = TranscribeConfig {
            default: ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            per_language: BTreeMap::new(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: TranscribeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn resolve_returns_per_language_match() {
        let mut per = BTreeMap::new();
        per.insert(
            "no".to_string(),
            ProviderConfig::Local(LocalWhisperConfig {
                model_id: "nb-whisper-large-q5".to_string(),
                preset: "quality".to_string(),
                use_gpu: true,
            }),
        );
        let cfg = TranscribeConfig {
            default: ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            per_language: per,
        };
        assert_eq!(cfg.resolve("no").provider_id(), "local");
    }

    #[test]
    fn resolve_falls_back_to_default_for_unmapped_language() {
        let cfg = TranscribeConfig {
            default: ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            per_language: BTreeMap::new(),
        };
        assert_eq!(cfg.resolve("de").provider_id(), "openai");
    }

    #[test]
    fn resolve_treats_auto_as_default_even_with_overrides_present() {
        let mut per = BTreeMap::new();
        per.insert(
            "no".to_string(),
            ProviderConfig::Local(LocalWhisperConfig {
                model_id: "nb-whisper-large-q5".to_string(),
                preset: "quality".to_string(),
                use_gpu: true,
            }),
        );
        let cfg = TranscribeConfig {
            default: ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            per_language: per,
        };
        // "auto" is never a real ISO language code — never matches an
        // override. Same semantics as the v0.23 addon_for_language guard.
        assert_eq!(cfg.resolve("auto").provider_id(), "openai");
    }

    #[test]
    fn legacy_provider_config_does_not_parse_as_transcribe_config() {
        // Migration logic relies on this asymmetry: a stored bare
        // ProviderConfig must FAIL to deserialise into a
        // TranscribeConfig so the migration knows to wrap.
        let legacy = r#"{"provider":"openai","model":"whisper-1"}"#;
        assert!(serde_json::from_str::<TranscribeConfig>(legacy).is_err());
    }
}
