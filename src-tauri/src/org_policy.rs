//! Organisation policies: what the organisations a user belongs to enforce on
//! this Mac.
//!
//! The server's `GET /api/humla/org/config` is fetched by `cloud_status` and the
//! sync worker and cached in the `org_config` setting, which only [`store`]
//! writes. The cache is released only by an authenticated answer listing no
//! organisations; a failed fetch, a signed-out session or a pending second
//! factor leaves it in force. [`effective`] reads it and merges every source
//! strictest-wins.

use rusqlite::Connection;
use serde::{Deserialize, Deserializer, Serialize};

use crate::providers::ProviderId;

pub(crate) const SETTING_ORG_CONFIG: &str = "org_config";
pub(crate) const CONFIG_PATH: &str = "/api/humla/org/config";

// ---- the server's answer ----------------------------------------------------

/// The body of `GET /api/humla/org/config`. `orgs` and every policy field are
/// required: a body missing them must fail to parse rather than read as "no
/// organisation", which would release the cache.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OrgConfig {
    pub orgs: Vec<Org>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Org {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub policy_reason: String,
    #[serde(default)]
    pub updated: String,
    pub policy: OrgPolicy,
    #[serde(default)]
    pub vocabulary: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub templates: Vec<OrgTemplate>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OrgPolicy {
    pub local_only: bool,
    /// Days before an organisation's notes are deleted; 0 is off.
    #[serde(deserialize_with = "days")]
    pub retention_days: u32,
    pub telemetry_off: bool,
    pub audio_forbidden: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct OrgTemplate {
    pub client_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub deleted: bool,
}

/// PocketBase numbers may arrive as `30` or `30.0`.
fn days<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let n = f64::deserialize(d)?;
    if !n.is_finite() || n < 0.0 {
        return Err(serde::de::Error::custom("retention_days must be a non-negative number"));
    }
    Ok(n.min(u32::MAX as f64).round() as u32)
}

// ---- fetching and caching ---------------------------------------------------

/// How a fetch of the config ended. Only `Fetched` may touch the cache.
#[derive(Debug, PartialEq)]
pub(crate) enum FetchOutcome {
    Fetched(OrgConfig),
    /// No usable session: signed out, or the token was refused.
    SignedOut,
    TwoFactorRequired,
    /// The server has no such route (an older or self-hosted server).
    Unsupported,
    Failed(String),
}

/// Classify a response from the config route.
pub(crate) fn classify(status: u16, body: &str) -> FetchOutcome {
    match status {
        200 => match serde_json::from_str::<OrgConfig>(body) {
            Ok(config) => FetchOutcome::Fetched(config),
            Err(e) => FetchOutcome::Failed(format!("unreadable organisation config: {e}")),
        },
        401 => FetchOutcome::SignedOut,
        404 => FetchOutcome::Unsupported,
        _ => {
            let reason = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from));
            if reason.as_deref() == Some("two_factor_required") {
                FetchOutcome::TwoFactorRequired
            } else {
                FetchOutcome::Failed(format!("organisation config: HTTP {status}"))
            }
        }
    }
}

/// Fetch the config with a session token.
pub(crate) async fn fetch(base: &str, token: &str) -> FetchOutcome {
    let url = format!("{}{CONFIG_PATH}", base.trim_end_matches('/'));
    let resp = match crate::http::client().get(url).bearer_auth(token).send().await {
        Ok(r) => r,
        Err(e) => return FetchOutcome::Failed(e.to_string()),
    };
    let status = resp.status().as_u16();
    match resp.text().await {
        Ok(body) => classify(status, &body),
        Err(e) => FetchOutcome::Failed(e.to_string()),
    }
}

/// Apply a fetch to the cache. Returns whether the cached config changed.
pub(crate) fn store(conn: &Connection, outcome: &FetchOutcome) -> anyhow::Result<bool> {
    let FetchOutcome::Fetched(config) = outcome else {
        return Ok(false);
    };
    let json = serde_json::to_string(config)?;
    if crate::db::get_setting(conn, SETTING_ORG_CONFIG)?.as_deref() == Some(json.as_str()) {
        return Ok(false);
    }
    crate::db::set_setting(conn, SETTING_ORG_CONFIG, &json)?;
    Ok(true)
}

