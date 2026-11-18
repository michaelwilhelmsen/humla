//! An organisation admin editing its templates (`org_templates`) and terms
//! (`organisations.vocabulary`) on the server. Nothing is written locally and
//! nothing is queued: each write that lands ends with a refresh of the
//! organisation config, which is what brings the change to this Mac.

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::org_policy;
use crate::AppState;

const ORGS_PATH: &str = "/api/collections/organisations/records";
const TEMPLATES_PATH: &str = "/api/collections/org_templates/records";

/// The most terms an organisation keeps, as for the user's own.
pub(crate) const VOCABULARY_LIMIT: usize = 100;
pub(crate) const MIN_TERM_LENGTH: usize = 3;
const NAME_MAX: usize = 200;

/// An organisation the signed-in user administers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AdminOrg {
    pub id: String,
    pub name: String,
    pub require_2fa: bool,
}

/// A template as its editor opens it. `workspaces` are the departments the
/// user is in; `hidden` counts the others it is offered to, which a save keeps.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrgTemplateDraft {
    pub name: String,
    pub content: String,
    pub workspaces: Vec<String>,
    pub hidden: usize,
}

/// An organisation the admin is in no department of, read straight from the
/// collections: only for editing, never cached or applied on this Mac.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AdminContent {
    pub templates: Vec<AdminTemplate>,
    pub vocabulary: Vec<String>,
}

/// `workspaces` are the departments a template is offered in, every one of
/// them, which an admin may list; empty is all.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AdminTemplate {
    pub client_id: String,
    pub name: String,
    pub content: String,
    pub workspaces: Vec<String>,
}

/// The stored record behind a template.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoredTemplate {
    pub record_id: String,
    pub name: String,
    pub content: String,
    pub workspaces: Vec<String>,
    pub client_updated_at: i64,
    pub deleted: bool,
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|s| s.as_str()).unwrap_or_default().to_string()
}

fn number(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|n| n.as_f64()).map(|n| n as i64).unwrap_or(0)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(|b| b.as_bool()).unwrap_or(false)
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str()).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// The organisations listing: the server lists only those the caller
/// administers.
pub(crate) fn parse_admin_orgs(body: &Value) -> Result<Vec<AdminOrg>, String> {
    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or("unreadable list of organisations")?;
    Ok(items
        .iter()
        .map(|o| AdminOrg {
            id: str_field(o, "id"),
            name: str_field(o, "name"),
            require_2fa: o.get("require_2fa").and_then(|b| b.as_bool()).unwrap_or(false),
        })
        .filter(|o| safe_id(&o.id))
        .collect())
}

/// An organisation record's terms. A field in any shape but a list of strings
/// (or empty) is refused, so an edit never overwrites what it couldn't read.
pub(crate) fn parse_vocabulary(record: &Value) -> Result<Vec<String>, String> {
    match record.get("vocabulary") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|t| t.as_str().map(String::from).ok_or_else(|| "unreadable terms".to_string()))
            .collect(),
        Some(_) => Err("unreadable terms".into()),
    }
}

