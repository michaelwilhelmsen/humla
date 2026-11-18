//! Transcription-routing config commands + reader. The typed
//! `TranscribeConfig` (default provider + per-language overrides) is the
//! single source of truth for STT dispatch. Commands are surfaced to the
//! frontend via `commands::*_transcribe_config` (re-exported from the
//! parent module). `read_transcribe_config` is also called by the
//! recording / transcribe paths that stay in the parent module, hence
//! `pub(crate)`.

use super::err;
use crate::org_policy::{self, EffectivePolicy};
use crate::stt::{ProviderConfig, TranscribeConfig};
use crate::db;
use crate::AppState;
use tauri::State;

/// Read the active transcription config (default + per-language
/// overrides). The Settings UI calls this on mount as the source of
/// truth for the Transcription tab and uses it to drive both the
/// Default provider section and the Per-language overrides list.
#[tauri::command]
pub fn get_transcribe_config(
    state: State<AppState>,
) -> Result<crate::stt::TranscribeConfig, String> {
    read_transcribe_config(&state).map_err(|e| e.to_string())
}

/// Persist a typed `TranscribeConfig` to settings. Frontend writes the
/// whole shape on every change so the choice (default + every per-
/// language entry) is atomic — no partial drift from one path
/// half-updating.
#[tauri::command]
pub fn set_transcribe_config(
    state: State<AppState>,
    config: crate::stt::TranscribeConfig,
) -> Result<(), String> {
    let json = serde_json::to_string(&config).map_err(err)?;
    let conn = state.db.lock();
    let policy = org_policy::effective_managed();
    if !validate(&config, &policy, custom_base_url(&conn).as_deref()).is_empty() {
        return Err(policy.blocked().to_string());
    }
    db::set_setting(&conn, "transcribe_config", &json).map_err(err)
}

/// Read the active transcription config (default + per-language
/// overrides) from the typed `transcribe_config` JSON. Falls back to a
/// hardcoded default when the row is absent or corrupt — defensive only;
/// `db::migrate_per_language_v4` ensures the row is always present after
/// one launch under v0.24+.
pub(crate) fn read_transcribe_config(
    state: &State<AppState>,
) -> anyhow::Result<crate::stt::TranscribeConfig> {
    let conn = state.db.lock();
    if let Some(json) = db::get_setting(&conn, "transcribe_config")? {
        if let Ok(cfg) = serde_json::from_str::<crate::stt::TranscribeConfig>(&json) {
            return Ok(cfg);
        }
        // Corrupted JSON — fall through to the default rather than locking
        // the user out over a malformed cache. Settings UI overwrites
        // this when the user opens the Transcription tab.
    }
    Ok(crate::stt::TranscribeConfig::default_fallback())
}

/// The custom endpoint's URL, shared by every surface that uses it.
pub(crate) fn custom_base_url(conn: &rusqlite::Connection) -> Option<String> {
    crate::custom_endpoint::base_url(conn).ok()
}

/// Where a transcription config sends audio: `None` for on-device Whisper.
pub(crate) fn endpoint_url<'a>(cfg: &'a ProviderConfig, custom_base_url: Option<&'a str>) -> Option<&'a str> {
    match cfg {
        ProviderConfig::Custom(_) => custom_base_url,
        ProviderConfig::Local(_)
        | ProviderConfig::OpenAi(_)
        | ProviderConfig::Deepgram(_)
        | ProviderConfig::Groq(_) => cfg.base_url(),
    }
}

/// Where a note's audio is transcribed: the user's config for `language`, with
/// each slot the policy for the note's workspace refuses moved to on-device
/// Whisper, as Settings shows it under "Managed by".
pub(crate) fn resolve_for_note(
    conn: &rusqlite::Connection,
    cfg: &TranscribeConfig,
    note_id: &str,
    language: &str,
) -> ProviderConfig {
    let policy = org_policy::effective_for_note(conn, note_id);
    enforce(cfg, &policy, custom_base_url(conn).as_deref()).resolve(language).clone()
}

/// Refuse a transcription config the policy for a note's workspace doesn't
/// allow, before any Keychain read.
pub(crate) fn check(conn: &rusqlite::Connection, note_id: &str, cfg: &ProviderConfig) -> anyhow::Result<()> {
    let policy = org_policy::effective_for_note(conn, note_id);
    let custom = custom_base_url(conn);
    if policy.allows(cfg.provider(), endpoint_url(cfg, custom.as_deref())) {
        Ok(())
    } else {
        Err(policy.blocked())
    }
}

/// The slots the policy refuses: `"default"` and the language code of each
/// refused per-language override.
pub(crate) fn validate(
    cfg: &TranscribeConfig,
    policy: &EffectivePolicy,
    custom_base_url: Option<&str>,
) -> Vec<String> {
    let refused = |c: &ProviderConfig| !policy.allows(c.provider(), endpoint_url(c, custom_base_url));
    let mut out = Vec::new();
    if refused(&cfg.default) {
        out.push("default".to_string());
    }
    out.extend(cfg.per_language.iter().filter(|(_, c)| refused(c)).map(|(l, _)| l.clone()));
    out
}