/// The cached config; `None` when nothing has been fetched yet. A cache that
/// can't be read is an error, never "no organisation".
pub(crate) fn cached(conn: &Connection) -> anyhow::Result<Option<OrgConfig>> {
    match crate::db::get_setting(conn, SETTING_ORG_CONFIG)? {
        None => Ok(None),
        Some(json) => Ok(Some(serde_json::from_str(&json)?)),
    }
}

// ---- the effective policy ---------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    LocalOnly,
    RetentionDays,
    TelemetryOff,
    AudioForbidden,
}

/// Who enforces one policy, and why.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PolicyRule {
    pub kind: PolicyKind,
    pub source: String,
    pub reason: String,
}

/// One set of policies with the name it is shown under. Organisations from the
/// server are layers; a managed configuration profile will be another.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layer {
    pub source: String,
    pub reason: String,
    pub policy: OrgPolicy,
}

/// The policies in force on this Mac. `org_name` and `reason` belong to the
/// first rule; `rules` names the source of each policy that is on.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EffectivePolicy {
    pub org_name: Option<String>,
    pub reason: Option<String>,
    pub local_only: bool,
    /// 0 is off.
    pub retention_days: u32,
    pub telemetry_off: bool,
    pub audio_forbidden: bool,
    pub rules: Vec<PolicyRule>,
}

/// Merge layers strictest-wins: a flag is on if any layer turns it on, and the
/// shortest non-zero retention applies. Each rule names the first layer that
/// set the winning value.
pub(crate) fn merge(layers: &[Layer]) -> EffectivePolicy {
    let mut out = EffectivePolicy::default();
    let rule = |kind, layer: &Layer| PolicyRule {
        kind,
        source: layer.source.clone(),
        reason: layer.reason.clone(),
    };
    let first = |on: fn(&OrgPolicy) -> bool| layers.iter().find(|l| on(&l.policy));
    if let Some(l) = first(|p| p.local_only) {
        out.local_only = true;
        out.rules.push(rule(PolicyKind::LocalOnly, l));
    }
    if let Some(l) = layers
        .iter()
        .filter(|l| l.policy.retention_days > 0)
        .min_by_key(|l| l.policy.retention_days)
    {
        out.retention_days = l.policy.retention_days;
        out.rules.push(rule(PolicyKind::RetentionDays, l));
    }
    if let Some(l) = first(|p| p.telemetry_off) {
        out.telemetry_off = true;
        out.rules.push(rule(PolicyKind::TelemetryOff, l));
    }
    if let Some(l) = first(|p| p.audio_forbidden) {
        out.audio_forbidden = true;
        out.rules.push(rule(PolicyKind::AudioForbidden, l));
    }
    if let Some(r) = out.rules.first() {
        out.org_name = Some(r.source.clone());
        out.reason = Some(r.reason.clone()).filter(|s| !s.is_empty());
    }
    out
}

fn server_layers(config: &OrgConfig) -> Vec<Layer> {
    config
        .orgs
        .iter()
        .map(|o| Layer { source: o.name.clone(), reason: o.policy_reason.clone(), policy: o.policy })
        .collect()
}

/// A layer from a managed configuration profile. None until that reader exists.
fn managed_layer() -> Option<Layer> {
    None
}

/// The policies in force. An error means they can't be known; callers that
/// enforce must refuse rather than treat it as "no policy".
pub(crate) fn effective(conn: &Connection) -> anyhow::Result<EffectivePolicy> {
    let mut layers = cached(conn)?.map(|c| server_layers(&c)).unwrap_or_default();
    layers.extend(managed_layer());
    Ok(merge(&layers))
}

// ---- local_only -------------------------------------------------------------

/// What `local_only` lets a provider reach. `src/lib/providers.ts`
/// (`LOCAL_ONLY_RULE`) mirrors [`local_only_rule`]; `providers.test.ts` reads
/// this file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalOnly {
    /// On this Mac with no URL at all (on-device Whisper), or a loopback URL.
    OnDeviceOrLoopback,
    /// A loopback URL only.
    Loopback,
    Refused,
}