/// `current` with `remove` taken out and `add` appended, trimmed and without
/// duplicates regardless of case. Terms too short to use are dropped.
pub(crate) fn edit_terms(current: &[String], add: &[String], remove: &[String]) -> Result<Vec<String>, String> {
    let removed: Vec<String> = remove.iter().map(|t| t.trim().to_lowercase()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let kept = current.iter().map(|t| (t, false));
    let added = add.iter().map(|t| (t, true));
    for (raw, is_new) in kept.chain(added) {
        let term = raw.trim();
        let key = term.to_lowercase();
        if term.is_empty() || removed.contains(&key) || !seen.insert(key) {
            continue;
        }
        if is_new && term.chars().count() < MIN_TERM_LENGTH {
            continue;
        }
        out.push(term.to_string());
    }
    if out.len() > VOCABULARY_LIMIT && out.len() > current.len() {
        return Err(format!("An organisation can have at most {VOCABULARY_LIMIT} terms."));
    }
    Ok(out)
}

/// The organisation PATCH: its terms and nothing else.
pub(crate) fn vocabulary_body(terms: &[String]) -> Value {
    json!({ "vocabulary": terms })
}

/// PocketBase record ids and template client ids: safe inside a filter.
pub(crate) fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub(crate) fn template_filter(org_id: &str, client_id: &str) -> Result<String, String> {
    if !safe_id(org_id) || !safe_id(client_id) {
        return Err("Not an organisation template.".into());
    }
    Ok(format!("organisation='{org_id}' && client_id='{client_id}'"))
}

/// The one record a [`template_filter`] listing found, or `None`.
pub(crate) fn parse_template_lookup(body: &Value) -> Result<Option<StoredTemplate>, String> {
    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or("unreadable organisation template")?;
    Ok(items.first().map(|t| StoredTemplate {
        record_id: str_field(t, "id"),
        name: str_field(t, "name"),
        content: str_field(t, "content"),
        workspaces: ids(t.get("workspaces").unwrap_or(&Value::Null)),
        client_updated_at: number(t, "client_updated_at"),
        deleted: flag(t, "deleted"),
    }))
}

pub(crate) fn templates_filter(org_id: &str) -> Result<String, String> {
    if !safe_id(org_id) {
        return Err("Not an organisation.".into());
    }
    Ok(format!("organisation='{org_id}' && deleted!=true"))
}

/// An organisation's live templates as an admin lists them.
pub(crate) fn parse_admin_templates(body: &Value) -> Result<Vec<AdminTemplate>, String> {
    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or("unreadable organisation templates")?;
    Ok(items
        .iter()
        .filter(|t| !t.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false))
        .map(|t| AdminTemplate {
            client_id: str_field(t, "client_id"),
            name: str_field(t, "name"),
            content: org_policy::plain_text(&str_field(t, "content")),
            workspaces: ids(t.get("workspaces").unwrap_or(&Value::Null)),
        })
        .filter(|t| safe_id(&t.client_id))
        .collect())
}

/// What the editor shows of `stored`, given the departments the user is in.
pub(crate) fn draft(stored: &StoredTemplate, mine: &[String]) -> OrgTemplateDraft {
    let (visible, hidden): (Vec<_>, Vec<_>) = stored.workspaces.iter().cloned().partition(|w| mine.contains(w));
    OrgTemplateDraft {
        name: stored.name.clone(),
        content: org_policy::plain_text(&stored.content),
        workspaces: visible,
        hidden: hidden.len(),
    }
}

/// The departments a save writes. `chosen` is `None` for every department
/// (empty); otherwise it must be departments the user is in, and every stored
/// department they aren't in is kept.
pub(crate) fn scope_workspaces(
    stored: &[String],
    mine: &[String],
    chosen: Option<&[String]>,
) -> Result<Vec<String>, String> {
    let Some(chosen) = chosen else { return Ok(Vec::new()) };
    if let Some(w) = chosen.iter().find(|w| !mine.contains(w)) {
        return Err(format!("You can only choose departments you're in ({w} isn't one)."));
    }
    let mut out: Vec<String> = stored.iter().filter(|w| !mine.contains(w)).cloned().collect();
    for w in mine.iter().filter(|w| chosen.contains(w)) {
        if !out.contains(w) {
            out.push(w.clone());
        }
    }
    if out.is_empty() {
        return Err("Choose at least one department.".into());
    }
    Ok(out)
}

/// Plain text as the editor field's HTML, one paragraph per line, so
/// `org_policy::plain_text` reads it back unchanged.
pub(crate) fn editor_html(text: &str) -> String {
    text.trim()
        .lines()
        .map(|line| {
            let escaped = line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            format!("<p>{escaped}</p>")
        })
        .collect()
}

fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the template a name.".into());
    }
    if name.chars().count() > NAME_MAX {
        return Err(format!("A template name can be at most {NAME_MAX} characters."));
    }
    Ok(name.to_string())
}

pub(crate) fn create_body(
    org_id: &str,
    client_id: &str,
    name: &str,
    content: &str,
    workspaces: &[String],
    now_ms: i64,
) -> Value {
    json!({
        "organisation": org_id,
        "client_id": client_id,
        "name": name,
        "content": editor_html(content),
        "workspaces": workspaces,
        "client_updated_at": now_ms,
        "deleted": false,
    })
}

/// The stamp a write carries: never older than the stored one, so this Mac's
/// clock running behind can't make the server keep the stored fields.
pub(crate) fn next_stamp(now_ms: i64, stored: i64) -> i64 {
    now_ms.max(stored.saturating_add(1))
}