/// `cfg` with every refused slot set to on-device Whisper, the one transcriber
/// `local_only` always allows. Mirrored by `enforceLocalOnly` in
/// `src/lib/orgPolicy.ts`.
pub(crate) fn enforce(
    cfg: &TranscribeConfig,
    policy: &EffectivePolicy,
    custom_base_url: Option<&str>,
) -> TranscribeConfig {
    let mut out = cfg.clone();
    let refused = validate(cfg, policy, custom_base_url);
    let local = || crate::stt::from_legacy_settings(Some("local"), None, None, None, None);
    for slot in refused {
        if slot == "default" {
            out.default = local();
        } else if let Some(c) = out.per_language.get_mut(&slot) {
            *c = local();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::{CustomConfig, DeepgramConfig, OpenAiConfig};

    fn local_only() -> EffectivePolicy {
        let (_d, conn) = conn();
        org_policy::cache_local_only(&conn, "Northwind");
        org_policy::effective_for(&conn, Some(org_policy::TEST_ORG_WORKSPACE)).unwrap()
    }

    fn conn() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.sqlite")).unwrap();
        (dir, conn)
    }

    fn config() -> TranscribeConfig {
        let mut cfg = TranscribeConfig::default_fallback();
        cfg.default = ProviderConfig::OpenAi(OpenAiConfig { model: "whisper-1".into(), base_url: None });
        cfg.per_language.insert(
            "no".into(),
            ProviderConfig::Deepgram(DeepgramConfig { model: "nova-3".into(), base_url: None }),
        );
        cfg.per_language.insert(
            "sv".into(),
            crate::stt::from_legacy_settings(Some("local"), None, Some("medium-q5"), None, None),
        );
        cfg.per_language.insert("da".into(), ProviderConfig::Custom(CustomConfig { model: "m".into() }));
        cfg
    }

    #[test]
    fn validate_names_every_refused_slot() {
        let policy = local_only();
        assert_eq!(validate(&config(), &policy, Some("http://localhost:9000/v1")), ["default", "no"]);
        assert_eq!(validate(&config(), &policy, Some("https://api.example.com")), ["default", "da", "no"]);
        assert!(validate(&config(), &EffectivePolicy::default(), None).is_empty());
    }

    #[test]
    fn enforce_moves_refused_slots_to_on_device_whisper_and_keeps_the_rest() {
        let policy = local_only();
        let out = enforce(&config(), &policy, Some("http://localhost:9000/v1"));
        assert_eq!(out.default.provider(), crate::providers::ProviderId::Local);
        assert_eq!(out.per_language["no"].provider(), crate::providers::ProviderId::Local);
        assert_eq!(out.per_language["sv"], config().per_language["sv"]);
        assert_eq!(out.per_language["da"], config().per_language["da"]);
        assert!(validate(&out, &policy, Some("http://localhost:9000/v1")).is_empty());
    }

    #[test]
    fn check_refuses_a_cloud_transcriber_only_for_a_governed_note() {
        let (_d, conn) = conn();
        org_policy::cache_local_only(&conn, "Northwind");
        let governed = org_policy::note_in(&conn, org_policy::TEST_ORG_WORKSPACE);
        let cfg = config();
        let err = check(&conn, &governed, &cfg.default).unwrap_err().to_string();
        assert_eq!(err, "Blocked by Northwind: processing stays on this Mac.");
        assert!(check(&conn, &governed, &cfg.per_language["sv"]).is_ok());
        assert!(check(&conn, &governed, &cfg.per_language["da"]).is_err(), "custom with no URL");
        for ws in ["", "w-other"] {
            let free = org_policy::note_in(&conn, ws);
            assert!(check(&conn, &free, &cfg.default).is_ok(), "{ws:?}");
        }
        db::set_setting(&conn, "custom_base_url", "http://[::1]:9000/v1").unwrap();
        assert!(check(&conn, &governed, &cfg.per_language["da"]).is_ok());
    }

    #[test]
    fn a_governed_note_is_transcribed_on_this_mac_and_others_as_configured() {
        let (_d, conn) = conn();
        org_policy::cache_local_only(&conn, "Northwind");
        let governed = org_policy::note_in(&conn, org_policy::TEST_ORG_WORKSPACE);
        let personal = org_policy::note_in(&conn, "");
        let cfg = config();
        for lang in ["en", "no"] {
            let p = resolve_for_note(&conn, &cfg, &governed, lang);
            assert_eq!(p.provider(), crate::providers::ProviderId::Local, "{lang}");
            assert!(check(&conn, &governed, &p).is_ok());
        }
        assert_eq!(resolve_for_note(&conn, &cfg, &personal, "en"), cfg.default);
        assert_eq!(resolve_for_note(&conn, &cfg, &personal, "no"), cfg.per_language["no"]);
    }

    #[test]
    fn saving_the_config_follows_only_the_managed_profile() {
        let (_d, conn) = conn();
        org_policy::cache_local_only(&conn, "Northwind");
        assert!(validate(&config(), &org_policy::effective_managed(), None).is_empty());
    }
}