pub(crate) fn local_only_rule(id: ProviderId) -> LocalOnly {
    match id {
        ProviderId::Local => LocalOnly::OnDeviceOrLoopback,
        ProviderId::Custom => LocalOnly::Loopback,
        ProviderId::OpenAi => LocalOnly::Refused,
        ProviderId::Anthropic => LocalOnly::Refused,
        ProviderId::Copilot => LocalOnly::Refused,
        ProviderId::Deepgram => LocalOnly::Refused,
        ProviderId::Groq => LocalOnly::Refused,
    }
}

/// Whether `url` is an http(s) URL whose host is `localhost`, in 127.0.0.0/8, or
/// `::1`. Mirrored by `isLoopbackUrl` in `src/lib/loopback.ts`, with the vectors
/// in `src/lib/loopbackVectors.json` pinning both.
pub(crate) fn is_loopback_url(url: &str) -> bool {
    use std::net::{IpAddr, Ipv6Addr};
    let Ok(url) = reqwest::Url::parse(url.trim()) else { return false };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    // The parser has already normalised the host: lowercase, IPv4 in dotted
    // quads, IPv6 in brackets.
    let Some(host) = url.host_str() else { return false };
    if host == "localhost" {
        return true;
    }
    match host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(IpAddr::V6(ip)) => ip == Ipv6Addr::LOCALHOST,
        Err(_) => false,
    }
}

/// Whether `local_only` lets note content go to this provider at `base_url`.
/// `None` means the provider is reached without a URL: on-device Whisper for
/// `Local`, never allowed for `Custom`.
pub(crate) fn allows_endpoint(id: ProviderId, base_url: Option<&str>) -> bool {
    match local_only_rule(id) {
        LocalOnly::OnDeviceOrLoopback => base_url.is_none_or(is_loopback_url),
        LocalOnly::Loopback => base_url.is_some_and(is_loopback_url),
        LocalOnly::Refused => false,
    }
}

impl EffectivePolicy {
    pub(crate) fn allows(&self, id: ProviderId, base_url: Option<&str>) -> bool {
        !self.local_only || allows_endpoint(id, base_url)
    }

    /// Who set `local_only`, if anyone.
    pub(crate) fn local_only_source(&self) -> Option<&str> {
        self.rules.iter().find(|r| r.kind == PolicyKind::LocalOnly).map(|r| r.source.as_str())
    }

    pub(crate) fn blocked(&self) -> anyhow::Error {
        anyhow::anyhow!(blocked_message(self.local_only_source()))
    }
}

pub(crate) fn blocked_message(source: Option<&str>) -> String {
    format!("Blocked by {}: processing stays on this Mac.", source.unwrap_or("your organisation"))
}

/// The policy, or the refusal an enforcing caller returns when it can't be read.
pub(crate) fn effective_or_refuse(conn: &Connection) -> anyhow::Result<EffectivePolicy> {
    effective(conn).map_err(|e| {
        eprintln!("org policy: unreadable, refusing: {e:#}");
        anyhow::anyhow!(blocked_message(None))
    })
}

/// Refuse unless the policy lets note content go to `id` at `base_url`.
pub(crate) fn check_endpoint(
    conn: &Connection,
    id: ProviderId,
    base_url: Option<&str>,
) -> anyhow::Result<EffectivePolicy> {
    let policy = effective_or_refuse(conn)?;
    if policy.allows(id, base_url) {
        Ok(policy)
    } else {
        Err(policy.blocked())
    }
}

/// Refuse when note content would leave this Mac at all (workspace chat, the
/// MCP server).
pub(crate) fn check_leaves_mac(conn: &Connection) -> anyhow::Result<()> {
    let policy = effective_or_refuse(conn)?;
    if policy.local_only {
        Err(policy.blocked())
    } else {
        Ok(())
    }
}