/// The PATCH for saving `stored`. A tombstoned template is refused rather than
/// brought back.
pub(crate) fn update_plan(
    stored: &StoredTemplate,
    mine: &[String],
    chosen: Option<&[String]>,
    name: &str,
    content: &str,
    now_ms: i64,
) -> Result<Value, String> {
    if stored.deleted {
        return Err(DELETED.into());
    }
    let scope = scope_workspaces(&stored.workspaces, mine, chosen)?;
    Ok(update_body(name, content, &scope, next_stamp(now_ms, stored.client_updated_at)))
}

pub(crate) const DELETED: &str = "This template was deleted.";
pub(crate) const OVERTAKEN: &str =
    "Someone else changed this template at the same time, so your change wasn't saved. Open it again to see theirs.";

/// Whether the record the server answered with holds what was sent. The
/// last-write-wins hook answers a stale write with 200 and the stored fields.
pub(crate) fn write_landed(sent: &Value, returned: &Value) -> bool {
    let same_stamp = number(sent, "client_updated_at") == number(returned, "client_updated_at");
    let same_deleted = flag(sent, "deleted") == flag(returned, "deleted");
    let same_name = sent.get("name").is_none_or(|n| returned.get("name") == Some(n));
    let same_content = sent.get("content").is_none_or(|_| {
        org_policy::plain_text(&str_field(sent, "content")) == org_policy::plain_text(&str_field(returned, "content"))
    });
    same_stamp && same_deleted && same_name && same_content
}

pub(crate) fn update_body(name: &str, content: &str, workspaces: &[String], now_ms: i64) -> Value {
    json!({
        "name": name,
        "content": editor_html(content),
        "workspaces": workspaces,
        "client_updated_at": now_ms,
        "deleted": false,
    })
}

/// A delete is a tombstone, so members' Macs drop the template on their next
/// fetch of the config.
pub(crate) fn tombstone_body(now_ms: i64) -> Value {
    json!({ "deleted": true, "client_updated_at": now_ms })
}

/// A refused request in plain words.
pub(crate) fn refusal(status: u16, body: &Value, org: &str) -> String {
    if str_field(body, "reason") == "two_factor_required" {
        return format!("{org} requires two-factor authentication. Set it up in Settings → Account.");
    }
    match status {
        401 => "Your session has ended. Sign in again.".into(),
        403 | 404 => format!("Only {org}'s admins can change this."),
        400 => {
            let message = str_field(body, "message");
            if message.is_empty() || message.starts_with("Failed to") {
                format!("{org} didn't accept the change. Check the name and try again.")
            } else {
                message
            }
        }
        _ => format!("Couldn't save to {org} ({status}). Try again."),
    }
}

fn not_found(org: &str) -> String {
    format!("This template is no longer on the server, or you're no longer an admin of {org}.")
}

// ---- requests -----------------------------------------------------------------

