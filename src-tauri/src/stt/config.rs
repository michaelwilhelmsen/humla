//! Tagged-union config for STT providers. Replaces the flat
//! `transcribe_provider` + `transcribe_model` + `whisper_preset` settings
//! triple. Stored in the `settings` table as JSON under key
//! `transcribe_config`. On first read, if the key is missing, we synthesise
//! it from the legacy keys (see `from_legacy_settings`).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider")]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAi(OpenAiConfig),
    #[serde(rename = "local")]
    Local(LocalWhisperConfig),
}

impl ProviderConfig {
    pub fn provider_id(&self) -> &'static str {
        match self {
            ProviderConfig::OpenAi(_) => "openai",
            ProviderConfig::Local(_) => "local",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            ProviderConfig::OpenAi(c) => &c.model,
            ProviderConfig::Local(c) => &c.model_id,
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        match self {
            ProviderConfig::OpenAi(c) => c.base_url.as_deref(),
            ProviderConfig::Local(_) => None,
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
            model: transcribe_model.unwrap_or("gpt-4o-transcribe").to_string(),
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
                model: "gpt-4o-transcribe".to_string(),
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
}