/// Cache one organisation with only `local_only` on, for other modules' tests.
#[cfg(test)]
pub(crate) fn cache_local_only(conn: &Connection, org: &str) {
    let body = format!(
        r#"{{"orgs":[{{"id":"o1","name":"{org}","policy":{{"local_only":true,"retention_days":0,"telemetry_off":false,"audio_forbidden":false}}}}]}}"#
    );
    store(conn, &classify(200, &body)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::REGISTRY;

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("notes.sqlite")).unwrap();
        (dir, conn)
    }

    const ONE_ORG: &str = r#"{"orgs":[{"id":"o1","name":"Northwind","policy_reason":"Nothing leaves this Mac.",
        "updated":"2026-11-17 10:00:00.000Z",
        "policy":{"local_only":true,"retention_days":30,"telemetry_off":true,"audio_forbidden":false},
        "vocabulary":["Northwind"],"workspaces":["w1"],
        "templates":[{"client_id":"t1","name":"Case meeting","content":"…","workspaces":[],"deleted":false}]}]}"#;

    fn policy(local_only: bool, retention_days: u32, telemetry_off: bool, audio_forbidden: bool) -> OrgPolicy {
        OrgPolicy { local_only, retention_days, telemetry_off, audio_forbidden }
    }

    fn layer(source: &str, p: OrgPolicy) -> Layer {
        Layer { source: source.into(), reason: format!("{source} says so"), policy: p }
    }

    fn source(p: &EffectivePolicy, kind: PolicyKind) -> &str {
        &p.rules.iter().find(|r| r.kind == kind).unwrap().source
    }

    fn cache_one_org(conn: &Connection) {
        assert!(store(conn, &classify(200, ONE_ORG)).unwrap());
    }

    #[test]
    fn parses_the_route_shape() {
        let FetchOutcome::Fetched(config) = classify(200, ONE_ORG) else { panic!() };
        let org = &config.orgs[0];
        assert_eq!(org.name, "Northwind");
        assert_eq!(org.policy, policy(true, 30, true, false));
        assert_eq!(org.workspaces, ["w1"]);
        assert_eq!(org.templates[0].client_id, "t1");
    }

    #[test]
    fn retention_accepts_a_float() {
        let body = ONE_ORG.replace("\"retention_days\":30", "\"retention_days\":14.0");
        let FetchOutcome::Fetched(config) = classify(200, &body) else { panic!() };
        assert_eq!(config.orgs[0].policy.retention_days, 14);
    }

    // A body missing what it should carry is a failure, not an empty config:
    // reading it as "no organisation" would release the policies.
    #[test]
    fn an_incomplete_body_never_reads_as_no_organisation() {
        let missing_policy_field = ONE_ORG.replace("\"local_only\":true,", "");
        for body in ["{}", r#"{"orgs":null}"#, "<html>502</html>", "", &missing_policy_field] {
            assert!(matches!(classify(200, body), FetchOutcome::Failed(_)), "{body:?}");
        }
    }

    #[test]
    fn classifies_failures() {
        assert_eq!(classify(401, "{}"), FetchOutcome::SignedOut);
        assert_eq!(classify(404, ""), FetchOutcome::Unsupported);
        assert_eq!(
            classify(403, r#"{"reason":"two_factor_required"}"#),
            FetchOutcome::TwoFactorRequired
        );
        assert!(matches!(classify(500, "oops"), FetchOutcome::Failed(_)));
        assert!(matches!(classify(403, "{}"), FetchOutcome::Failed(_)));
    }

    #[test]
    fn nothing_but_a_fetched_config_touches_the_cache() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        let before = effective(&conn).unwrap();
        assert!(before.local_only);
        for outcome in [
            FetchOutcome::SignedOut,
            FetchOutcome::TwoFactorRequired,
            FetchOutcome::Unsupported,
            FetchOutcome::Failed("timeout".into()),
            classify(200, "{}"),
            classify(502, "Bad gateway"),
        ] {
            assert!(!store(&conn, &outcome).unwrap(), "{outcome:?}");
            assert_eq!(effective(&conn).unwrap(), before, "{outcome:?}");
        }
    }

    #[test]
    fn an_authenticated_empty_answer_releases_the_policies() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        assert!(store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap());
        assert_eq!(effective(&conn).unwrap(), EffectivePolicy::default());
    }

    #[test]
    fn signing_out_keeps_the_cache() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        // What `cloud_logout` clears.
        crate::db::delete_settings_with_prefix(&conn, "roster_cache:").unwrap();
        assert!(effective(&conn).unwrap().local_only);
    }

    #[test]
    fn storing_the_same_config_reports_no_change() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        assert!(!store(&conn, &classify(200, ONE_ORG)).unwrap());
    }

    #[test]
    fn nothing_cached_is_no_policy() {
        let (_d, conn) = conn();
        assert_eq!(effective(&conn).unwrap(), EffectivePolicy::default());
    }

    // The `.catch(() => null)` trap: an unreadable cache must not collapse into
    // "no policy" for a caller that enforces.
    #[test]
    fn an_unreadable_cache_is_an_error() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(effective(&conn).is_err());
    }

    #[test]
    fn merges_strictest_wins() {
        let merged = merge(&[
            layer("A", policy(false, 90, true, false)),
            layer("B", policy(true, 30, false, false)),
            layer("C", policy(true, 0, true, true)),
        ]);
        assert!(merged.local_only && merged.telemetry_off && merged.audio_forbidden);
        assert_eq!(merged.retention_days, 30);
        assert_eq!(source(&merged, PolicyKind::LocalOnly), "B");
        assert_eq!(source(&merged, PolicyKind::RetentionDays), "B");
        assert_eq!(source(&merged, PolicyKind::TelemetryOff), "A");
        assert_eq!(source(&merged, PolicyKind::AudioForbidden), "C");
        assert_eq!(merged.org_name.as_deref(), Some("B"));
        assert_eq!(merged.reason.as_deref(), Some("B says so"));
    }

    #[test]
    fn an_org_with_every_policy_off_enforces_nothing() {
        let merged = merge(&[layer("A", OrgPolicy::default())]);
        assert_eq!(merged, EffectivePolicy::default());
    }

    #[test]
    fn loopback_matches_the_shared_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../src/lib/loopbackVectors.json")).unwrap();
        for url in v["loopback"].as_array().unwrap() {
            assert!(is_loopback_url(url.as_str().unwrap()), "{url}");
        }
        for url in v["elsewhere"].as_array().unwrap() {
            assert!(!is_loopback_url(url.as_str().unwrap()), "{url}");
        }
    }

    #[test]
    fn local_only_keeps_providers_on_this_mac() {
        let local = Some("http://localhost:11434/v1");
        let lan = Some("http://192.168.1.20:11434/v1");
        assert!(allows_endpoint(ProviderId::Local, None), "on-device Whisper");
        assert!(allows_endpoint(ProviderId::Local, local));
        assert!(!allows_endpoint(ProviderId::Local, lan));
        assert!(allows_endpoint(ProviderId::Custom, local));
        assert!(!allows_endpoint(ProviderId::Custom, lan));
        assert!(!allows_endpoint(ProviderId::Custom, None));
        for spec in REGISTRY {
            if !matches!(spec.id, ProviderId::Local | ProviderId::Custom) {
                assert!(!allows_endpoint(spec.id, None), "{}", spec.id_str);
                assert!(!allows_endpoint(spec.id, local), "{}", spec.id_str);
            }
        }
    }

    #[test]
    fn without_local_only_everything_is_allowed() {
        let policy = merge(&[layer("A", policy(false, 30, true, true))]);
        for spec in REGISTRY {
            assert!(policy.allows(spec.id, Some("https://api.example.com")), "{}", spec.id_str);
        }
    }

    #[test]
    fn refusals_name_the_organisation_that_set_local_only() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        let err = check_endpoint(&conn, ProviderId::OpenAi, None).unwrap_err().to_string();
        assert_eq!(err, "Blocked by Northwind: processing stays on this Mac.");
        assert!(check_endpoint(&conn, ProviderId::Local, None).is_ok());
        assert_eq!(check_leaves_mac(&conn).unwrap_err().to_string(), err);
    }

    #[test]
    fn an_unreadable_cache_refuses_every_endpoint() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        for spec in REGISTRY {
            let err = check_endpoint(&conn, spec.id, Some("http://localhost:11434/v1")).unwrap_err();
            assert_eq!(err.to_string(), "Blocked by your organisation: processing stays on this Mac.");
        }
        assert!(check_leaves_mac(&conn).is_err());
    }

    #[test]
    fn no_policy_refuses_nothing() {
        let (_d, conn) = conn();
        assert!(check_endpoint(&conn, ProviderId::OpenAi, None).is_ok());
        assert!(check_leaves_mac(&conn).is_ok());
    }
}