enum Req<'a> {
    Get(&'a [(&'a str, &'a str)]),
    Post(&'a Value),
    Patch(&'a Value),
}

struct Answer {
    base: String,
    token: String,
    status: u16,
    body: Value,
}

/// One request with the session; a 401 renews the session and tries once more.
async fn send(state: &State<'_, AppState>, path: &str, req: Req<'_>) -> Result<Answer, String> {
    let mut retried = false;
    loop {
        let (base, token) = super::cloud_auth::cloud_session(state).await?;
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let http = crate::http::client();
        let builder = match req {
            Req::Get(q) => http.get(url).query(q),
            Req::Post(b) => http.post(url).json(b),
            Req::Patch(b) => http.patch(url).json(b),
        };
        let resp = builder
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| "Couldn't reach the server. Try again when you're online.".to_string())?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or_default();
        if status == 401 && !retried {
            super::cloud_auth::forget_session_token(&token);
            retried = true;
            continue;
        }
        return Ok(Answer { base, token, status, body });
    }
}

fn ok(answer: &Answer, org: &str) -> Result<(), String> {
    if (200..300).contains(&answer.status) {
        Ok(())
    } else {
        Err(refusal(answer.status, &answer.body, org))
    }
}

/// The organisation's name and the departments of it the user is in, from the
/// cached config.
fn cached_org(state: &State<'_, AppState>, org_id: &str) -> (String, Vec<String>) {
    let config = org_policy::cached(&state.db.lock()).ok().flatten();
    config
        .and_then(|c| c.orgs.into_iter().find(|o| o.id == org_id))
        .map(|o| (o.name, o.workspaces))
        .unwrap_or_else(|| ("your organisation".into(), Vec::new()))
}

async fn lookup(state: &State<'_, AppState>, org_id: &str, client_id: &str, org: &str) -> Result<StoredTemplate, String> {
    let filter = template_filter(org_id, client_id)?;
    let answer = send(state, TEMPLATES_PATH, Req::Get(&[("filter", filter.as_str()), ("perPage", "1")])).await?;
    ok(&answer, org)?;
    parse_template_lookup(&answer.body)?.ok_or_else(|| not_found(org))
}

async fn refresh(app: &tauri::AppHandle, state: &State<'_, AppState>, answer: &Answer) {
    super::org::refresh_org_config(&state.db, app, &answer.base, &answer.token).await;
}

/// [`refresh`] without an answer in hand.
async fn refresh_config(app: &tauri::AppHandle, state: &State<'_, AppState>) {
    if let Ok((base, token)) = super::cloud_auth::cloud_session(state).await {
        super::org::refresh_org_config(&state.db, app, &base, &token).await;
    }
}

// ---- commands -----------------------------------------------------------------

/// The organisations the signed-in user administers.
#[tauri::command]
pub async fn org_admin_orgs(state: State<'_, AppState>) -> Result<Vec<AdminOrg>, String> {
    let query = [("fields", "id,name,require_2fa"), ("perPage", "200")];
    let answer = send(&state, ORGS_PATH, Req::Get(&query)).await?;
    ok(&answer, "Your organisation")?;
    parse_admin_orgs(&answer.body)
}

/// The templates and terms of an organisation the user administers, for an
/// admin who is in none of its departments. Nothing here is written locally.
#[tauri::command]
pub async fn org_admin_content(state: State<'_, AppState>, org_id: String) -> Result<AdminContent, String> {
    let filter = templates_filter(&org_id)?;
    let (org, _) = cached_org(&state, &org_id);
    let query = [
        ("filter", filter.as_str()),
        ("fields", "client_id,name,content,workspaces,deleted"),
        ("sort", "name"),
        ("perPage", "500"),
    ];
    let templates = send(&state, TEMPLATES_PATH, Req::Get(&query)).await?;
    ok(&templates, &org)?;
    let record = send(&state, &format!("{ORGS_PATH}/{org_id}"), Req::Get(&[("fields", "vocabulary")])).await?;
    ok(&record, &org)?;
    Ok(AdminContent {
        templates: parse_admin_templates(&templates.body)?,
        vocabulary: parse_vocabulary(&record.body)?,
    })
}

/// Read a template from the server for its editor.
#[tauri::command]
pub async fn org_template_open(
    state: State<'_, AppState>,
    org_id: String,
    client_id: String,
) -> Result<OrgTemplateDraft, String> {
    let (org, mine) = cached_org(&state, &org_id);
    let stored = lookup(&state, &org_id, &client_id, &org).await?;
    Ok(draft(&stored, &mine))
}

/// Create a template (`client_id` `None`) or save one. `workspaces` `None` is
/// every department.
#[tauri::command]
pub async fn org_template_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    client_id: Option<String>,
    name: String,
    content: String,
    workspaces: Option<Vec<String>>,
) -> Result<(), String> {
    let name = check_name(&name)?;
    let (org, mine) = cached_org(&state, &org_id);
    let now = crate::db::now_ms();
    let (body, answer) = match client_id {
        None => {
            if !safe_id(&org_id) {
                return Err("Not an organisation.".into());
            }
            let scope = scope_workspaces(&[], &mine, workspaces.as_deref())?;
            let client_id = uuid::Uuid::new_v4().to_string();
            let body = create_body(&org_id, &client_id, &name, &content, &scope, now);
            let answer = send(&state, TEMPLATES_PATH, Req::Post(&body)).await?;
            (body, answer)
        }
        Some(client_id) => {
            let stored = lookup(&state, &org_id, &client_id, &org).await?;
            let body = match update_plan(&stored, &mine, workspaces.as_deref(), &name, &content, now) {
                Ok(body) => body,
                Err(e) => {
                    refresh_config(&app, &state).await;
                    return Err(e);
                }
            };
            let answer = send(&state, &format!("{TEMPLATES_PATH}/{}", stored.record_id), Req::Patch(&body)).await?;
            (body, answer)
        }
    };
    ok(&answer, &org)?;
    refresh(&app, &state, &answer).await;
    if !write_landed(&body, &answer.body) {
        return Err(OVERTAKEN.into());
    }
    Ok(())
}

