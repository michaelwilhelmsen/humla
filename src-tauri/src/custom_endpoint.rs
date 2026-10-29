//! The user's own OpenAI-compatible endpoint (#173): one base URL and an
//! optional key (Keychain account `custom_api_key`), shared by summaries, chat
//! and transcription, each with a model of its own. The base URL is used
//! exactly as typed — Humla appends only the route — so a wrong root has to be
//! visible in the errors instead.

use crate::db;
use anyhow::anyhow;

pub const BASE_URL_SETTING: &str = "custom_base_url";
pub const SUMMARY_MODEL_SETTING: &str = "custom_model";
pub const CHAT_MODEL_SETTING: &str = "custom_chat_model";

pub const NO_BASE_URL: &str = "The custom endpoint has no base URL — add one in Settings.";

fn setting(conn: &rusqlite::Connection, key: &str) -> anyhow::Result<Option<String>> {
    Ok(db::get_setting(conn, key)?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// The base URL as the user typed it, surrounding whitespace aside.
pub fn base_url(conn: &rusqlite::Connection) -> anyhow::Result<String> {
    setting(conn, BASE_URL_SETTING)?.ok_or_else(|| anyhow!(NO_BASE_URL))
}

/// One surface's model on the endpoint, read from that surface's setting.
pub fn model(conn: &rusqlite::Connection, setting_key: &str) -> anyhow::Result<String> {
    setting(conn, setting_key)?
        .ok_or_else(|| anyhow!("No model is set for the custom endpoint — type one in Settings."))
}

/// The error for a non-2xx answer, naming the full URL that was requested.
pub fn status_error(status: u16, url: &str, body: &str) -> anyhow::Error {
    let hint = match status {
        401 | 403 => " The endpoint refused the API key — check the key in Settings.",
        404 => {
            " Nothing answers at that address, or it doesn't know the model — check the base URL \
             and model in Settings."
        }
        429 => " The endpoint is rate limiting or out of quota — try again later.",
        _ => "",
    };
    let body: String = body.trim().chars().take(300).collect();
    if body.is_empty() {
        anyhow!("HTTP {status} from {url}.{hint}")
    } else {
        anyhow!("HTTP {status} from {url}.{hint} {body}")
    }
}

/// The models the endpoint lists, sorted. Only ever a help in filling the model
/// field: a gateway may serve no listing and still answer completions.
pub async fn list_models(base_url: &str, api_key: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut ids = crate::openai::list_models_with_key(base_url, api_key).await?;
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Probe {
    pub outcome: Outcome,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Check {
    pub listing: Probe,
    pub completion: Probe,
}

/// Settings' Test: the listing and a one-line completion, each reported on its
/// own. The completion is the summary's own request; it carries no tools, so it
/// can't show that a model will call chat's.
pub async fn check(base_url: &str, api_key: Option<&str>, model: Option<&str>) -> Check {
    let listing = match list_models(base_url, api_key).await {
        Ok(ids) => Probe { outcome: Outcome::Ok, message: format!("Lists {} models.", ids.len()) },
        Err(e) => Probe { outcome: Outcome::Failed, message: format!("{e:#}") },
    };
    let completion = match model.map(str::trim).filter(|m| !m.is_empty()) {
        None => Probe {
            outcome: Outcome::Skipped,
            message: "No model is set, so no completion was asked for.".into(),
        },
        Some(model) => {
            match crate::openai::summarize_custom(base_url, api_key, model, "Reply with OK.", "OK?", |_| {})
                .await
            {
                Ok(_) => Probe { outcome: Outcome::Ok, message: format!("{model} answered.") },
                Err(e) => Probe { outcome: Outcome::Failed, message: format!("{e:#}") },
            }
        }
    };
    Check { listing, completion }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::test_server::serve;

    const LISTING: &str = r#"{"object":"list","data":[{"id":"z-model"},{"id":"a-model"}]}"#;
    const ANSWER: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\ndata: [DONE]\n\n";

    #[tokio::test]
    async fn reports_the_listing_and_a_completion_separately() {
        let (port, server) = serve(vec![
            (200, "application/json", LISTING.into()),
            (200, "text/event-stream", ANSWER.into()),
        ])
        .await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let report = check(&base, Some("sk-test"), Some("a-model")).await;
        assert_eq!(report.listing.outcome, Outcome::Ok);
        assert!(report.listing.message.contains('2'), "{}", report.listing.message);
        assert_eq!(report.completion.outcome, Outcome::Ok);
        let seen = server.await.unwrap();
        assert!(seen[0].starts_with("GET /v1/models "), "{}", seen[0]);
        assert!(seen[0].to_lowercase().contains("authorization: bearer sk-test"));
        assert!(seen[1].starts_with("POST /v1/chat/completions "), "{}", seen[1]);
    }

    // Some gateways gate or omit `GET /models` while chat completions work.
    #[tokio::test]
    async fn a_missing_listing_does_not_fail_the_completion() {
        let (port, _server) = serve(vec![
            (404, "text/plain", "no such route".into()),
            (200, "text/event-stream", ANSWER.into()),
        ])
        .await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let report = check(&base, None, Some("a-model")).await;
        assert_eq!(report.listing.outcome, Outcome::Failed);
        assert!(report.listing.message.contains(&format!("{base}/models")), "{}", report.listing.message);
        assert_eq!(report.completion.outcome, Outcome::Ok);
    }

    #[tokio::test]
    async fn without_a_model_the_completion_is_skipped_not_failed() {
        let (port, _server) = serve(vec![(200, "application/json", LISTING.into())]).await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let report = check(&base, None, Some("  ")).await;
        assert_eq!(report.listing.outcome, Outcome::Ok);
        assert_eq!(report.completion.outcome, Outcome::Skipped);
    }

    #[tokio::test]
    async fn the_listing_comes_back_sorted() {
        let (port, _server) = serve(vec![(200, "application/json", LISTING.into())]).await;
        let base = format!("http://127.0.0.1:{port}/v1");
        assert_eq!(list_models(&base, None).await.unwrap(), ["a-model", "z-model"]);
    }
}
