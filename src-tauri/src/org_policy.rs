//! Organisation policies: what the organisations a user belongs to enforce on
//! this Mac.
//!
//! The server's `GET /api/humla/org/config` is fetched by `cloud_status` and the
//! sync worker and cached in the `org_config` setting, which only [`store`]
//! writes. The cache is released only by an authenticated answer listing no
//! organisations; a failed fetch, a signed-out session or a pending second
//! factor leaves it in force. An organisation's policies govern only notes in
//! its own departments ([`effective_for`]); a managed configuration profile
//! ([`managed`]), which no sign-out releases, governs the whole Mac.

use std::collections::BTreeMap;

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
    /// The user's departments behind a 2FA requirement they haven't met: the
    /// policy still governs their notes, though no content comes with them.
    #[serde(default)]
    pub locked_workspaces: Vec<String>,
    #[serde(default)]
    pub templates: Vec<OrgTemplate>,
}

impl Org {
    /// Every department of the user's it names, open or behind 2FA.
    pub(crate) fn departments(&self) -> impl Iterator<Item = &str> {
        self.workspaces.iter().chain(&self.locked_workspaces).map(|w| w.trim()).filter(|w| !w.is_empty())
    }

    fn claims(&self, workspace_id: &str) -> bool {
        self.departments().any(|w| w == workspace_id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OrgPolicy {
    pub local_only: bool,
    /// Days before an organisation's notes are deleted; 0 is off.
    #[serde(deserialize_with = "days")]
    pub retention_days: u32,
    pub telemetry_off: bool,
    pub audio_forbidden: bool,
    /// Opening a note in the organisation's workspaces is recorded in its audit
    /// log (`org_audit`). Per organisation, so it never joins [`merge`]. Optional,
    /// since a server from before the audit log doesn't send it.
    #[serde(default)]
    pub audit_reads: bool,
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
    /// Opening its notes is recorded (`OrgPolicy::audit_reads`).
    pub audit_reads: bool,
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
                    audit_reads: o.policy.audit_reads,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The terms of the organisation owning `workspace_id`, in order. Personal, a
/// workspace in no organisation, nothing cached, or a cache that can't be read
/// adds none.
pub(crate) fn vocabulary_for(conn: &Connection, workspace_id: Option<&str>) -> Vec<String> {
    match cached(conn) {
        Ok(config) => config
            .iter()
            .flat_map(|c| orgs_for(c, workspace_id))
            .flat_map(|o| o.vocabulary.iter().cloned())
            .collect(),
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

/// One set of policies with the name it is shown under: an organisation from
/// the server, or a managed configuration profile.
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

fn layer_of(org: &Org) -> Layer {
    Layer { source: org.name.clone(), reason: org.policy_reason.clone(), policy: org.policy }
}

/// A workspace id that can name a department: `None` for Personal and blanks.
fn department(workspace_id: Option<&str>) -> Option<&str> {
    workspace_id.map(str::trim).filter(|w| !w.is_empty())
}

/// The organisations governing `workspace_id`: those naming it as a
/// department, open or locked. A workspace belongs to one organisation; should
/// two name it, both apply. A server from before `locked_workspaces` names no
/// department of an organisation whose 2FA is unmet, so while one names none
/// at all and has a policy on, it governs every workspace no organisation
/// claims. Personal never.
pub(crate) fn orgs_for<'a>(config: &'a OrgConfig, workspace_id: Option<&str>) -> Vec<&'a Org> {
    let Some(ws) = department(workspace_id) else { return Vec::new() };
    let claimed: Vec<&Org> = config.orgs.iter().filter(|o| o.claims(ws)).collect();
    if !claimed.is_empty() {
        return claimed;
    }
    config.orgs.iter().filter(|o| o.departments().next().is_none() && governs_notes(&o.policy)).collect()
}

/// Whether an organisation's policy governs notes in `workspace_id`, for a
/// sync conflict copy. A cache that can't be read says yes; nothing cached
/// says no.
pub(crate) fn governs_workspace(conn: &Connection, workspace_id: &str) -> bool {
    match cached(conn) {
        Ok(Some(config)) => orgs_for(&config, Some(workspace_id)).iter().any(|o| governs_notes(&o.policy)),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// Whether an organisation's policy reaches its notes, as opposed to the app
/// (`telemetry_off`) or nothing at all.
pub(crate) fn governs_notes(p: &OrgPolicy) -> bool {
    p.local_only || p.audio_forbidden || p.retention_days > 0
}

/// The policies in force for notes in `workspace_id`: the managed profile and
/// the organisation that owns that department. Personal, a blank id and a
/// workspace in no organisation get the profile alone. An error means the
/// cache can't be read for a department; callers that enforce must refuse
/// rather than treat it as "no policy".
pub(crate) fn effective_for(conn: &Connection, workspace_id: Option<&str>) -> anyhow::Result<EffectivePolicy> {
    effective_for_with(conn, workspace_id, managed().layer)
}

fn effective_for_with(
    conn: &Connection,
    workspace_id: Option<&str>,
    managed: Option<Layer>,
) -> anyhow::Result<EffectivePolicy> {
    let mut layers = match department(workspace_id) {
        None => Vec::new(),
        Some(_) => cached(conn)?
            .map(|c| orgs_for(&c, workspace_id).into_iter().map(layer_of).collect())
            .unwrap_or_default(),
    };
    layers.extend(managed);
    Ok(merge(&layers))
}

/// The managed configuration profile's policies alone: what governs the whole
/// Mac, and the user's own settings everywhere.
pub(crate) fn effective_managed() -> EffectivePolicy {
    merge(&managed().layer.into_iter().collect::<Vec<_>>())
}

/// What isn't tied to a note: the managed profile, and every organisation's
/// `telemetry_off`. Nothing else from the server applies to the whole app.
pub(crate) fn effective_device(conn: &Connection) -> anyhow::Result<EffectivePolicy> {
    let mut layers: Vec<Layer> = cached(conn)?
        .map(|c| {
            c.orgs
                .iter()
                .map(|o| Layer {
                    policy: OrgPolicy { telemetry_off: o.policy.telemetry_off, ..OrgPolicy::default() },
                    ..layer_of(o)
                })
                .collect()
        })
        .unwrap_or_default();
    layers.extend(managed().layer);
    Ok(merge(&layers))
}

/// Each organisation's own policies, for the Policies list.
pub(crate) fn org_policies(config: Option<&OrgConfig>) -> Vec<OrgPolicies> {
    config
        .map(|c| {
            c.orgs
                .iter()
                .map(|o| OrgPolicies {
                    org_id: o.id.clone(),
                    workspaces: o.departments().map(String::from).collect(),
                    policy: merge(&[layer_of(o)]),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One organisation's policies and the departments they govern.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrgPolicies {
    pub org_id: String,
    pub workspaces: Vec<String>,
    pub policy: EffectivePolicy,
}

/// The organisation whose policies govern notes in each department, by name.
pub(crate) fn governed_workspaces(config: Option<&OrgConfig>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for org in config.iter().flat_map(|c| &c.orgs).filter(|o| governs_notes(&o.policy)) {
        for ws in org.departments() {
            out.entry(ws.to_string()).or_insert_with(|| org.name.clone());
        }
    }
    out
}

// ---- the managed configuration profile --------------------------------------

/// The name a managed configuration profile's policies are shown under.
pub(crate) const MANAGED_SOURCE: &str = "This Mac's configuration";
const MANAGED_FLAGS: [&str; 3] = ["local_only", "telemetry_off", "audio_forbidden"];
#[cfg(target_os = "macos")]
const MANAGED_KEYS: [&str; 5] = ["local_only", "telemetry_off", "audio_forbidden", "policy_reason", "server_url"];

/// A forced preference value, as far as the profile's keys care.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum ManagedValue {
    Bool(bool),
    Text(String),
    Other,
}

/// What a managed configuration profile sets. `retention_days` is not read:
/// retention covers organisation workspaces only, which a profile can't name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Managed {
    pub layer: Option<Layer>,
    pub server_url: Option<String>,
    /// `audio_forbidden` is forced to `true` itself, not read as on from a
    /// value of the wrong type: only then may it delete stored audio.
    pub sweeps_audio: bool,
}

/// Read the forced values of the profile's keys. A flag forced to anything but
/// a boolean reads as on; a server URL that isn't an http(s) URL is ignored.
pub(crate) fn parse_managed(forced: &BTreeMap<String, ManagedValue>) -> Managed {
    let flag = |key: &str| match forced.get(key) {
        None => false,
        Some(ManagedValue::Bool(on)) => *on,
        Some(_) => true,
    };
    let layer = MANAGED_FLAGS.iter().any(|k| forced.contains_key(*k)).then(|| Layer {
        source: MANAGED_SOURCE.into(),
        reason: match forced.get("policy_reason") {
            Some(ManagedValue::Text(s)) => s.trim().to_string(),
            _ => String::new(),
        },
        policy: OrgPolicy {
            local_only: flag("local_only"),
            retention_days: 0,
            telemetry_off: flag("telemetry_off"),
            audio_forbidden: flag("audio_forbidden"),
            audit_reads: false,
        },
    });
    let server_url = match forced.get("server_url") {
        None => None,
        Some(ManagedValue::Text(s)) if is_server_url(s) => Some(s.trim().trim_end_matches('/').to_string()),
        Some(_) => {
            eprintln!("org policy: the managed server_url isn't an http(s) URL; ignored");
            None
        }
    };
    let sweeps_audio = forced.get("audio_forbidden") == Some(&ManagedValue::Bool(true));
    Managed { layer, server_url, sweeps_audio }
}

fn is_server_url(s: &str) -> bool {
    reqwest::Url::parse(s.trim())
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty()))
}

#[cfg(not(test))]
static MANAGED: parking_lot::RwLock<Option<Managed>> = parking_lot::RwLock::new(None);

#[cfg(not(test))]
fn swap_managed(fresh: Managed) -> Option<Managed> {
    MANAGED.write().replace(fresh)
}

#[cfg(not(test))]
fn last_managed() -> Option<Managed> {
    MANAGED.read().clone()
}

// Per test thread, so a test's profile never leaks into another test.
#[cfg(test)]
thread_local! {
    static MANAGED: std::cell::RefCell<Option<Managed>> = const { std::cell::RefCell::new(None) };
    static FORCED: std::cell::RefCell<BTreeMap<String, ManagedValue>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
fn swap_managed(fresh: Managed) -> Option<Managed> {
    MANAGED.with(|m| m.borrow_mut().replace(fresh))
}

#[cfg(test)]
fn last_managed() -> Option<Managed> {
    MANAGED.with(|m| m.borrow().clone())
}

/// Install a profile for this test thread; the next read sees it.
#[cfg(test)]
pub(crate) fn force_for_test(pairs: &[(&str, ManagedValue)]) {
    FORCED.with(|f| *f.borrow_mut() = pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
}

/// The managed configuration as last read; read once on first use.
pub(crate) fn managed() -> Managed {
    if let Some(m) = last_managed() {
        return m;
    }
    reload_managed();
    last_managed().unwrap_or_default()
}

/// What a re-read of the managed configuration changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ManagedChange {
    pub policy: bool,
    pub server_url: bool,
}

/// Read the managed configuration again; profiles can arrive while the app runs.
pub(crate) fn reload_managed() -> ManagedChange {
    let fresh = parse_managed(&read_forced());
    let old = swap_managed(fresh.clone()).unwrap_or_default();
    ManagedChange {
        policy: old.layer != fresh.layer || old.sweeps_audio != fresh.sweeps_audio,
        server_url: old.server_url != fresh.server_url,
    }
}

/// The preference domain an MDM profile's payload targets is the bundle id.
#[cfg(all(target_os = "macos", not(test)))]
fn read_forced() -> BTreeMap<String, ManagedValue> {
    read_forced_in("no.humla.app")
}

#[cfg(all(not(target_os = "macos"), not(test)))]
fn read_forced() -> BTreeMap<String, ManagedValue> {
    BTreeMap::new()
}

/// Tests never see this Mac's profiles, only [`force_for_test`]'s.
#[cfg(test)]
fn read_forced() -> BTreeMap<String, ManagedValue> {
    FORCED.with(|f| f.borrow().clone())
}

/// Only values a profile forces count, never the user's own `defaults write`.
#[cfg(target_os = "macos")]
fn read_forced_in(domain: &str) -> BTreeMap<String, ManagedValue> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::string::CFString;
    use core_foundation_sys::preferences::{
        CFPreferencesAppSynchronize, CFPreferencesAppValueIsForced, CFPreferencesCopyAppValue,
    };

    let app = CFString::new(domain);
    // SAFETY: `app` outlives the call. Drops this process's cached values, so
    // a profile installed since the last read is seen.
    unsafe { CFPreferencesAppSynchronize(app.as_concrete_TypeRef()) };
    let mut out = BTreeMap::new();
    for key in MANAGED_KEYS {
        let k = CFString::new(key);
        // SAFETY: both strings outlive the calls, and a non-null copy is owned
        // by us (the create rule).
        let value = unsafe {
            if CFPreferencesAppValueIsForced(k.as_concrete_TypeRef(), app.as_concrete_TypeRef()) == 0 {
                continue;
            }
            let raw = CFPreferencesCopyAppValue(k.as_concrete_TypeRef(), app.as_concrete_TypeRef());
            if raw.is_null() {
                continue;
            }
            CFType::wrap_under_create_rule(raw)
        };
        let value = if let Some(b) = value.downcast::<CFBoolean>() {
            ManagedValue::Bool(b.into())
        } else if let Some(s) = value.downcast::<CFString>() {
            ManagedValue::Text(s.to_string())
        } else {
            ManagedValue::Other
        };
        out.insert(key.to_string(), value);
    }
    out
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

/// The policy for a caller that enforces on notes in `workspace_id`. For a
/// department whose cache can't be read, `local_only`, `telemetry_off` and
/// `audio_forbidden` read as on: each only stops something leaving or staying
/// on this Mac. Retention is left off, since it deletes notes, and nothing read
/// through here may start a deletion. Personal never reads the cache.
pub(crate) fn effective_for_or_strictest(conn: &Connection, workspace_id: Option<&str>) -> EffectivePolicy {
    effective_for(conn, workspace_id).unwrap_or_else(|e| {
        eprintln!("org policy: unreadable, enforcing the strictest policy: {e:#}");
        strictest(OrgPolicy { local_only: true, telemetry_off: true, audio_forbidden: true, ..OrgPolicy::default() })
    })
}

/// [`effective_device`] for a caller that enforces: a cache that can't be read
/// turns `telemetry_off` on, the one thing the server sets app-wide.
pub(crate) fn effective_device_or_strictest(conn: &Connection) -> EffectivePolicy {
    effective_device(conn).unwrap_or_else(|e| {
        eprintln!("org policy: unreadable, sending no usage counters: {e:#}");
        strictest(OrgPolicy { telemetry_off: true, ..OrgPolicy::default() })
    })
}

fn strictest(policy: OrgPolicy) -> EffectivePolicy {
    let mut layers = vec![Layer { source: UNKNOWN_SOURCE.into(), reason: String::new(), policy }];
    layers.extend(managed().layer);
    merge(&layers)
}

/// The policy for notes in a note's workspace. A note that can't be read is
/// treated as a department whose cache can't be read, unless no organisation
/// is cached.
pub(crate) fn effective_for_note(conn: &Connection, note_id: &str) -> EffectivePolicy {
    match crate::db::get_note(conn, note_id) {
        Ok(note) => effective_for_or_strictest(conn, Some(&note.workspace_id)),
        Err(_) if cached(conn).is_ok_and(|c| c.is_none_or(|c| c.orgs.is_empty())) => effective_managed(),
        Err(e) => {
            eprintln!("org policy: note {note_id} unreadable, enforcing the strictest policy: {e:#}");
            strictest(OrgPolicy { local_only: true, telemetry_off: true, audio_forbidden: true, ..OrgPolicy::default() })
        }
    }
}

const UNKNOWN_SOURCE: &str = "your organisation";

/// Refuse unless the policy for notes in `workspace_id` lets their content go
/// to `id` at `base_url`.
pub(crate) fn check_endpoint(
    conn: &Connection,
    workspace_id: Option<&str>,
    id: ProviderId,
    base_url: Option<&str>,
) -> anyhow::Result<EffectivePolicy> {
    let policy = effective_for_or_strictest(conn, workspace_id);
    if policy.allows(id, base_url) {
        Ok(policy)
    } else {
        Err(policy.blocked())
    }
}

/// Refuse when content of notes in `workspace_id` would leave this Mac at all
/// (workspace chat, an MCP client).
pub(crate) fn check_leaves_mac(conn: &Connection, workspace_id: Option<&str>) -> anyhow::Result<()> {
    let policy = effective_for_or_strictest(conn, workspace_id);
    if policy.local_only {
        Err(policy.blocked())
    } else {
        Ok(())
    }
}

// ---- audio_forbidden: the one-time sweep ------------------------------------

/// The organisations whose `audio_forbidden` has already swept this Mac's
/// stored audio, as a JSON array of ids.
pub(crate) const SETTING_AUDIO_SWEPT: &str = "org_audio_swept";

/// An organisation whose `audio_forbidden` still owes the sweep of audio
/// already on this Mac: of notes in `workspaces`, its departments not swept
/// yet, or of every note when `all` (a managed profile).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AudioSweep {
    pub org_id: String,
    pub org_name: String,
    pub all: bool,
    pub workspaces: Vec<String>,
}

/// The id a managed configuration profile's sweep is recorded under. Not a
/// PocketBase id, which is alphanumeric, so it can't name an organisation.
pub(crate) const MANAGED_SWEEP_ID: &str = "mac-configuration";

/// How one department's sweep is recorded: `<org id>/<workspace id>`.
fn swept_key(org_id: &str, workspace_id: &str) -> String {
    format!("{org_id}/{}", workspace_id.trim())
}

/// The first organisation, then a managed profile, that forbids audio and has
/// a department not swept yet. Only a cache that reads cleanly can name an
/// organisation, and only one with an id and a name; a profile owes it only
/// when it forces `audio_forbidden` to `true`. A policy that can't be read
/// closes the audio gate through [`effective_for_or_strictest`] but never
/// deletes anything.
pub(crate) fn audio_sweep_due(conn: &Connection) -> Option<AudioSweep> {
    let config = match cached(conn) {
        Ok(Some(config)) => Some(config),
        _ => None,
    };
    let swept = match &config {
        Some(c) => migrate_swept(conn, c),
        None => swept_entries(conn),
    };
    config
        .into_iter()
        .flat_map(|c| c.orgs)
        .filter(|o| o.policy.audio_forbidden && !o.id.trim().is_empty() && !o.name.trim().is_empty())
        .find_map(|o| {
            let pending: Vec<String> = o
                .departments()
                .filter(|w| !swept.contains(&swept_key(&o.id, w)))
                .map(String::from)
                .collect();
            (!pending.is_empty()).then(|| AudioSweep { org_id: o.id.clone(), org_name: o.name.clone(), all: false, workspaces: pending })
        })
        .or_else(|| {
            (managed().sweeps_audio && !swept.iter().any(|id| id == MANAGED_SWEEP_ID)).then(|| AudioSweep {
                org_id: MANAGED_SWEEP_ID.into(),
                org_name: MANAGED_SOURCE.into(),
                all: true,
                workspaces: Vec::new(),
            })
        })
}

/// Which notes' audio a due sweep deletes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SweepScope {
    /// A managed profile's: every note on this Mac.
    Everything,
    /// An organisation's: notes in these departments.
    Workspaces(Vec<String>),
}

impl SweepScope {
    pub(crate) fn covers(&self, workspace_id: &str) -> bool {
        match self {
            SweepScope::Everything => true,
            SweepScope::Workspaces(ws) => {
                let w = workspace_id.trim();
                !w.is_empty() && ws.iter().any(|d| d.trim() == w)
            }
        }
    }
}

/// The notes `sweep` deletes the audio of.
pub(crate) fn sweep_scope(sweep: &AudioSweep) -> SweepScope {
    if sweep.all {
        SweepScope::Everything
    } else {
        SweepScope::Workspaces(sweep.workspaces.clone())
    }
}

fn swept_entries(conn: &Connection) -> Vec<String> {
    crate::db::get_setting(conn, SETTING_AUDIO_SWEPT)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// The swept entries, with each bare organisation id an older version recorded
/// replaced by that organisation's departments as `config` lists them now, so
/// a department it gains later still owes its sweep. Written back once.
fn migrate_swept(conn: &Connection, config: &OrgConfig) -> Vec<String> {
    let entries = swept_entries(conn);
    let legacy = |e: &String| e != MANAGED_SWEEP_ID && !e.contains('/');
    if !entries.iter().any(legacy) {
        return entries;
    }
    let mut out: Vec<String> = Vec::new();
    for e in &entries {
        let expanded: Vec<String> = if legacy(e) {
            config
                .orgs
                .iter()
                .filter(|o| &o.id == e)
                .flat_map(|o| o.departments().map(|w| swept_key(&o.id, w)).collect::<Vec<_>>())
                .collect()
        } else {
            vec![e.clone()]
        };
        for k in expanded {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    if let Ok(json) = serde_json::to_string(&out) {
        if let Err(e) = crate::db::set_setting(conn, SETTING_AUDIO_SWEPT, &json) {
            eprintln!("org policy: couldn't migrate the audio sweep record: {e:#}");
        }
    }
    out
}

/// Record that `sweep` ran, so its notice doesn't come back.
pub(crate) fn record_audio_sweep(conn: &Connection, sweep: &AudioSweep) -> anyhow::Result<()> {
    let mut swept = swept_entries(conn);
    let keys: Vec<String> = if sweep.all {
        vec![MANAGED_SWEEP_ID.to_string()]
    } else {
        sweep.workspaces.iter().map(|w| swept_key(&sweep.org_id, w)).collect()
    };
    let before = swept.len();
    for k in keys {
        if !swept.contains(&k) {
            swept.push(k);
        }
    }
    if swept.len() != before {
        crate::db::set_setting(conn, SETTING_AUDIO_SWEPT, &serde_json::to_string(&swept)?)?;
    }
    Ok(())
}

// ---- retention_days ---------------------------------------------------------

pub(crate) const DAY_MS: i64 = 86_400_000;

/// The smallest `created_at` read as a real time (2001-09-09). Anything below is
/// garbage or seconds: retention never deletes on it and leaves the note to the
/// server, which uses the same floor. `cloud-sync` holds its own copy.
pub(crate) const MIN_EPOCH_MS: i64 = 1_000_000_000_000;

/// The retention one workspace is under, and the organisation that sets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetentionRule {
    pub days: u32,
    pub source: String,
}

/// Each workspace under an organisation's `retention_days`, with the rule that
/// applies to it: the fewest days when several organisations list it. A blank
/// workspace id is never covered, so Personal notes (`workspace_id = ''`) are
/// out of reach.
pub(crate) fn retention_rules(config: &OrgConfig) -> BTreeMap<String, RetentionRule> {
    let mut rules: BTreeMap<String, RetentionRule> = BTreeMap::new();
    for org in config.orgs.iter().filter(|o| o.policy.retention_days > 0 && !o.id.trim().is_empty()) {
        let rule = RetentionRule { days: org.policy.retention_days, source: org.name.clone() };
        for ws in org.departments() {
            match rules.get(ws) {
                Some(have) if have.days <= rule.days => {}
                _ => {
                    rules.insert(ws.to_string(), rule.clone());
                }
            }
        }
    }
    rules
}

/// [`retention_rules`] as days alone, for the UI through `org_status`.
pub(crate) fn retention_scope(config: &OrgConfig) -> BTreeMap<String, u32> {
    retention_rules(config).into_iter().map(|(ws, r)| (ws, r.days)).collect()
}

/// The retention rules that may delete notes or refuse an edit. Only a cache
/// that reads cleanly can name one; nothing cached, or a cache that can't be
/// read, covers nothing.
pub(crate) fn retention_in_force(conn: &Connection) -> BTreeMap<String, RetentionRule> {
    match cached(conn) {
        Ok(Some(config)) => retention_rules(&config),
        Ok(None) => Default::default(),
        Err(e) => {
            eprintln!("org policy: unreadable, no retention: {e:#}");
            Default::default()
        }
    }
}

/// The rule for notes in `workspace_id`, or `None` (Personal included).
pub(crate) fn retention_rule(conn: &Connection, workspace_id: &str) -> Option<RetentionRule> {
    if workspace_id.trim().is_empty() {
        return None;
    }
    retention_in_force(conn).remove(workspace_id)
}

/// Whether an organisation naming `workspace_id` as a department forbids
/// audio, read from a cache that parses cleanly: only that may delete audio.
pub(crate) fn forbids_audio(conn: &Connection, workspace_id: &str) -> bool {
    let ws = workspace_id.trim();
    match cached(conn) {
        Ok(Some(config)) => config.orgs.iter().any(|o| o.policy.audio_forbidden && o.claims(ws)),
        _ => false,
    }
}

/// Notes created before the returned time are past `days` at `now_ms`.
pub(crate) fn retention_cutoff(now_ms: i64, days: u32) -> i64 {
    now_ms.saturating_sub(i64::from(days).saturating_mul(DAY_MS))
}

/// Whether a note created at `created_at` is past `cutoff`. An implausible
/// `created_at` never is.
pub(crate) fn is_past(created_at: i64, cutoff: i64) -> bool {
    created_at >= MIN_EPOCH_MS && created_at < cutoff
}

/// Under `days` of retention, the dates a note created at `created_at` may be
/// moved to at `now_ms`, inclusive: never later than it is (the server keeps
/// the stored date) and never past the cutoff. A note with an implausible
/// date may go up to now. An empty range means none. Mirrored by
/// `redateBounds` in `src/lib/orgPolicy.ts`; `src/lib/redateVectors.json`
/// pins both.
pub(crate) fn redate_bounds(days: u32, created_at: i64, now_ms: i64) -> (i64, i64) {
    let min = retention_cutoff(now_ms, days).max(MIN_EPOCH_MS);
    let max = if created_at >= MIN_EPOCH_MS { created_at } else { now_ms };
    (min, max)
}

/// The cutoff for notes in `workspace_id` at `now_ms`, or `None` when no
/// retention covers that workspace.
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) fn workspace_cutoff(conn: &Connection, workspace_id: &str, now_ms: i64) -> Option<i64> {
    retention_rule(conn, workspace_id).map(|r| retention_cutoff(now_ms, r.days))
}

/// The department [`cache_local_only`]'s organisation owns.
#[cfg(test)]
pub(crate) const TEST_ORG_WORKSPACE: &str = "w-org";

/// Cache one organisation with only `local_only` on, owning
/// [`TEST_ORG_WORKSPACE`], for other modules' tests.
#[cfg(test)]
pub(crate) fn cache_local_only(conn: &Connection, org: &str) {
    let body = format!(
        r#"{{"orgs":[{{"id":"o1","name":"{org}","policy":{{"local_only":true,"retention_days":0,"telemetry_off":false,"audio_forbidden":false}},"workspaces":["{TEST_ORG_WORKSPACE}"]}}]}}"#
    );
    store(conn, &classify(200, &body)).unwrap();
}

/// A new note in `workspace`, for other modules' tests.
#[cfg(test)]
pub(crate) fn note_in(conn: &Connection, workspace: &str) -> String {
    let note = crate::db::create_note(conn, "en", "meeting", "").unwrap();
    crate::db::set_note_workspace(conn, &note.id, workspace).unwrap();
    note.id
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
        OrgPolicy { local_only, retention_days, telemetry_off, audio_forbidden, audit_reads: false }
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
    fn retention_scope_takes_the_fewest_days_per_workspace() {
        let config: OrgConfig = serde_json::from_str(
            r#"{"orgs":[
              {"id":"a","name":"A","policy":{"local_only":false,"retention_days":30,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w1","w2",""," "]},
              {"id":"b","name":"B","policy":{"local_only":false,"retention_days":7,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w2"]},
              {"id":"c","name":"C","policy":{"local_only":true,"retention_days":0,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w3"]},
              {"id":" ","name":"D","policy":{"local_only":false,"retention_days":1,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w4"]}
            ]}"#,
        )
        .unwrap();
        let scope = retention_scope(&config);
        let expected: std::collections::BTreeMap<String, u32> = [("w1".to_string(), 30), ("w2".to_string(), 7)].into();
        assert_eq!(scope, expected, "0 days covers nothing; blank ids never");
    }

    #[derive(serde::Deserialize)]
    struct RedateVectors {
        now: i64,
        cases: Vec<RedateCase>,
    }

    #[derive(serde::Deserialize)]
    struct RedateCase {
        days: u32,
        created_at: i64,
        min: i64,
        max: i64,
    }

    /// The vectors `orgPolicy.test.ts` runs against `redateBounds`.
    #[test]
    fn redate_bounds_match_the_shared_vectors() {
        let v: RedateVectors = serde_json::from_str(include_str!("../../src/lib/redateVectors.json")).unwrap();
        assert!(v.cases.len() >= 7);
        for c in v.cases {
            assert_eq!(redate_bounds(c.days, c.created_at, v.now), (c.min, c.max), "{} days, created {}", c.days, c.created_at);
        }
    }

    #[test]
    fn a_rule_names_the_organisation_with_the_fewest_days() {
        let config: OrgConfig = serde_json::from_str(
            r#"{"orgs":[
              {"id":"a","name":"A","policy":{"local_only":false,"retention_days":30,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w1"]},
              {"id":"b","name":"B","policy":{"local_only":false,"retention_days":7,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w1"]},
              {"id":"c","name":"C","policy":{"local_only":false,"retention_days":7,"telemetry_off":false,"audio_forbidden":false},"workspaces":["w1"]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(retention_rules(&config)["w1"], RetentionRule { days: 7, source: "B".into() });
    }

    #[test]
    fn an_implausible_creation_time_is_never_past() {
        let cutoff = 1_800_000_000_000;
        assert!(is_past(MIN_EPOCH_MS, cutoff));
        assert!(!is_past(MIN_EPOCH_MS - 1, cutoff));
        assert!(!is_past(0, cutoff));
        assert!(!is_past(cutoff, cutoff));
        assert!(is_past(cutoff - 1, cutoff));
    }

    #[test]
    fn retention_covers_nothing_without_a_clean_cache() {
        let (_d, conn) = conn();
        assert!(retention_in_force(&conn).is_empty(), "nothing cached");
        assert_eq!(workspace_cutoff(&conn, "w1", 10 * DAY_MS), None);
        store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap();
        assert!(retention_in_force(&conn).is_empty(), "no organisation");
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(retention_in_force(&conn).is_empty(), "unreadable");
        assert_eq!(workspace_cutoff(&conn, "w1", 10 * DAY_MS), None);
        assert_eq!(effective_for_or_strictest(&conn, Some("w1")).retention_days, 0);
    }

    #[test]
    fn retention_reaches_only_the_organisation_workspaces() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        let now = 100 * DAY_MS;
        assert_eq!(workspace_cutoff(&conn, "w1", now), Some(70 * DAY_MS));
        assert_eq!(retention_rule(&conn, "w1"), Some(RetentionRule { days: 30, source: "Northwind".into() }));
        assert_eq!(workspace_cutoff(&conn, "", now), None, "Personal");
        assert_eq!(workspace_cutoff(&conn, "  ", now), None);
        assert_eq!(workspace_cutoff(&conn, "w2", now), None, "a workspace outside the organisation");
    }

    #[test]
    fn the_cutoff_is_days_before_now_and_saturates() {
        assert_eq!(retention_cutoff(100 * DAY_MS, 30), 70 * DAY_MS);
        assert_eq!(retention_cutoff(i64::MIN + 1, u32::MAX), i64::MIN);
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
    fn audit_reads_is_read_when_sent_and_off_when_not() {
        let FetchOutcome::Fetched(config) = classify(200, ONE_ORG) else { panic!() };
        assert!(!config.orgs[0].policy.audit_reads, "a server from before the audit log");
        let body = ONE_ORG.replace("\"audio_forbidden\":false}", "\"audio_forbidden\":false,\"audit_reads\":true}");
        let FetchOutcome::Fetched(config) = classify(200, &body) else { panic!() };
        assert!(config.orgs[0].policy.audit_reads);
        assert!(summaries(Some(&config))[0].audit_reads);
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
        let before = effective_for(&conn, Some("w1")).unwrap();
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
            assert_eq!(effective_for(&conn, Some("w1")).unwrap(), before, "{outcome:?}");
        }
    }

    #[test]
    fn an_authenticated_empty_answer_releases_the_policies() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        assert!(store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap());
        assert_eq!(effective_for(&conn, Some("w1")).unwrap(), EffectivePolicy::default());
    }

    #[test]
    fn signing_out_keeps_the_cache() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        // What `cloud_logout` clears.
        crate::db::delete_settings_with_prefix(&conn, "roster_cache:").unwrap();
        assert!(effective_for(&conn, Some("w1")).unwrap().local_only);
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
        assert_eq!(effective_for(&conn, Some("w1")).unwrap(), EffectivePolicy::default());
    }

    // The `.catch(() => null)` trap: an unreadable cache must not collapse into
    // "no policy" for a caller that enforces.
    #[test]
    fn an_unreadable_cache_is_an_error() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(effective_for(&conn, Some("w1")).is_err());
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

    fn forced(pairs: &[(&str, ManagedValue)]) -> BTreeMap<String, ManagedValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn no_forced_keys_is_no_managed_configuration() {
        assert_eq!(parse_managed(&BTreeMap::new()), Managed::default());
    }

    #[test]
    fn a_profile_becomes_a_layer_under_this_macs_name() {
        let m = parse_managed(&forced(&[
            ("telemetry_off", ManagedValue::Bool(true)),
            ("local_only", ManagedValue::Bool(false)),
            ("policy_reason", ManagedValue::Text("  Set by IT.  ".into())),
            ("server_url", ManagedValue::Text(" https://sync.example.org/ ".into())),
        ]));
        let layer = m.layer.unwrap();
        assert_eq!(layer.source, MANAGED_SOURCE);
        assert_eq!(layer.reason, "Set by IT.");
        assert_eq!(layer.policy, policy(false, 0, true, false));
        assert_eq!(m.server_url.as_deref(), Some("https://sync.example.org"));
    }

    #[test]
    fn a_flag_of_the_wrong_type_is_on() {
        let m = parse_managed(&forced(&[
            ("local_only", ManagedValue::Text("false".into())),
            ("telemetry_off", ManagedValue::Other),
            ("audio_forbidden", ManagedValue::Bool(false)),
        ]));
        assert_eq!(m.layer.unwrap().policy, policy(true, 0, true, false));
    }

    #[test]
    fn a_bad_server_url_or_reason_is_ignored() {
        for bad in [ManagedValue::Bool(true), ManagedValue::Other, ManagedValue::Text("sync.example.org".into()),
            ManagedValue::Text("ftp://sync.example.org".into()), ManagedValue::Text("".into())]
        {
            let m = parse_managed(&forced(&[("server_url", bad.clone()), ("policy_reason", ManagedValue::Other)]));
            assert_eq!(m, Managed::default(), "{bad:?}");
        }
        let m = parse_managed(&forced(&[("audio_forbidden", ManagedValue::Bool(true)), ("policy_reason", ManagedValue::Bool(true))]));
        assert_eq!(m.layer.unwrap().reason, "");
    }

    #[test]
    fn a_server_url_alone_sets_no_policy() {
        let m = parse_managed(&forced(&[("server_url", ManagedValue::Text("http://localhost:8090".into()))]));
        assert_eq!(m.layer, None);
        assert_eq!(m.server_url.as_deref(), Some("http://localhost:8090"));
    }

    fn managed_layer(p: OrgPolicy) -> Option<Layer> {
        Some(Layer { source: MANAGED_SOURCE.into(), reason: String::new(), policy: p })
    }

    #[test]
    fn a_managed_profile_applies_with_nothing_cached() {
        let (_d, conn) = conn();
        let p = effective_for_with(&conn, Some("w1"), managed_layer(policy(false, 0, true, false))).unwrap();
        assert!(p.telemetry_off && !p.local_only && !p.audio_forbidden);
        assert_eq!(source(&p, PolicyKind::TelemetryOff), MANAGED_SOURCE);
        assert_eq!(p.org_name.as_deref(), Some(MANAGED_SOURCE));
    }

    #[test]
    fn a_managed_profile_merges_strictest_wins_with_the_server() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        let p = effective_for_with(&conn, Some("w1"), managed_layer(policy(true, 0, false, true))).unwrap();
        assert!(p.local_only && p.telemetry_off && p.audio_forbidden);
        assert_eq!(p.retention_days, 30);
        assert_eq!(source(&p, PolicyKind::LocalOnly), "Northwind", "the organisation set it first");
        assert_eq!(source(&p, PolicyKind::TelemetryOff), "Northwind");
        assert_eq!(source(&p, PolicyKind::AudioForbidden), MANAGED_SOURCE);
        assert_eq!(source(&p, PolicyKind::RetentionDays), "Northwind");
    }

    #[test]
    fn a_released_organisation_leaves_the_managed_profile_in_force() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        assert!(store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap());
        let p = effective_for_with(&conn, Some("w1"), managed_layer(policy(true, 0, false, false))).unwrap();
        assert!(p.local_only);
        assert_eq!(p.local_only_source(), Some(MANAGED_SOURCE));
        assert!(p.blocked().to_string().contains(MANAGED_SOURCE));
    }

    #[test]
    fn tests_never_read_this_macs_profile() {
        assert_eq!(managed(), Managed::default());
    }

    /// Run by hand: a value the user writes with `defaults write` is not forced.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn a_users_own_default_is_not_managed() {
        use core_foundation::base::TCFType;
        use core_foundation::boolean::CFBoolean;
        use core_foundation::string::CFString;
        use core_foundation_sys::preferences::{CFPreferencesAppSynchronize, CFPreferencesSetAppValue};
        let domain = "no.humla.app.managed-test";
        let app = CFString::new(domain);
        let key = CFString::new("telemetry_off");
        unsafe {
            CFPreferencesSetAppValue(key.as_concrete_TypeRef(), CFBoolean::true_value().as_CFTypeRef(), app.as_concrete_TypeRef());
            CFPreferencesAppSynchronize(app.as_concrete_TypeRef());
        }
        let visible = unsafe {
            let raw = core_foundation_sys::preferences::CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), app.as_concrete_TypeRef());
            let seen = !raw.is_null();
            if seen {
                core_foundation_sys::base::CFRelease(raw);
            }
            seen
        };
        let read = read_forced_in(domain);
        unsafe {
            CFPreferencesSetAppValue(key.as_concrete_TypeRef(), std::ptr::null(), app.as_concrete_TypeRef());
            CFPreferencesAppSynchronize(app.as_concrete_TypeRef());
        }
        assert!(visible, "the user's value is there to read");
        assert!(read.is_empty(), "{read:?}");
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
        let err = check_endpoint(&conn, Some("w1"), ProviderId::OpenAi, None).unwrap_err().to_string();
        assert_eq!(err, "Blocked by Northwind: processing stays on this Mac.");
        assert!(check_endpoint(&conn, Some("w1"), ProviderId::Local, None).is_ok());
        assert_eq!(check_leaves_mac(&conn, Some("w1")).unwrap_err().to_string(), err);
    }

    #[test]
    fn an_unreadable_cache_reads_as_local_only() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(check_endpoint(&conn, Some("w1"), ProviderId::Local, None).is_ok(), "on-device Whisper");
        assert!(check_endpoint(&conn, Some("w1"), ProviderId::Local, Some("http://localhost:11434/v1")).is_ok());
        assert!(check_endpoint(&conn, Some("w1"), ProviderId::Custom, Some("http://127.0.0.1:1234/v1")).is_ok());
        for spec in REGISTRY {
            if !matches!(spec.id, ProviderId::Local | ProviderId::Custom) {
                let err = check_endpoint(&conn, Some("w1"), spec.id, None).unwrap_err();
                assert_eq!(err.to_string(), "Blocked by your organisation: processing stays on this Mac.");
            }
        }
        assert!(check_endpoint(&conn, Some("w1"), ProviderId::Custom, Some("https://api.example.com")).is_err());
        assert!(check_leaves_mac(&conn, Some("w1")).is_err());
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
        assert!(vocabulary_for(&conn, Some("w1")).is_empty());
        assert!(!apply_templates(&conn, &OrgConfig::default()).unwrap());
        store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap();
        assert!(vocabulary_for(&conn, Some("w1")).is_empty());
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
    fn terms_come_only_from_the_organisation_owning_the_department() {
        let (_d, conn) = conn();
        cache_one_org(&conn);
        assert_eq!(vocabulary_for(&conn, Some("w1")), ["Northwind"]);
        for ws in [None, Some(""), Some("w2")] {
            assert!(vocabulary_for(&conn, ws).is_empty(), "{ws:?}");
        }
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(vocabulary_for(&conn, Some("w1")).is_empty());
    }

    #[test]
    fn only_a_clean_cache_forbids_audio_for_a_move() {
        let (_d, conn) = conn();
        assert!(store(&conn, &classify(200, TWO_ORGS)).unwrap());
        assert!(forbids_audio(&conn, "wa"));
        assert!(!forbids_audio(&conn, "wb") && !forbids_audio(&conn, ""));
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(!forbids_audio(&conn, "wa"));
    }

    #[test]
    fn no_policy_refuses_nothing() {
        let (_d, conn) = conn();
        assert!(check_endpoint(&conn, Some("w1"), ProviderId::OpenAi, None).is_ok());
        assert!(check_leaves_mac(&conn, Some("w1")).is_ok());
    }
    fn record_due(conn: &Connection, org_id: &str) {
        let due = audio_sweep_due(conn).unwrap();
        assert_eq!(due.org_id, org_id);
        record_audio_sweep(conn, &due).unwrap();
    }

    fn cache_audio(conn: &Connection, id: &str, name: &str, forbidden: bool) {
        let body = format!(
            r#"{{"orgs":[{{"id":"{id}","name":"{name}","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":{forbidden}}},"workspaces":["w1"]}}]}}"#
        );
        assert!(matches!(classify(200, &body), FetchOutcome::Fetched(_)), "{body}");
        store(conn, &classify(200, &body)).unwrap();
    }

    #[test]
    fn no_organisation_owes_no_sweep_and_forbids_nothing() {
        let (_d, conn) = conn();
        assert_eq!(audio_sweep_due(&conn), None, "nothing cached");
        let p = effective_for_or_strictest(&conn, Some("w1"));
        assert!(!p.audio_forbidden && !p.telemetry_off);
        store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap();
        assert_eq!(audio_sweep_due(&conn), None, "an empty answer");
        let p = effective_for_or_strictest(&conn, Some("w1"));
        assert!(!p.audio_forbidden && !p.telemetry_off);
        cache_audio(&conn, "o1", "Northwind", false);
        assert_eq!(audio_sweep_due(&conn), None, "an organisation that allows audio");
    }

    #[test]
    fn an_unreadable_cache_closes_the_audio_gate_but_never_sweeps() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        let p = effective_for_or_strictest(&conn, Some("w1"));
        assert!(p.audio_forbidden && p.telemetry_off);
        assert_eq!(p.retention_days, 0, "an unreadable policy deletes no notes");
        assert_eq!(audio_sweep_due(&conn), None);
        let garbled = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"audio_forbidden":true}}]}"#;
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, garbled).unwrap();
        assert!(effective_for_or_strictest(&conn, Some("w1")).audio_forbidden);
        assert_eq!(audio_sweep_due(&conn), None, "a policy missing fields is unreadable");
    }

    #[test]
    fn audio_forbidden_from_a_named_organisation_owes_one_sweep() {
        let (_d, conn) = conn();
        cache_audio(&conn, "o1", "Northwind", true);
        let due = audio_sweep_due(&conn).unwrap();
        assert_eq!((due.org_id.as_str(), due.org_name.as_str()), ("o1", "Northwind"));
        record_due(&conn, "o1");
        record_audio_sweep(&conn, &due).unwrap();
        assert_eq!(audio_sweep_due(&conn), None);
        assert_eq!(crate::db::get_setting(&conn, SETTING_AUDIO_SWEPT).unwrap().as_deref(), Some(r#"["o1/w1"]"#));
        assert!(effective_for_or_strictest(&conn, Some("w1")).audio_forbidden, "the gate stays closed after the sweep");
    }

    #[test]
    fn a_second_organisation_owes_its_own_sweep() {
        let (_d, conn) = conn();
        cache_audio(&conn, "o1", "Northwind", true);
        record_due(&conn, "o1");
        cache_audio(&conn, "o2", "Fjordby", true);
        assert_eq!(audio_sweep_due(&conn).map(|d| d.org_id), Some("o2".into()));
    }

    #[test]
    fn an_organisation_without_an_id_or_name_never_sweeps() {
        let (_d, conn) = conn();
        cache_audio(&conn, "o1", " ", true);
        assert_eq!(audio_sweep_due(&conn), None);
        cache_audio(&conn, "", "Northwind", true);
        assert_eq!(audio_sweep_due(&conn), None);
        assert!(effective_for_or_strictest(&conn, Some("w1")).audio_forbidden);
    }

    #[test]
    fn a_profile_forcing_audio_forbidden_owes_one_sweep() {
        let (_d, conn) = conn();
        force_for_test(&[("audio_forbidden", ManagedValue::Bool(true))]);
        reload_managed();
        let due = audio_sweep_due(&conn).unwrap();
        assert_eq!(
            due,
            AudioSweep { org_id: MANAGED_SWEEP_ID.into(), org_name: MANAGED_SOURCE.into(), all: true, workspaces: vec![] }
        );
        assert_eq!(sweep_scope(&due), SweepScope::Everything);
        record_due(&conn, MANAGED_SWEEP_ID);
        assert_eq!(audio_sweep_due(&conn), None, "once only");
    }

    #[test]
    fn an_organisation_sweeps_before_the_profile_and_both_once() {
        let (_d, conn) = conn();
        cache_audio(&conn, "o1", "Northwind", true);
        force_for_test(&[("audio_forbidden", ManagedValue::Bool(true))]);
        reload_managed();
        assert_eq!(audio_sweep_due(&conn).unwrap().org_id, "o1");
        record_due(&conn, "o1");
        assert_eq!(audio_sweep_due(&conn).unwrap().org_id, MANAGED_SWEEP_ID);
        record_due(&conn, MANAGED_SWEEP_ID);
        assert_eq!(audio_sweep_due(&conn), None);
    }

    #[test]
    fn a_profile_sweeps_only_when_it_forces_audio_forbidden_true() {
        let (_d, conn) = conn();
        for pairs in [
            vec![],
            vec![("audio_forbidden", ManagedValue::Bool(false))],
            vec![("local_only", ManagedValue::Bool(true))],
            vec![("audio_forbidden", ManagedValue::Text("true".into()))],
        ] {
            force_for_test(&pairs);
            reload_managed();
            assert_eq!(audio_sweep_due(&conn), None, "{pairs:?}");
        }
        assert!(effective_for_or_strictest(&conn, None).audio_forbidden, "a mistyped value still closes the gate, Personal too");
    }

    #[test]
    fn a_profile_still_sweeps_beside_an_unreadable_cache() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        force_for_test(&[("audio_forbidden", ManagedValue::Bool(true))]);
        reload_managed();
        assert_eq!(audio_sweep_due(&conn).unwrap().org_id, MANAGED_SWEEP_ID);
    }

    const TWO_ORGS: &str = r#"{"orgs":[
        {"id":"a","name":"Humla","policy_reason":"Nothing leaves this Mac.",
         "policy":{"local_only":true,"retention_days":30,"telemetry_off":false,"audio_forbidden":true},"workspaces":["wa"]},
        {"id":"b","name":"Screenpartner",
         "policy":{"local_only":false,"retention_days":0,"telemetry_off":true,"audio_forbidden":false},"workspaces":["wb"]}]}"#;

    #[test]
    fn an_organisations_policies_govern_only_its_own_departments() {
        let (_d, conn) = conn();
        assert!(store(&conn, &classify(200, TWO_ORGS)).unwrap());
        let a = effective_for(&conn, Some("wa")).unwrap();
        assert!(a.local_only && a.audio_forbidden && !a.telemetry_off);
        assert_eq!(a.retention_days, 30);
        assert_eq!(a.org_name.as_deref(), Some("Humla"));
        assert_eq!(effective_for(&conn, Some(" wa ")).unwrap(), a);
        for ws in [Some("wb"), None, Some(""), Some("  "), Some("w-none")] {
            let p = effective_for(&conn, ws).unwrap();
            assert!(!p.local_only && !p.audio_forbidden && p.retention_days == 0, "{ws:?}");
        }
        assert!(effective_for(&conn, Some("wb")).unwrap().telemetry_off, "B's own department");
        assert!(!effective_for(&conn, None).unwrap().telemetry_off);
        let d = effective_device(&conn).unwrap();
        assert!(d.telemetry_off && !d.local_only && !d.audio_forbidden && d.retention_days == 0);
        assert_eq!(source(&d, PolicyKind::TelemetryOff), "Screenpartner");
    }

    #[test]
    fn a_managed_profile_governs_every_workspace() {
        let (_d, conn) = conn();
        assert!(store(&conn, &classify(200, TWO_ORGS)).unwrap());
        force_for_test(&[("local_only", ManagedValue::Bool(true)), ("telemetry_off", ManagedValue::Bool(true))]);
        reload_managed();
        for ws in [Some("wa"), Some("wb"), None, Some("w-none")] {
            assert!(effective_for(&conn, ws).unwrap().local_only, "{ws:?}");
        }
        assert_eq!(effective_for(&conn, None).unwrap().local_only_source(), Some(MANAGED_SOURCE));
        assert_eq!(effective_for(&conn, Some("wa")).unwrap().local_only_source(), Some("Humla"));
        assert!(effective_managed().local_only && effective_device(&conn).unwrap().local_only);
        force_for_test(&[]);
        reload_managed();
        assert_eq!(effective_managed(), EffectivePolicy::default());
    }

    #[test]
    fn an_unreadable_cache_is_strictest_only_for_a_department() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(effective_for(&conn, Some("wa")).is_err());
        let p = effective_for_or_strictest(&conn, Some("wa"));
        assert!(p.local_only && p.audio_forbidden && p.retention_days == 0);
        for ws in [None, Some(""), Some(" ")] {
            assert_eq!(effective_for_or_strictest(&conn, ws), EffectivePolicy::default(), "{ws:?}");
        }
        assert!(effective_device(&conn).is_err());
        let d = effective_device_or_strictest(&conn);
        assert!(d.telemetry_off && !d.local_only && !d.audio_forbidden);
    }

    #[test]
    fn a_note_is_held_to_its_own_departments_policy() {
        let (_d, conn) = conn();
        assert!(store(&conn, &classify(200, TWO_ORGS)).unwrap());
        assert!(effective_for_note(&conn, &note_in(&conn, "wa")).audio_forbidden);
        assert!(!effective_for_note(&conn, &note_in(&conn, "wb")).audio_forbidden);
        assert!(!effective_for_note(&conn, &note_in(&conn, "")).audio_forbidden);
        assert!(effective_for_note(&conn, "missing").audio_forbidden, "a note that can't be read");
    }

    #[test]
    fn the_ui_sees_each_organisations_rules_and_departments() {
        let config: OrgConfig = serde_json::from_str(TWO_ORGS).unwrap();
        let governed = governed_workspaces(Some(&config));
        assert_eq!(governed.into_iter().collect::<Vec<_>>(), [("wa".to_string(), "Humla".to_string())]);
        let per_org = org_policies(Some(&config));
        assert_eq!(per_org.len(), 2);
        assert_eq!(per_org[0].workspaces, ["wa"]);
        assert!(per_org[0].policy.local_only && !per_org[0].policy.telemetry_off);
        assert!(per_org[1].policy.telemetry_off && !per_org[1].policy.local_only);
        assert!(org_policies(None).is_empty() && governed_workspaces(None).is_empty());
    }

    #[test]
    fn an_organisations_sweep_covers_only_its_departments() {
        let (_d, conn) = conn();
        assert!(store(&conn, &classify(200, TWO_ORGS)).unwrap());
        let due = audio_sweep_due(&conn).unwrap();
        assert_eq!(due, AudioSweep { org_id: "a".into(), org_name: "Humla".into(), all: false, workspaces: vec!["wa".into()] });
        let scope = sweep_scope(&due);
        assert!(scope.covers("wa"));
        assert!(!scope.covers("wb") && !scope.covers("") && !scope.covers(" "));
        let without_departments = TWO_ORGS.replace(r#""workspaces":["wa"]"#, r#""workspaces":[]"#);
        assert!(store(&conn, &classify(200, &without_departments)).unwrap());
        assert_eq!(audio_sweep_due(&conn), None, "nothing of its own to sweep");
    }

    #[test]
    fn a_department_attached_or_unlocked_later_owes_its_own_sweep() {
        let (_d, conn) = conn();
        cache_audio(&conn, "o1", "Northwind", true);
        record_due(&conn, "o1");
        let body = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":true},"workspaces":["w1","w2"],"locked_workspaces":["w3"]}]}"#;
        store(&conn, &classify(200, body)).unwrap();
        let due = audio_sweep_due(&conn).unwrap();
        assert_eq!(due.workspaces, ["w2", "w3"], "only the departments not swept yet");
        assert!(!sweep_scope(&due).covers("w1"));
        record_audio_sweep(&conn, &due).unwrap();
        assert_eq!(audio_sweep_due(&conn), None);
    }

    #[test]
    fn a_sweep_recorded_per_organisation_covers_the_departments_it_had() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, SETTING_AUDIO_SWEPT, r#"["o1","mac-configuration","gone"]"#).unwrap();
        cache_audio(&conn, "o1", "Northwind", true);
        assert_eq!(audio_sweep_due(&conn), None);
        assert_eq!(
            crate::db::get_setting(&conn, SETTING_AUDIO_SWEPT).unwrap().as_deref(),
            Some(r#"["o1/w1","mac-configuration"]"#)
        );
        let body = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":true},"workspaces":["w1","w2"]}]}"#;
        store(&conn, &classify(200, body)).unwrap();
        assert_eq!(audio_sweep_due(&conn).unwrap().workspaces, ["w2"], "attached after the upgrade");
    }

    #[test]
    fn a_department_behind_unmet_2fa_keeps_its_organisations_policy() {
        let (_d, conn) = conn();
        let body = r#"{"orgs":[{"id":"a","name":"Humla","policy":{"local_only":true,"retention_days":30,"telemetry_off":false,"audio_forbidden":true},"workspaces":[],"locked_workspaces":["wl"]}]}"#;
        store(&conn, &classify(200, body)).unwrap();
        let p = effective_for(&conn, Some("wl")).unwrap();
        assert!(p.local_only && p.audio_forbidden && p.retention_days == 30);
        assert!(!effective_for(&conn, Some("w-other")).unwrap().local_only, "a claimed org governs only its own");
        assert!(!effective_for(&conn, None).unwrap().local_only);
        let config = cached(&conn).unwrap().unwrap();
        assert_eq!(retention_rules(&config)["wl"].days, 30);
        assert_eq!(governed_workspaces(Some(&config))["wl"], "Humla");
        assert_eq!(org_policies(Some(&config))[0].workspaces, ["wl"]);
        assert!(forbids_audio(&conn, "wl") && governs_workspace(&conn, "wl"));
        assert_eq!(audio_sweep_due(&conn).unwrap().workspaces, ["wl"]);
        assert!(vocabulary_for(&conn, Some("wl")).is_empty());
    }

    #[test]
    fn an_older_server_naming_no_department_governs_every_unclaimed_workspace() {
        let (_d, conn) = conn();
        let body = r#"{"orgs":[
            {"id":"a","name":"Humla","policy":{"local_only":true,"retention_days":0,"telemetry_off":false,"audio_forbidden":true},"workspaces":[]},
            {"id":"b","name":"Screenpartner","policy":{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false},"workspaces":["wb"]}]}"#;
        store(&conn, &classify(200, body)).unwrap();
        assert!(effective_for(&conn, Some("w-unknown")).unwrap().local_only);
        assert!(!effective_for(&conn, Some("wb")).unwrap().local_only, "claimed by another organisation");
        assert!(!effective_for(&conn, None).unwrap().local_only, "Personal is never affected");
        assert!(governs_workspace(&conn, "w-unknown") && !governs_workspace(&conn, "wb"));
        assert!(!forbids_audio(&conn, "w-unknown"), "a guess deletes nothing");
        assert_eq!(audio_sweep_due(&conn), None);
    }

    #[test]
    fn a_note_that_cant_be_read_is_strictest_only_with_an_organisation_cached() {
        let (_d, conn) = conn();
        assert_eq!(effective_for_note(&conn, "missing"), EffectivePolicy::default());
        store(&conn, &classify(200, r#"{"orgs":[]}"#)).unwrap();
        assert_eq!(effective_for_note(&conn, "missing"), EffectivePolicy::default());
        cache_one_org(&conn);
        assert!(effective_for_note(&conn, "missing").local_only);
    }

    #[test]
    fn a_reload_sees_a_profile_installed_since_the_last_read() {
        assert_eq!(managed(), Managed::default());
        force_for_test(&[("local_only", ManagedValue::Bool(true)), ("server_url", ManagedValue::Text("https://s.example".into()))]);
        assert_eq!(managed().layer, None, "cached until re-read");
        assert_eq!(reload_managed(), ManagedChange { policy: true, server_url: true });
        assert!(managed().layer.unwrap().policy.local_only);
        assert_eq!(reload_managed(), ManagedChange::default(), "nothing new");
        force_for_test(&[]);
        assert_eq!(reload_managed(), ManagedChange { policy: true, server_url: true });
        assert_eq!(managed(), Managed::default());
    }
}
