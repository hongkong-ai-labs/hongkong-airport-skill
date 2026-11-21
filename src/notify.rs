//! Best-effort notification delivery for watchlist alerts.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

pub async fn desktop(title: &str, body: &str) -> Result<()> {
    let title = title.to_string();
    let body = body.to_string();
    tokio::task::spawn_blocking(move || {
        notify_rust::Notification::new()
            .appname("hkia")
            .summary(&title)
            .body(&body)
            .show()
            .map(|_| ())
            .map_err(|error| anyhow!("desktop notification failed: {error}"))
    })
    .await
    .context("desktop notification task panicked")??;
    Ok(())
}

pub async fn webhook(url: &str, payload: &Value) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("building webhook client")?;
    let response = http
        .post(url)
        .json(payload)
        .send()
        .await
        .with_context(|| format!("posting webhook to {url}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "webhook returned HTTP {}: {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }
    Ok(())
}
