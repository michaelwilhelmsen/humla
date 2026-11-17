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

// ---- templates and vocabulary ------------------------------------------------

/// Bring the organisation rows in `summary_prompts` in line with `config`:
/// upsert each live template whose name, text or departments differ, and delete
/// tombstoned ones and every org row the config no longer lists. The row id
/// comes from the organisation and the template's `client_id` alone, so a
/// synced note's `custom:<id>` resolves on every member's Mac. A row the user
/// owns is never overwritten. Returns whether anything changed.
pub(crate) fn apply_templates(conn: &Connection, config: &OrgConfig) -> anyhow::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let mut changed = false;
    let mut live: Vec<(String, String)> = Vec::new();
    for org in &config.orgs {
        for t in &org.templates {
            let Some(id) = template_row_id(&org.id, &t.client_id) else { continue };
            if t.deleted {
                continue;
            }
            let content = plain_text(&t.content);
            let departments = serde_json::to_string(&t.workspaces)?;
            let now = crate::db::now_ms();
            changed |= tx.execute(
                "INSERT INTO summary_prompts
                     (id, name, content, created_at, updated_at, workspace_id, org_id, org_workspaces)
                 VALUES (?1, ?2, ?3, ?4, ?4, '', ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                     name = excluded.name, content = excluded.content,
                     org_workspaces = excluded.org_workspaces, updated_at = excluded.updated_at
                 WHERE summary_prompts.org_id = excluded.org_id
                   AND (summary_prompts.name IS NOT excluded.name
                        OR summary_prompts.content IS NOT excluded.content
                        OR summary_prompts.org_workspaces IS NOT excluded.org_workspaces)",
                rusqlite::params![id, t.name, content, now, org.id, departments],
            )? > 0;
            live.push((org.id.clone(), id));
        }
    }
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT org_id, id FROM summary_prompts WHERE org_id != ''")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (org_id, id) in rows.into_iter().filter(|row| !live.contains(row)) {
        tx.execute(
            "DELETE FROM summary_prompts WHERE id = ?1 AND org_id = ?2",
            rusqlite::params![id, org_id],
        )?;
        changed = true;
    }
    tx.commit()?;
    Ok(changed)
}

/// The local row id of an organisation's template: `org_<org id>_<client_id>`.
/// PocketBase ids are alphanumeric, so the first `_` after the prefix ends the
/// org id. `None` for ids unsafe as a row id or inside `custom:<id>`.
pub(crate) fn template_row_id(org_id: &str, client_id: &str) -> Option<String> {
    let org_ok = !org_id.is_empty() && org_id.len() <= 32 && org_id.bytes().all(|c| c.is_ascii_alphanumeric());
    let client_ok = !client_id.is_empty()
        && client_id.len() <= 64
        && client_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    (org_ok && client_ok).then(|| format!("org_{org_id}_{client_id}"))
}

