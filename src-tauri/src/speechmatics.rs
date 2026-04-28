use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::{Duration, Instant};

// Speechmatics SaaS Batch API. The endpoint depends on the region the user's
// account was provisioned in. EU1/US1/AU1 are self-serve; EU2/US2 are
// Enterprise-only. A key from one region returns nginx 401 on another.
pub fn base_url(region: &str) -> String {
    let host = match region {
        "eu2" => "eu2.asr.api.speechmatics.com",
        "us1" => "us1.asr.api.speechmatics.com",
        "us2" => "us2.asr.api.speechmatics.com",
        "au1" => "au1.asr.api.speechmatics.com",
        _ => "eu1.asr.api.speechmatics.com", // default
    };
    format!("https://{host}/v2")
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        // Generous timeout: a single chunk can sit in the queue for tens of
        // seconds during peak load.
        .timeout(Duration::from_secs(180))
        .build()
        .expect("reqwest client")
}

#[derive(Deserialize)]
struct SubmitResp {
    id: String,
}

#[derive(Deserialize)]
struct StatusResp {
    job: StatusJob,
}

#[derive(Deserialize)]
struct StatusJob {
    status: String,
}

pub async fn transcribe_file(
    api_key: &str,
    region: &str,
    language: &str,
    operating_point: &str,
    additional_vocab: &[String],
    audio_path: &Path,
) -> Result<String> {
    let base = base_url(region);
    let bytes = tokio::fs::read(audio_path).await?;
    let file_name = audio_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("chunk.wav")
        .to_string();

    let lang = if language == "auto" { "en" } else { language };
    let mut transcription_config = serde_json::json!({
        "language": lang,
        "operating_point": operating_point,
    });
    if !additional_vocab.is_empty() {
        // Speechmatics accepts a list of {content, sounds_like?} entries.
        // We only have plain strings, so map content-only.
        let entries: Vec<serde_json::Value> = additional_vocab
            .iter()
            .map(|w| serde_json::json!({ "content": w }))
            .collect();
        transcription_config["additional_vocab"] = serde_json::Value::Array(entries);
    }
    let config = serde_json::json!({
        "type": "transcription",
        "transcription_config": transcription_config,
    });

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new()
        .part("data_file", part)
        .text("config", config.to_string());

    let r = client()
        .post(format!("{base}/jobs/"))
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await?;
    if !r.status().is_success() {
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("Speechmatics submit {s}: {body}"));
    }
    let job_id = r.json::<SubmitResp>().await?.id;

    // Poll for completion. Short audio (≤30s) usually finishes within 5–15s.
    // Cap at 5 minutes so a stuck job can't hang a recording session.
    let started = Instant::now();
    let deadline = Duration::from_secs(300);
    let mut delay = Duration::from_millis(800);
    loop {
        if started.elapsed() > deadline {
            return Err(anyhow!("Speechmatics job {job_id} timed out"));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5));

        let s = client()
            .get(format!("{base}/jobs/{job_id}"))
            .bearer_auth(api_key)
            .send()
            .await?;
        if !s.status().is_success() {
            // Transient (e.g., 502); keep polling.
            continue;
        }
        let status = s.json::<StatusResp>().await?;
        match status.job.status.as_str() {
            "running" => continue,
            "done" => break,
            "rejected" => {
                return Err(anyhow!("Speechmatics job {job_id} rejected"));
            }
            other => return Err(anyhow!("Speechmatics job {job_id} unknown status: {other}")),
        }
    }

    // Plain-text format avoids the JSON word-by-word schema; we just want
    // the same kind of output the OpenAI path produces.
    let r = client()
        .get(format!("{base}/jobs/{job_id}/transcript?format=txt"))
        .bearer_auth(api_key)
        .send()
        .await?;
    if !r.status().is_success() {
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("Speechmatics transcript {s}: {body}"));
    }
    Ok(r.text().await?)
}
