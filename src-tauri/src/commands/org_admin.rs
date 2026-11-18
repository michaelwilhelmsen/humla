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

/// The stored record behind a template.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoredTemplate {
    pub record_id: String,
    pub name: String,
    pub content: String,
    pub workspaces: Vec<String>,
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|s| s.as_str()).unwrap_or_default().to_string()
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
    }))
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

// ---- commands -----------------------------------------------------------------

/// The organisations the signed-in user administers.
#[tauri::command]
pub async fn org_admin_orgs(state: State<'_, AppState>) -> Result<Vec<AdminOrg>, String> {
    let query = [("fields", "id,name,require_2fa"), ("perPage", "200")];
    let answer = send(&state, ORGS_PATH, Req::Get(&query)).await?;
    ok(&answer, "Your organisation")?;
    parse_admin_orgs(&answer.body)
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
    let answer = match client_id {
        None => {
            if !safe_id(&org_id) {
                return Err("Not an organisation.".into());
            }
            let scope = scope_workspaces(&[], &mine, workspaces.as_deref())?;
            let client_id = uuid::Uuid::new_v4().to_string();
            let body = create_body(&org_id, &client_id, &name, &content, &scope, now);
            send(&state, TEMPLATES_PATH, Req::Post(&body)).await?
        }
        Some(client_id) => {
            let stored = lookup(&state, &org_id, &client_id, &org).await?;
            let scope = scope_workspaces(&stored.workspaces, &mine, workspaces.as_deref())?;
            let body = update_body(&name, &content, &scope, now);
            send(&state, &format!("{TEMPLATES_PATH}/{}", stored.record_id), Req::Patch(&body)).await?
        }
    };
    ok(&answer, &org)?;
    refresh(&app, &state, &answer).await;
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
    let body = tombstone_body(crate::db::now_ms());
    let answer = send(&state, &format!("{TEMPLATES_PATH}/{}", stored.record_id), Req::Patch(&body)).await?;
    ok(&answer, &org)?;
    refresh(&app, &state, &answer).await;
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
        let body = json!({ "items": [{ "id": "r1", "name": "Case", "content": "<p>A &amp; B</p>", "workspaces": ["w1", "w9"] }] });
        let stored = parse_template_lookup(&body).unwrap().unwrap();
        assert_eq!(stored, StoredTemplate {
            record_id: "r1".into(),
            name: "Case".into(),
            content: "<p>A &amp; B</p>".into(),
            workspaces: s(&["w1", "w9"]),
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
    fn names_are_trimmed_and_bounded() {
        assert_eq!(check_name("  Case  ").unwrap(), "Case");
        assert!(check_name("   ").is_err());
        assert!(check_name(&"x".repeat(NAME_MAX + 1)).is_err());
    }
}
