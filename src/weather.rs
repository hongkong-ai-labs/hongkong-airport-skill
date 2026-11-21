//! Hong Kong Observatory weather warning context.
//!
//! Public endpoint:
//! `https://data.weather.gov.hk/weatherAPI/opendata/weather.php?dataType=warningInfo&lang=en`
//!
//! Warnings are context, not a claim about the cause of any specific delay.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api;

pub const DEFAULT_WEATHER_URL: &str = "https://data.weather.gov.hk/weatherAPI/opendata/weather.php";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherWarning {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub contents: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WeatherReport {
    pub source: String,
    pub fetched_at: String,
    pub warnings: Vec<WeatherWarning>,
}

pub struct WeatherClient {
    http: reqwest::Client,
    base: String,
    cache: Mutex<HashMap<String, (Instant, Vec<WeatherWarning>)>>,
}

impl WeatherClient {
    pub fn new() -> Result<Self> {
        let base = std::env::var("HKIA_WEATHER_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_WEATHER_URL.to_string());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(format!("hkia-rust/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building weather HTTP client")?;
        Ok(Self {
            http,
            base,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub async fn warnings(&self, lang: &str) -> Result<Vec<WeatherWarning>> {
        let hko_lang = match lang.trim() {
            "zh_HK" | "zh_hk" | "zh-HK" => "tc",
            "zh_CN" | "zh_cn" | "zh-CN" => "sc",
            _ => "en",
        };
        if let Some((fetched_at, cached)) = self
            .cache
            .lock()
            .expect("weather cache lock poisoned")
            .get(hko_lang)
            .cloned()
            && fetched_at.elapsed() < Duration::from_secs(300)
        {
            return Ok(cached);
        }

        let url = format!(
            "{}?dataType=warningInfo&lang={}",
            self.base.trim_end_matches('?'),
            hko_lang
        );
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .context("requesting Hong Kong Observatory warnings")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Hong Kong Observatory returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let value: Value = response
            .json()
            .await
            .context("decoding Hong Kong Observatory warnings")?;
        let warnings = parse_warnings(&value);
        self.cache
            .lock()
            .expect("weather cache lock poisoned")
            .insert(hko_lang.to_string(), (Instant::now(), warnings.clone()));
        Ok(warnings)
    }

    pub async fn report(&self, lang: &str) -> Result<WeatherReport> {
        Ok(WeatherReport {
            source: "Hong Kong Observatory".to_string(),
            fetched_at: api::hk_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            warnings: self.warnings(lang).await?,
        })
    }
}

pub fn parse_warnings(value: &Value) -> Vec<WeatherWarning> {
    let mut warnings = Vec::new();
    if let Some(details) = value.get("details").and_then(|value| value.as_array()) {
        for detail in details {
            warnings.push(WeatherWarning {
                code: string_field(detail, "warningStatementCode"),
                subtype: string_field(detail, "subtype"),
                contents: contents_field(detail),
                updated_at: string_field(detail, "updateTime"),
            });
        }
    }
    if warnings.is_empty()
        && let Some(statement) = value
            .get("warningStatement")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
    {
        warnings.push(WeatherWarning {
            code: None,
            subtype: None,
            contents: vec![statement.to_string()],
            updated_at: string_field(value, "updateTime"),
        });
    }
    warnings
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn contents_field(value: &Value) -> Vec<String> {
    match value.get("contents") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Some(Value::String(text)) => {
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![text.clone()]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hko_details() {
        let value = serde_json::json!({
            "details": [{
                "contents": ["The fire danger warning is Red."],
                "warningStatementCode": "WFIRE",
                "subtype": "WFIRER",
                "updateTime": "2026-11-21T06:00:00+08:00"
            }]
        });
        let warnings = parse_warnings(&value);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_deref(), Some("WFIRE"));
        assert_eq!(warnings[0].contents.len(), 1);
    }
}