/// Remove a template for everyone in the organisation.
#[tauri::command]
pub async fn org_template_delete(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    client_id: String,
) -> Result<(), String> {
    let (org, _) = cached_org(&state, &org_id);
    let stored = lookup(&state, &org_id, &client_id, &org).await?;
    if stored.deleted {
        refresh_config(&app, &state).await;
        return Ok(());
    }
    let body = tombstone_body(next_stamp(crate::db::now_ms(), stored.client_updated_at));
    let answer = send(&state, &format!("{TEMPLATES_PATH}/{}", stored.record_id), Req::Patch(&body)).await?;
    ok(&answer, &org)?;
    refresh(&app, &state, &answer).await;
    if !write_landed(&body, &answer.body) {
        return Err(OVERTAKEN.into());
    }
    Ok(())
}

/// Add and remove organisation terms against what the server holds now.
#[tauri::command]
pub async fn org_vocabulary_edit(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<(), String> {
    if !safe_id(&org_id) {
        return Err("Not an organisation.".into());
    }
    let (org, _) = cached_org(&state, &org_id);
    let path = format!("{ORGS_PATH}/{org_id}");
    let current = send(&state, &path, Req::Get(&[("fields", "vocabulary")])).await?;
    ok(&current, &org)?;
    let terms = parse_vocabulary(&current.body)?;
    let next = edit_terms(&terms, &add, &remove)?;
    if next == terms {
        return Ok(());
    }
    let answer = send(&state, &path, Req::Patch(&vocabulary_body(&next))).await?;
    ok(&answer, &org)?;
    refresh(&app, &state, &answer).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_listing_gives_each_administered_organisation() {
        let body = json!({ "items": [
            { "id": "o1", "name": "Northwind", "require_2fa": true },
            { "id": "o2", "name": "Rana" },
            { "id": "bad id", "name": "Skipped" },
        ]});
        assert_eq!(
            parse_admin_orgs(&body).unwrap(),
            vec![
                AdminOrg { id: "o1".into(), name: "Northwind".into(), require_2fa: true },
                AdminOrg { id: "o2".into(), name: "Rana".into(), require_2fa: false },
            ]
        );
        assert_eq!(parse_admin_orgs(&json!({ "items": [] })).unwrap(), vec![]);
        assert!(parse_admin_orgs(&json!({ "message": "x" })).is_err());
    }

    #[test]
    fn terms_read_only_as_a_list_of_strings() {
        assert_eq!(parse_vocabulary(&json!({ "vocabulary": ["IPLOS", "Rana"] })).unwrap(), s(&["IPLOS", "Rana"]));
        assert_eq!(parse_vocabulary(&json!({ "vocabulary": null })).unwrap(), Vec::<String>::new());
        assert_eq!(parse_vocabulary(&json!({})).unwrap(), Vec::<String>::new());
        assert_eq!(parse_vocabulary(&json!({ "vocabulary": "" })).unwrap(), Vec::<String>::new());
        assert!(parse_vocabulary(&json!({ "vocabulary": [1] })).is_err());
        assert!(parse_vocabulary(&json!({ "vocabulary": { "a": 1 } })).is_err());
    }

    #[test]
    fn editing_terms_adds_removes_and_dedupes_regardless_of_case() {
        let current = s(&["IPLOS", "Rana"]);
        assert_eq!(edit_terms(&current, &s(&[" Northwind ", "iplos", "AI"]), &[]).unwrap(), s(&["IPLOS", "Rana", "Northwind"]));
        assert_eq!(edit_terms(&current, &[], &s(&["rana"])).unwrap(), s(&["IPLOS"]));
    }

    #[test]
    fn terms_stop_at_the_limit_but_a_removal_over_it_still_lands() {
        let full: Vec<String> = (0..VOCABULARY_LIMIT).map(|i| format!("Term{i}")).collect();
        assert!(edit_terms(&full, &s(&["Another"]), &[]).is_err());
        let over: Vec<String> = (0..VOCABULARY_LIMIT + 5).map(|i| format!("Term{i}")).collect();
        assert_eq!(edit_terms(&over, &[], &s(&["Term0"])).unwrap().len(), VOCABULARY_LIMIT + 4);
    }

    #[test]
    fn the_vocabulary_patch_carries_nothing_but_terms() {
        let body = vocabulary_body(&s(&["IPLOS"]));
        assert_eq!(body, json!({ "vocabulary": ["IPLOS"] }));
        assert_eq!(body.as_object().unwrap().len(), 1);
    }

    #[test]
    fn a_filter_takes_only_safe_ids() {
        assert_eq!(template_filter("o1", "a-b_c").unwrap(), "organisation='o1' && client_id='a-b_c'");
        assert!(template_filter("o1", "x' || 1=1 || '").is_err());
        assert!(template_filter("", "a").is_err());
    }

    #[test]
    fn a_lookup_reads_the_first_record_or_none() {
        let body = json!({ "items": [{
            "id": "r1", "name": "Case", "content": "<p>A &amp; B</p>", "workspaces": ["w1", "w9"],
            "client_updated_at": 1700000000000.0, "deleted": false,
        }] });
        let stored = parse_template_lookup(&body).unwrap().unwrap();
        assert_eq!(stored, StoredTemplate {
            record_id: "r1".into(),
            name: "Case".into(),
            content: "<p>A &amp; B</p>".into(),
            workspaces: s(&["w1", "w9"]),
            client_updated_at: 1_700_000_000_000,
            deleted: false,
        });
        assert_eq!(parse_template_lookup(&json!({ "items": [] })).unwrap(), None);
        assert!(parse_template_lookup(&json!({})).is_err());

        let d = draft(&stored, &s(&["w1", "w2"]));
        assert_eq!(d, OrgTemplateDraft { name: "Case".into(), content: "A & B".into(), workspaces: s(&["w1"]), hidden: 1 });
    }

    #[test]
    fn every_department_is_empty() {
        assert_eq!(scope_workspaces(&s(&["w1", "w9"]), &s(&["w1", "w2"]), None).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn departments_the_user_isnt_in_survive_a_save() {
        let stored = s(&["w9", "w1"]);
        let mine = s(&["w1", "w2"]);
        assert_eq!(scope_workspaces(&stored, &mine, Some(&s(&["w2"]))).unwrap(), s(&["w9", "w2"]));
        assert_eq!(scope_workspaces(&stored, &mine, Some(&[])).unwrap(), s(&["w9"]));
        assert_eq!(scope_workspaces(&stored, &mine, Some(&s(&["w2", "w1"]))).unwrap(), s(&["w9", "w1", "w2"]));
    }

    #[test]
    fn only_the_users_own_departments_can_be_chosen() {
        assert!(scope_workspaces(&[], &s(&["w1"]), Some(&s(&["w9"]))).is_err());
        assert!(scope_workspaces(&[], &s(&["w1"]), Some(&[])).is_err());
    }

    #[test]
    fn editor_html_reads_back_as_the_text_typed() {
        for text in ["One line", "Line one\nLine two", "Para\n\nNext <b> & \"x\"", "  padded\n"] {
            assert_eq!(org_policy::plain_text(&editor_html(text)), text.trim());
        }
        assert_eq!(editor_html("a < b"), "<p>a &lt; b</p>");
    }

    #[test]
    fn write_bodies_carry_lww_and_departments() {
        let body = create_body("o1", "c1", "Case", "Text", &s(&["w1"]), 5);
        assert_eq!(body, json!({
            "organisation": "o1", "client_id": "c1", "name": "Case", "content": "<p>Text</p>",
            "workspaces": ["w1"], "client_updated_at": 5, "deleted": false,
        }));
        let body = update_body("Case", "Text", &[], 6);
        assert!(body.get("organisation").is_none() && body.get("client_id").is_none());
        assert_eq!(body["workspaces"], json!([]));
        assert_eq!(tombstone_body(7), json!({ "deleted": true, "client_updated_at": 7 }));
    }

    #[test]
    fn refusals_read_in_plain_words() {
        let none = json!({});
        assert_eq!(
            refusal(403, &json!({ "reason": "two_factor_required" }), "Northwind"),
            "Northwind requires two-factor authentication. Set it up in Settings → Account."
        );
        assert_eq!(refusal(404, &none, "Northwind"), "Only Northwind's admins can change this.");
        assert_eq!(refusal(403, &none, "Northwind"), "Only Northwind's admins can change this.");
        assert_eq!(
            refusal(400, &json!({ "message": "Every department must belong to the template's organisation." }), "N"),
            "Every department must belong to the template's organisation."
        );
        assert_eq!(
            refusal(400, &json!({ "message": "Failed to create record." }), "N"),
            "N didn't accept the change. Check the name and try again."
        );
        assert_eq!(refusal(500, &none, "N"), "Couldn't save to N (500). Try again.");
    }

    #[test]
    fn an_admin_lists_live_templates_with_a_department_count() {
        assert_eq!(templates_filter("o1").unwrap(), "organisation='o1' && deleted!=true");
        assert!(templates_filter("o1' || '1").is_err());
        let body = json!({ "items": [
            { "client_id": "case", "name": "Case", "content": "<p>A</p><p>B</p>", "workspaces": [] },
            { "client_id": "home", "name": "Home", "content": "x", "workspaces": ["w1", "w9"] },
            { "client_id": "gone", "name": "Gone", "content": "", "deleted": true },
            { "client_id": "bad id", "name": "Bad", "content": "" },
        ]});
        assert_eq!(
            parse_admin_templates(&body).unwrap(),
            vec![
                AdminTemplate { client_id: "case".into(), name: "Case".into(), content: "A\nB".into(), workspaces: vec![] },
                AdminTemplate { client_id: "home".into(), name: "Home".into(), content: "x".into(), workspaces: s(&["w1", "w9"]) },
            ]
        );
        assert!(parse_admin_templates(&json!({})).is_err());
    }

    // Organisation content reaches this Mac only through the config's cache
    // (`org_policy::apply_templates`); nothing here may write the database.
    #[test]
    fn no_admin_command_writes_this_mac() {
        let src = include_str!("org_admin.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for forbidden in ["summary_prompts", "set_setting", "apply_templates", "execute(", "store("] {
            assert!(!code.contains(forbidden), "org_admin.rs must not use {forbidden}");
        }
    }

    fn stored(stamp: i64, deleted: bool) -> StoredTemplate {
        StoredTemplate {
            record_id: "r1".into(),
            name: "Case".into(),
            content: "<p>x</p>".into(),
            workspaces: s(&["w9"]),
            client_updated_at: stamp,
            deleted,
        }
    }

    #[test]
    fn a_deleted_template_is_refused_rather_than_brought_back() {
        assert_eq!(update_plan(&stored(5, true), &[], Some(&[]), "Case", "x", 10), Err(DELETED.to_string()));
    }

    #[test]
    fn a_clock_behind_the_stored_stamp_still_writes_newer() {
        assert_eq!(next_stamp(10, 5), 10);
        assert_eq!(next_stamp(10, 50), 51);
        let body = update_plan(&stored(50, false), &[], Some(&[]), "Case", "x", 10).unwrap();
        assert_eq!(body["client_updated_at"], json!(51));
        assert_eq!(body["workspaces"], json!(["w9"]));
    }

    #[test]
    fn a_stale_write_answered_with_the_stored_record_is_caught() {
        let sent = update_body("New name", "New text", &[], 51);
        let landed = json!({ "name": "New name", "content": "<p>New text</p>", "client_updated_at": 51.0, "deleted": false });
        assert!(write_landed(&sent, &landed));
        let kept = json!({ "name": "Case", "content": "<p>x</p>", "client_updated_at": 90, "deleted": false });
        assert!(!write_landed(&sent, &kept));
        let reworded = json!({ "name": "New name", "content": "<p>Other</p>", "client_updated_at": 51, "deleted": false });
        assert!(!write_landed(&sent, &reworded));
        let tombstone = tombstone_body(60);
        assert!(write_landed(&tombstone, &json!({ "name": "Case", "client_updated_at": 60, "deleted": true })));
        assert!(!write_landed(&tombstone, &json!({ "name": "Case", "client_updated_at": 90, "deleted": false })));
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(check_name("  Case  ").unwrap(), "Case");
        assert!(check_name("   ").is_err());
        assert!(check_name(&"x".repeat(NAME_MAX + 1)).is_err());
    }
}