/// A template's text from PocketBase's editor field: paragraphs, headings,
/// list items and `<br>` end a line, other tags are dropped and entities are
/// decoded. A `<` not followed by a letter, `/` or `!` is kept as text.
pub(crate) fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let is_tag = after.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        match after.find('>').filter(|_| is_tag) {
            Some(end) => {
                let tag = after[..end].trim_start_matches('/').trim().to_ascii_lowercase();
                let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                let closing = after.starts_with('/');
                let breaks = name == "br"
                    || (closing && matches!(name.as_str(), "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"));
                if breaks {
                    out.push('\n');
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out).trim().to_string()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let decoded = after.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &after[..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What the Templates and Vocabulary sections show of one organisation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrgSummary {
    pub id: String,
    pub name: String,
    pub reason: String,
    pub vocabulary: Vec<String>,
    /// The organisation's departments the user is in.
    pub workspaces: Vec<String>,
}

pub(crate) fn summaries(config: Option<&OrgConfig>) -> Vec<OrgSummary> {
    config
        .map(|c| {
            c.orgs
                .iter()
                .map(|o| OrgSummary {
                    id: o.id.clone(),
                    name: o.name.clone(),
                    reason: o.policy_reason.clone(),
                    vocabulary: o.vocabulary.clone(),
                    workspaces: o.workspaces.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every organisation's terms in order. Nothing cached, or a cache that can't
/// be read, adds none.
pub(crate) fn vocabulary(conn: &Connection) -> Vec<String> {
    match cached(conn) {
        Ok(config) => config.into_iter().flat_map(|c| c.orgs).flat_map(|o| o.vocabulary).collect(),
        Err(e) => {
            eprintln!("org policy: unreadable, no organisation terms: {e:#}");
            Vec::new()
        }
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
    format!("Blocked by {}: processing stays on this Mac.", source.unwrap_or(UNKNOWN_SOURCE))
}

/// The policy for a caller that enforces. A cache that can't be read is read as
/// `local_only` on, the strictest setting this module can enforce: it only
/// narrows processing to this Mac, so on-device and loopback work carries on.
pub(crate) fn effective_or_strictest(conn: &Connection) -> EffectivePolicy {
    effective(conn).unwrap_or_else(|e| {
        eprintln!("org policy: unreadable, enforcing local_only: {e:#}");
        merge(&[Layer {
            source: UNKNOWN_SOURCE.into(),
            reason: String::new(),
            policy: OrgPolicy { local_only: true, ..OrgPolicy::default() },
        }])
    })
}

const UNKNOWN_SOURCE: &str = "your organisation";

/// Refuse unless the policy lets note content go to `id` at `base_url`.
pub(crate) fn check_endpoint(
    conn: &Connection,
    id: ProviderId,
    base_url: Option<&str>,
) -> anyhow::Result<EffectivePolicy> {
    let policy = effective_or_strictest(conn);
    if policy.allows(id, base_url) {
        Ok(policy)
    } else {
        Err(policy.blocked())
    }
}

/// Refuse when note content would leave this Mac at all (workspace chat, the
/// MCP server).
pub(crate) fn check_leaves_mac(conn: &Connection) -> anyhow::Result<()> {
    let policy = effective_or_strictest(conn);
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
    fn an_unreadable_cache_reads_as_local_only() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(check_endpoint(&conn, ProviderId::Local, None).is_ok(), "on-device Whisper");
        assert!(check_endpoint(&conn, ProviderId::Local, Some("http://localhost:11434/v1")).is_ok());
        assert!(check_endpoint(&conn, ProviderId::Custom, Some("http://127.0.0.1:1234/v1")).is_ok());
        for spec in REGISTRY {
            if !matches!(spec.id, ProviderId::Local | ProviderId::Custom) {
                let err = check_endpoint(&conn, spec.id, None).unwrap_err();
                assert_eq!(err.to_string(), "Blocked by your organisation: processing stays on this Mac.");
            }
        }
        assert!(check_endpoint(&conn, ProviderId::Custom, Some("https://api.example.com")).is_err());
        assert!(check_leaves_mac(&conn).is_err());
    }

    fn template_body(templates: &str) -> OrgConfig {
        let body = format!(
            r#"{{"orgs":[{{"id":"o1","name":"Northwind","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false}},"templates":[{templates}]}}]}}"#
        );
        let FetchOutcome::Fetched(config) = classify(200, &body) else { panic!("{body}") };
        config
    }

    fn org_rows(conn: &Connection) -> Vec<(String, String, String, String)> {
        let mut stmt = conn
            .prepare("SELECT id, name, content, org_workspaces FROM summary_prompts WHERE org_id != '' ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    const CASE: &str = r#"{"client_id":"t1","name":"Case meeting","content":"Decisions.","workspaces":[],"deleted":false}"#;
    const INTAKE: &str = r#"{"client_id":"t2","name":"Intake","content":"Needs.","workspaces":["w1"],"deleted":false}"#;

    #[test]
    fn templates_are_cached_as_read_only_prompt_rows() {
        let (_d, conn) = conn();
        assert!(apply_templates(&conn, &template_body(&format!("{CASE},{INTAKE}"))).unwrap());
        assert_eq!(
            org_rows(&conn),
            [
                ("org_o1_t1".into(), "Case meeting".into(), "Decisions.".into(), "[]".into()),
                ("org_o1_t2".into(), "Intake".into(), "Needs.".into(), r#"["w1"]"#.into()),
            ]
        );
        let row = crate::db::get_summary_prompt(&conn, "org_o1_t2").unwrap();
        assert_eq!((row.org_id.as_str(), row.workspace_id.as_str()), ("o1", ""));
        assert_eq!(row.org_workspaces, ["w1"]);
        assert!(crate::db::update_summary_prompt(&conn, "org_o1_t1", "Mine", "x").is_err());
        assert!(crate::db::delete_summary_prompt(&conn, "org_o1_t1").is_err());
        assert_eq!(org_rows(&conn).len(), 2);
    }

    #[test]
    fn an_unchanged_config_writes_nothing() {
        let (_d, conn) = conn();
        let config = template_body(CASE);
        apply_templates(&conn, &config).unwrap();
        let before = crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap().updated_at;
        assert!(!apply_templates(&conn, &config).unwrap());
        assert_eq!(crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap().updated_at, before);
    }

    // The org record's `updated` doesn't move when a template is edited.
    #[test]
    fn an_edited_template_is_seen_by_its_content() {
        let (_d, conn) = conn();
        apply_templates(&conn, &template_body(CASE)).unwrap();
        let edited = CASE.replace("Decisions.", "Decisions and owners.");
        assert!(apply_templates(&conn, &template_body(&edited)).unwrap());
        assert_eq!(crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap().content, "Decisions and owners.");
        let narrowed = edited.replace(r#""workspaces":[]"#, r#""workspaces":["w2"]"#);
        assert!(apply_templates(&conn, &template_body(&narrowed)).unwrap());
        assert_eq!(crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap().org_workspaces, ["w2"]);
    }

    #[test]
    fn tombstones_and_missing_templates_are_removed() {
        let (_d, conn) = conn();
        apply_templates(&conn, &template_body(&format!("{CASE},{INTAKE}"))).unwrap();
        let tombstone = CASE.replace(r#""deleted":false"#, r#""deleted":true"#);
        assert!(apply_templates(&conn, &template_body(&tombstone)).unwrap());
        assert!(org_rows(&conn).is_empty(), "t1 tombstoned, t2 no longer listed");
    }

    #[test]
    fn a_released_config_removes_every_org_row_and_nothing_else() {
        let (_d, conn) = conn();
        let mine = crate::db::create_summary_prompt(&conn, "Mine", "My text", "").unwrap();
        let shared = crate::db::create_summary_prompt(&conn, "Shared", "Team text", "w1").unwrap();
        apply_templates(&conn, &template_body(CASE)).unwrap();
        assert!(apply_templates(&conn, &OrgConfig::default()).unwrap());
        assert!(org_rows(&conn).is_empty());
        assert!(crate::db::get_summary_prompt(&conn, &mine.id).is_ok());
        assert!(crate::db::get_summary_prompt(&conn, &shared.id).is_ok());
    }

    #[test]
    fn a_users_own_row_is_never_overwritten() {
        let (_d, conn) = conn();
        let now = crate::db::now_ms();
        conn.execute(
            "INSERT INTO summary_prompts (id, name, content, created_at, updated_at) VALUES ('org_o1_t1', 'Mine', 'My text', ?1, ?1)",
            [now],
        )
        .unwrap();
        apply_templates(&conn, &template_body(CASE)).unwrap();
        let row = crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap();
        assert_eq!((row.name.as_str(), row.org_id.as_str()), ("Mine", ""));
        apply_templates(&conn, &OrgConfig::default()).unwrap();
        assert!(crate::db::get_summary_prompt(&conn, "org_o1_t1").is_ok());
    }

    #[test]
    fn two_organisations_may_share_a_client_id() {
        let (_d, conn) = conn();
        let org = |id: &str, content: &str| {
            format!(
                r#"{{"id":"{id}","name":"{id}","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false}},"templates":[{{"client_id":"t1","name":"Case meeting","content":"{content}","workspaces":[],"deleted":false}}]}}"#
            )
        };
        let body = format!(r#"{{"orgs":[{},{}]}}"#, org("o1", "First"), org("o2", "Second"));
        let FetchOutcome::Fetched(config) = classify(200, &body) else { panic!() };
        assert!(apply_templates(&conn, &config).unwrap());
        assert_eq!(crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap().content, "First");
        assert_eq!(crate::db::get_summary_prompt(&conn, "org_o2_t1").unwrap().content, "Second");
        assert!(!apply_templates(&conn, &config).unwrap());
    }

    #[test]
    fn row_ids_are_deterministic_and_never_ambiguous() {
        assert_eq!(template_row_id("c0kb400ij62hde6", "all").as_deref(), Some("org_c0kb400ij62hde6_all"));
        assert_eq!(template_row_id("o1", "a_b").as_deref(), Some("org_o1_a_b"));
        assert_eq!(template_row_id("o_1", "b"), None, "an org id with `_` would be ambiguous");
        assert_eq!(template_row_id("", "t1"), None);
        assert_eq!(template_row_id("o1", "../x"), None);
    }

    #[test]
    fn editor_html_is_stored_as_plain_text() {
        // The server's real sample body.
        let sample = CASE.replace("Decisions.", "<p>OnlyA</p>");
        let (_d, conn) = conn();
        apply_templates(&conn, &template_body(&sample)).unwrap();
        assert_eq!(crate::db::get_summary_prompt(&conn, "org_o1_t1").unwrap().content, "OnlyA");

        assert_eq!(plain_text("<p>List the decisions.</p><p>Then owners &amp; dates.</p>"), "List the decisions.\nThen owners & dates.");
        assert_eq!(plain_text("Line one<br>Line two<BR />three"), "Line one\nLine two\nthree");
        assert_eq!(plain_text("<ul><li>One</li><li>Two</li></ul>"), "One\nTwo");
        assert_eq!(plain_text("<p><strong>Bold</strong> and <em>soft</em>&nbsp;text</p>"), "Bold and soft text");
        assert_eq!(plain_text("&lt;tag&gt; &#229; &#xE6; &quot;q&quot; &unknown; a & b"), "<tag> å æ \"q\" &unknown; a & b");
        assert_eq!(plain_text("Keep x < y and plain text"), "Keep x < y and plain text");
    }

    #[test]
    fn an_unsafe_client_id_is_skipped() {
        let (_d, conn) = conn();
        let bad = CASE.replace(r#""client_id":"t1""#, r#""client_id":"../x""#);
        assert!(!apply_templates(&conn, &template_body(&bad)).unwrap());
        assert!(org_rows(&conn).is_empty());
    }

    #[test]
    fn no_organisation_adds_no_rows_and_no_terms() {
        let (_d, conn) = conn();
        assert!(vocabulary(&conn).is_empty());
        assert!(!apply_templates(&conn, &OrgConfig::default()).unwrap());
        store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap();
        assert!(vocabulary(&conn).is_empty());
        assert!(summaries(cached(&conn).unwrap().as_ref()).is_empty());
    }

    #[test]
    fn a_summary_carries_the_organisations_departments() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        let s = summaries(cached(&conn).unwrap().as_ref());
        assert_eq!((s[0].name.as_str(), s[0].workspaces.as_slice()), ("Northwind", &["w1".to_string()][..]));
    }

    #[test]
    fn terms_come_from_every_organisation_in_order() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        assert_eq!(vocabulary(&conn), ["Northwind"]);
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(vocabulary(&conn).is_empty());
    }

    #[test]
    fn no_policy_refuses_nothing() {
        let (_d, conn) = conn();
        assert!(check_endpoint(&conn, ProviderId::OpenAi, None).is_ok());
        assert!(check_leaves_mac(&conn).is_ok());
    }
}
