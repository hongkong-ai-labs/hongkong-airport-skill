//! Client and normalisation layer for Hong Kong International Airport flight data.
//!
//! Data comes from the airport's public flight-information REST service, the same
//! feed used by the HKIA website and app:
//!
//! `GET https://www.hongkongairport.com/flightinfo-rest/rest/flights`
//!   `?date=YYYY-MM-DD&lang=en&cargo=false&arrival=false`
//!
//! The feed reports a board per calendar day, with free-text status values such as
//! `Dep 00:00 (21/11/2026)`, `Est at 07:00`, `At gate 02:31` and `Cancelled`.
//! This module turns those strings into structured times and computes delays
//! against the scheduled time.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::{
    DateTime, Duration as ChronoDuration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime,
    SecondsFormat, TimeZone, Utc,
};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::reference;

pub const HK_OFFSET_SECS: i32 = 8 * 3600;
pub const DEFAULT_BASE_URL: &str = "https://www.hongkongairport.com/flightinfo-rest/rest";

pub fn hk_offset() -> FixedOffset {
    FixedOffset::east_opt(HK_OFFSET_SECS).expect("valid Hong Kong offset")
}

pub fn hk_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&hk_offset())
}

pub fn today_hk() -> NaiveDate {
    hk_now().date_naive()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[value(name = "departure", alias = "dep")]
    Departure,
    #[value(name = "arrival", alias = "arr")]
    Arrival,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Departure => "departure",
            Direction::Arrival => "arrival",
        }
    }

    pub fn is_arrival(self) -> bool {
        matches!(self, Direction::Arrival)
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "departure" | "departures" | "dep" | "outbound" | "d" => Some(Direction::Departure),
            "arrival" | "arrivals" | "arr" | "inbound" | "a" => Some(Direction::Arrival),
            _ => None,
        }
    }
}

/// Resolve a user-supplied date against the Hong Kong calendar.
pub fn resolve_date(input: Option<&str>) -> Result<NaiveDate> {
    let today = today_hk();
    match input.map(|value| value.trim().to_ascii_lowercase()) {
        None => Ok(today),
        Some(value) if value.is_empty() || value == "today" || value == "now" => Ok(today),
        Some(value) if value == "tomorrow" => Ok(today + ChronoDuration::days(1)),
        Some(value) if value == "yesterday" => Ok(today - ChronoDuration::days(1)),
        Some(value) => NaiveDate::parse_from_str(&value, "%Y-%m-%d").map_err(|_| {
            anyhow!("invalid date '{value}'; use YYYY-MM-DD, today, yesterday or tomorrow")
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flight {
    pub flight_number: String,
    pub airline: String,
    #[serde(default)]
    pub airline_name: Option<String>,
    pub codeshares: Vec<String>,
    pub direction: String,
    pub date: String,
    pub scheduled_time: String,
    pub scheduled_at: String,
    pub origin: Vec<String>,
    pub destination: Vec<String>,
    #[serde(default)]
    pub origin_details: Vec<reference::AirportInfo>,
    #[serde(default)]
    pub destination_details: Vec<reference::AirportInfo>,
    pub status: Option<String>,
    pub status_type: String,
    #[serde(default)]
    pub status_published: bool,
    pub estimated_at: Option<String>,
    pub actual_at: Option<String>,
    pub delay_minutes: Option<i64>,
    #[serde(default)]
    pub delay_basis: Option<String>,
    pub state: String,
    pub cancelled: bool,
    pub terminal: Option<String>,
    pub gate: Option<String>,
    pub check_in_aisle: Option<String>,
    pub baggage_belt: Option<String>,
    pub arrival_hall: Option<String>,
    pub stand: Option<String>,
    #[serde(default)]
    pub guidance: Option<String>,
    #[serde(default)]
    pub fetched_at: Option<String>,
}

impl Flight {
    pub fn route(&self) -> Vec<String> {
        if self.direction == "arrival" {
            self.origin.clone()
        } else {
            self.destination.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Board {
    pub date: String,
    pub direction: String,
    pub cargo: bool,
    pub last_updated: Option<String>,
    pub fetched_at: String,
    pub flights: Vec<Flight>,
}

#[derive(Debug, Deserialize)]
struct DayBoard {
    #[serde(default)]
    date: String,
    #[serde(default)]
    list: Vec<RawFlight>,
    #[serde(rename = "lastUpdatedTime", default)]
    last_updated: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFlight {
    #[serde(default)]
    time: String,
    #[serde(default)]
    flight: Vec<RawFlightNo>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    destination: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec")]
    origin: Vec<String>,
    #[serde(default)]
    terminal: Option<String>,
    #[serde(default)]
    aisle: Option<String>,
    #[serde(default)]
    gate: Option<String>,
    #[serde(default)]
    baggage: Option<String>,
    #[serde(default)]
    hall: Option<String>,
    #[serde(default)]
    stand: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFlightNo {
    #[serde(default)]
    no: String,
    #[serde(default)]
    airline: String,
}

/// The public feed normally returns arrays, but single values have been seen.
fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::String(text) => {
            if text.trim().is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![text])
            }
        }
        serde_json::Value::Array(items) => Ok(items
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(text) if !text.trim().is_empty() => Some(text),
                _ => None,
            })
            .collect()),
        other => Err(D::Error::custom(format!(
            "expected a string or an array of strings, got {other}"
        ))),
    }
}

#[derive(Debug, Clone)]
struct ParsedStatus {
    status_type: &'static str,
    estimated: Option<DateTime<FixedOffset>>,
    actual: Option<DateTime<FixedOffset>>,
    cancelled: bool,
}

impl Default for ParsedStatus {
    fn default() -> Self {
        Self {
            status_type: "scheduled",
            estimated: None,
            actual: None,
            cancelled: false,
        }
    }
}

fn parse_status(
    raw: Option<&str>,
    board_date: NaiveDate,
    scheduled: NaiveDateTime,
) -> ParsedStatus {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return ParsedStatus::default();
    };

    let lower = raw.to_ascii_lowercase();
    if lower.contains("cancel") {
        return ParsedStatus {
            status_type: "cancelled",
            cancelled: true,
            ..ParsedStatus::default()
        };
    }

    let estimated = lower.starts_with("est") || lower.contains("expected");
    let departed = lower.starts_with("dep") || lower.contains("departed");
    let arrived = lower.starts_with("at gate")
        || lower.starts_with("landed")
        || lower.starts_with("arriv")
        || lower.starts_with("arr ");

    let status_type = if estimated {
        "estimated"
    } else if departed {
        "departed"
    } else if arrived {
        "arrived"
    } else {
        "other"
    };

    let mut parsed = ParsedStatus {
        status_type,
        ..ParsedStatus::default()
    };

    if let Some((hour, minute)) = parse_clock(raw) {
        let explicit_date = parse_status_date(raw);
        let date = explicit_date.unwrap_or(board_date);
        let Some(naive) = date.and_hms_opt(hour, minute, 0) else {
            return parsed;
        };
        let Some(mut event) = hk_offset().from_local_datetime(&naive).single() else {
            return parsed;
        };
        if explicit_date.is_none() {
            // Post-midnight statuses sometimes omit the date, e.g. a 23:55 flight
            // showing "Dep 00:10". Treat a large backwards jump as a day rollover.
            if let Some(scheduled_at) = hk_offset().from_local_datetime(&scheduled).single()
                && event < scheduled_at - ChronoDuration::hours(12)
            {
                event += ChronoDuration::days(1);
            }
        }
        if estimated {
            parsed.estimated = Some(event);
        } else {
            parsed.actual = Some(event);
        }
        parsed.status_type = status_type;
    }

    parsed
}

/// Find the first `HH:MM` token in a status string.
fn parse_clock(text: &str) -> Option<(u32, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() < 5 {
        return None;
    }
    for index in 0..=bytes.len() - 5 {
        if bytes[index].is_ascii_digit()
            && bytes[index + 1].is_ascii_digit()
            && bytes[index + 2] == b':'
            && bytes[index + 3].is_ascii_digit()
            && bytes[index + 4].is_ascii_digit()
        {
            let hour: u32 = text[index..index + 2].parse().ok()?;
            let minute: u32 = text[index + 3..index + 5].parse().ok()?;
            if hour < 24 && minute < 60 {
                return Some((hour, minute));
            }
        }
    }
    None
}

/// Parse the `(dd/mm/yyyy)` suffix that the feed adds when a status crosses midnight.
fn parse_status_date(text: &str) -> Option<NaiveDate> {
    let start = text.find('(')? + 1;
    let rest = &text[start..];
    let end = rest.find(')')?;
    NaiveDate::parse_from_str(rest[..end].trim(), "%d/%m/%Y").ok()
}

/// Canonical form for matching: letters and digits only, uppercased.
fn canonical_flight_number(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Display form: whitespace collapsed and uppercased, e.g. `CX 251`.
fn normalize_flight_number(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn build_board(day: &DayBoard, direction: Direction, cargo: bool) -> Board {
    let fetched_at = hk_now().to_rfc3339_opts(SecondsFormat::Secs, false);
    let mut flights = build_flights(day, direction);
    for flight in &mut flights {
        flight.fetched_at = Some(fetched_at.clone());
    }
    Board {
        date: day.date.trim().to_string(),
        direction: direction.as_str().to_string(),
        cargo,
        last_updated: day.last_updated.clone(),
        fetched_at,
        flights,
    }
}

fn guidance_for(
    direction: Direction,
    terminal: Option<&str>,
    gate: Option<&str>,
    aisle: Option<&str>,
    stand: Option<&str>,
    belt: Option<&str>,
    hall: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if direction == Direction::Departure {
        if let Some(terminal) = terminal.map(str::trim).filter(|value| !value.is_empty()) {
            let label = terminal.strip_prefix('T').unwrap_or(terminal);
            parts.push(format!("Terminal {label}"));
        }
        if let Some(aisle) = aisle.map(str::trim).filter(|value| !value.is_empty()) {
            parts.push(format!("check-in aisle {aisle}"));
        }
        if let Some(gate) = gate.map(str::trim).filter(|value| !value.is_empty()) {
            parts.push(format!("gate {gate}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(format!("Depart from {}", parts.join(", ")))
        }
    } else {
        let mut airport = Vec::new();
        if let Some(terminal) = terminal.map(str::trim).filter(|value| !value.is_empty()) {
            let label = terminal.strip_prefix('T').unwrap_or(terminal);
            airport.push(format!("Terminal {label}"));
        }
        if let Some(gate) = gate.map(str::trim).filter(|value| !value.is_empty()) {
            airport.push(format!("gate {gate}"));
        }
        if let Some(stand) = stand.map(str::trim).filter(|value| !value.is_empty()) {
            airport.push(format!("stand {stand}"));
        }
        if let Some(belt) = belt.map(str::trim).filter(|value| !value.is_empty()) {
            parts.push(format!("baggage belt {belt}"));
        }
        if let Some(hall) = hall.map(str::trim).filter(|value| !value.is_empty()) {
            parts.push(format!("arrival hall {hall}"));
        }
        if airport.is_empty() && parts.is_empty() {
            None
        } else {
            let mut guidance = if airport.is_empty() {
                "Arrive at HKG".to_string()
            } else {
                format!("Arrive at {}", airport.join(", "))
            };
            if !parts.is_empty() {
                guidance.push(';');
                guidance.push(' ');
                guidance.push_str(&parts.join(", "));
            }
            Some(guidance)
        }
    }
}

fn build_flights(day: &DayBoard, direction: Direction) -> Vec<Flight> {
    let Ok(board_date) = NaiveDate::parse_from_str(day.date.trim(), "%Y-%m-%d") else {
        return Vec::new();
    };

    let mut flights = Vec::with_capacity(day.list.len());
    for raw in &day.list {
        let Some(primary) = raw.flight.first() else {
            continue;
        };
        let flight_number = normalize_flight_number(&primary.no);
        if flight_number.is_empty() {
            continue;
        }
        let Ok(time) = NaiveTime::parse_from_str(raw.time.trim(), "%H:%M") else {
            continue;
        };
        let scheduled_naive = board_date.and_time(time);
        let Some(scheduled_at) = hk_offset().from_local_datetime(&scheduled_naive).single() else {
            continue;
        };

        let parsed = parse_status(raw.status.as_deref(), board_date, scheduled_naive);
        let delay_basis = if parsed.actual.is_some() {
            Some("actual".to_string())
        } else if parsed.estimated.is_some() {
            Some("estimated".to_string())
        } else {
            None
        };
        let event = parsed
            .actual
            .as_ref()
            .or(parsed.estimated.as_ref())
            .cloned();
        let delay_minutes = event.map(|event| (event - scheduled_at).num_minutes());

        let cancelled = parsed.cancelled;
        let state = if cancelled {
            "cancelled"
        } else {
            match delay_minutes {
                Some(delay) if delay >= 15 => "delayed",
                Some(delay) if delay <= -15 => "early",
                Some(_) => "on_time",
                None => "scheduled",
            }
        };

        let origin: Vec<String> = raw
            .origin
            .iter()
            .map(|code| code.trim().to_ascii_uppercase())
            .filter(|code| !code.is_empty())
            .collect();
        let destination: Vec<String> = raw
            .destination
            .iter()
            .map(|code| code.trim().to_ascii_uppercase())
            .filter(|code| !code.is_empty())
            .collect();
        let origin_details = origin
            .iter()
            .filter_map(|code| reference::airport(code))
            .collect();
        let destination_details = destination
            .iter()
            .filter_map(|code| reference::airport(code))
            .collect();
        let airline = primary.airline.trim().to_ascii_uppercase();
        let status = clean(raw.status.as_deref());
        let terminal = clean(raw.terminal.as_deref());
        let gate = clean(raw.gate.as_deref());
        let aisle = clean(raw.aisle.as_deref());
        let belt = clean(raw.baggage.as_deref());
        let hall = clean(raw.hall.as_deref());
        let stand = clean(raw.stand.as_deref());
        let guidance = if cancelled {
            Some("Flight cancelled".to_string())
        } else {
            guidance_for(
                direction,
                terminal.as_deref(),
                gate.as_deref(),
                aisle.as_deref(),
                stand.as_deref(),
                belt.as_deref(),
                hall.as_deref(),
            )
        };

        flights.push(Flight {
            flight_number: flight_number.clone(),
            airline: airline.clone(),
            airline_name: reference::airline_name(&airline),
            codeshares: raw
                .flight
                .iter()
                .skip(1)
                .map(|flight| normalize_flight_number(&flight.no))
                .filter(|number| !number.is_empty())
                .collect(),
            direction: direction.as_str().to_string(),
            date: board_date.to_string(),
            scheduled_time: time.format("%H:%M").to_string(),
            scheduled_at: scheduled_at.to_rfc3339_opts(SecondsFormat::Secs, false),
            origin,
            destination,
            origin_details,
            destination_details,
            status: status.clone(),
            status_type: parsed.status_type.to_string(),
            status_published: status.is_some(),
            estimated_at: parsed
                .estimated
                .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, false)),
            actual_at: parsed
                .actual
                .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, false)),
            delay_minutes,
            delay_basis,
            state: state.to_string(),
            cancelled,
            terminal,
            gate,
            check_in_aisle: aisle,
            baggage_belt: belt,
            arrival_hall: hall,
            stand,
            guidance,
            fetched_at: None,
        });
    }

    flights.sort_by(|left, right| {
        left.scheduled_at
            .cmp(&right.scheduled_at)
            .then_with(|| left.flight_number.cmp(&right.flight_number))
    });
    flights
}

/// Decode a raw board response and select the requested date.
///
/// The feed can include the previous day's tail block, so selecting by date is
/// part of normalisation rather than an optional filter.
pub fn board_from_body(
    body: &str,
    date: NaiveDate,
    direction: Direction,
    cargo: bool,
) -> Result<Board> {
    let days: Vec<DayBoard> = serde_json::from_str(body).with_context(|| {
        format!(
            "decoding the HKIA response for {date}: {}",
            api_error_message(body)
        )
    })?;
    let day = days
        .into_iter()
        .find(|day| day.date.trim() == date.to_string())
        .ok_or_else(|| {
            anyhow!(
                "HKIA returned no board for {date}; schedules are published per calendar day and future dates may not be available yet"
            )
        })?;
    Ok(build_board(&day, direction, cargo))
}

/// Cached HKIA REST client.
pub struct Client {
    http: reqwest::Client,
    base: String,
    ttl: Duration,
    cache: Mutex<HashMap<String, (Instant, std::sync::Arc<Board>)>>,
}

impl Client {
    pub fn new(ttl: Duration) -> Result<Self> {
        let base = std::env::var("HKIA_API_BASE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(25))
            .user_agent(format!("hkia-rust/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            base,
            ttl,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub async fn board(
        &self,
        date: NaiveDate,
        direction: Direction,
        cargo: bool,
        lang: &str,
    ) -> Result<std::sync::Arc<Board>> {
        self.board_mode(date, direction, cargo, lang, false).await
    }

    /// Historical board. The airport accepts roughly D-91 through D+14.
    pub async fn past_board(
        &self,
        date: NaiveDate,
        direction: Direction,
        cargo: bool,
        lang: &str,
    ) -> Result<std::sync::Arc<Board>> {
        self.board_mode(date, direction, cargo, lang, true).await
    }

    async fn board_mode(
        &self,
        date: NaiveDate,
        direction: Direction,
        cargo: bool,
        lang: &str,
        past: bool,
    ) -> Result<std::sync::Arc<Board>> {
        let lang = normalize_lang(lang)?;
        let key = format!("{date}|{}|{cargo}|{lang}|{past}", direction.as_str());

        if !self.ttl.is_zero()
            && let Some((fetched_at, cached)) = self
                .cache
                .lock()
                .expect("cache lock poisoned")
                .get(&key)
                .cloned()
            && fetched_at.elapsed() < self.ttl
        {
            return Ok(cached);
        }

        let endpoint = if past { "flights/past" } else { "flights" };
        let url = format!(
            "{}/{}?date={}&lang={}&cargo={}&arrival={}",
            self.base.trim_end_matches('/'),
            endpoint,
            date.format("%Y-%m-%d"),
            lang,
            cargo,
            direction.is_arrival()
        );
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting the HKIA board for {date}"))?;

        let http_status = response.status();
        let body = response
            .text()
            .await
            .context("reading the HKIA response body")?;

        if !http_status.is_success() {
            return Err(anyhow!(
                "HKIA API returned HTTP {} for {date}: {}",
                http_status.as_u16(),
                api_error_message(&body)
            ));
        }

        let board = std::sync::Arc::new(
            board_from_body(&body, date, direction, cargo)
                .with_context(|| format!("loading the HKIA board for {date}"))?,
        );
        if !self.ttl.is_zero() {
            self.cache
                .lock()
                .expect("cache lock poisoned")
                .insert(key, (Instant::now(), board.clone()));
        }
        Ok(board)
    }
}

fn normalize_lang(lang: &str) -> Result<String> {
    match lang.trim() {
        "en" | "EN" | "En" => Ok("en".to_string()),
        "zh_HK" | "zh_hk" | "zh-HK" | "zh-hk" => Ok("zh_HK".to_string()),
        "zh_CN" | "zh_cn" | "zh-CN" | "zh-cn" => Ok("zh_CN".to_string()),
        other => Err(anyhow!(
            "unsupported language '{other}'; use en, zh_HK or zh_CN"
        )),
    }
}

fn api_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(message) = value.get("message").and_then(|value| value.as_str())
    {
        return message.to_string();
    }
    let trimmed = body.trim().replace('\n', " ");
    trimmed.chars().take(300).collect()
}

async fn fetch_direction(
    client: &Client,
    date: NaiveDate,
    direction: Direction,
    cargo: bool,
    lang: &str,
    past: bool,
) -> Result<std::sync::Arc<Board>> {
    if past {
        client.past_board(date, direction, cargo, lang).await
    } else {
        client.board(date, direction, cargo, lang).await
    }
}

/// Fetch one direction, or both when `direction` is `None`.
pub async fn fetch_flights(
    client: &Client,
    date: NaiveDate,
    direction: Option<Direction>,
    cargo: bool,
    lang: &str,
) -> Result<(Vec<Flight>, Option<String>)> {
    fetch_flights_at(client, date, direction, cargo, lang, false).await
}

/// Fetch live or historical boards. `past` selects the airport's historical endpoint.
pub async fn fetch_flights_at(
    client: &Client,
    date: NaiveDate,
    direction: Option<Direction>,
    cargo: bool,
    lang: &str,
    past: bool,
) -> Result<(Vec<Flight>, Option<String>)> {
    match direction {
        Some(direction) => {
            let board = fetch_direction(client, date, direction, cargo, lang, past).await?;
            Ok((board.flights.clone(), board.last_updated.clone()))
        }
        None => {
            let (departures, arrivals) = tokio::join!(
                fetch_direction(client, date, Direction::Departure, cargo, lang, past),
                fetch_direction(client, date, Direction::Arrival, cargo, lang, past)
            );
            let departures = departures?;
            let mut flights = departures.flights.clone();
            let mut last_updated = departures.last_updated.clone();
            match arrivals {
                Ok(arrivals) => {
                    flights.extend(arrivals.flights.iter().cloned());
                    if last_updated.is_none() {
                        last_updated = arrivals.last_updated.clone();
                    }
                }
                Err(error) => eprintln!("warning: arrival board unavailable: {error}"),
            }
            flights.sort_by(|left, right| {
                left.scheduled_at
                    .cmp(&right.scheduled_at)
                    .then_with(|| left.flight_number.cmp(&right.flight_number))
            });
            Ok((flights, last_updated))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Scheduled,
    Delay,
    Flight,
    State,
}

impl SortKey {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "time" | "scheduled" | "schedule" => Some(Self::Scheduled),
            "delay" | "delayed" => Some(Self::Delay),
            "flight" | "number" => Some(Self::Flight),
            "state" | "status" => Some(Self::State),
            _ => None,
        }
    }
}

fn state_rank(flight: &Flight) -> u8 {
    match flight.state.as_str() {
        "cancelled" => 0,
        "delayed" => 1,
        "scheduled" => 2,
        "on_time" => 3,
        "early" => 4,
        _ => 5,
    }
}

#[derive(Debug, Clone, Default)]
pub struct FlightFilter {
    pub flight: Option<String>,
    pub airport: Option<String>,
    pub airline: Option<String>,
    pub terminal: Option<String>,
    pub state: Option<String>,
    pub min_delay: Option<i64>,
    pub sort: SortKey,
    /// 0 means no limit.
    pub limit: usize,
}

impl FlightFilter {
    pub fn apply(&self, flights: &[Flight]) -> Vec<Flight> {
        let flight_key = self
            .flight
            .as_deref()
            .map(canonical_flight_number)
            .filter(|value| !value.is_empty());
        let airport = self
            .airport
            .as_deref()
            .map(|value| value.trim().to_ascii_uppercase())
            .filter(|value| !value.is_empty());
        let airline = self
            .airline
            .as_deref()
            .map(|value| value.trim().to_ascii_uppercase())
            .filter(|value| !value.is_empty());
        let terminal = self
            .terminal
            .as_deref()
            .map(|value| value.trim().to_ascii_uppercase())
            .filter(|value| !value.is_empty());
        let state = self
            .state
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());

        let mut matched: Vec<Flight> = flights
            .iter()
            .filter(|flight| {
                if let Some(key) = &flight_key {
                    let primary = canonical_flight_number(&flight.flight_number);
                    let codeshare = flight
                        .codeshares
                        .iter()
                        .any(|number| canonical_flight_number(number) == *key);
                    if primary != *key && !codeshare {
                        return false;
                    }
                }
                if let Some(code) = &airport
                    && !flight
                        .origin
                        .iter()
                        .chain(flight.destination.iter())
                        .any(|candidate| candidate.eq_ignore_ascii_case(code))
                {
                    return false;
                }
                if let Some(code) = &airline
                    && !flight.airline.eq_ignore_ascii_case(code)
                {
                    return false;
                }
                if let Some(code) = &terminal
                    && flight
                        .terminal
                        .as_deref()
                        .map(|value| !value.eq_ignore_ascii_case(code))
                        .unwrap_or(true)
                {
                    return false;
                }
                if let Some(state) = &state {
                    let matches = match state.as_str() {
                        "all" | "active" => true,
                        "disrupted" | "disruption" => flight.cancelled || flight.state == "delayed",
                        "cancelled" | "canceled" => flight.cancelled,
                        "delayed" | "late" => flight.state == "delayed",
                        "on_time" | "ontime" => flight.state == "on_time",
                        "early" => flight.state == "early",
                        "scheduled" => flight.state == "scheduled",
                        other => flight.state == other || flight.status_type == other,
                    };
                    if !matches {
                        return false;
                    }
                }
                if let Some(minimum) = self.min_delay
                    && flight.delay_minutes.unwrap_or(i64::MIN) < minimum
                {
                    return false;
                }
                true
            })
            .cloned()
            .collect();

        matched.sort_by(|left, right| {
            let primary = match self.sort {
                SortKey::Scheduled => std::cmp::Ordering::Equal,
                SortKey::Delay => right
                    .delay_minutes
                    .unwrap_or(i64::MIN)
                    .cmp(&left.delay_minutes.unwrap_or(i64::MIN)),
                SortKey::Flight => left.flight_number.cmp(&right.flight_number),
                SortKey::State => state_rank(left).cmp(&state_rank(right)),
            };
            primary
                .then_with(|| left.scheduled_at.cmp(&right.scheduled_at))
                .then_with(|| left.flight_number.cmp(&right.flight_number))
        });
        if self.limit > 0 {
            matched.truncate(self.limit);
        }
        matched
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub kind: String,
    pub severity: String,
    pub flight_number: String,
    pub codeshares: Vec<String>,
    pub direction: String,
    pub route: Vec<String>,
    pub scheduled_at: String,
    pub delay_minutes: Option<i64>,
    pub estimated_at: Option<String>,
    pub actual_at: Option<String>,
    pub terminal: Option<String>,
    pub gate: Option<String>,
    pub baggage_belt: Option<String>,
    pub stand: Option<String>,
    pub message: String,
}

impl Alert {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.flight_number, self.direction, self.kind, self.scheduled_at
        )
    }

    pub fn fingerprint(&self) -> String {
        serde_json::json!([
            self.delay_minutes,
            self.estimated_at,
            self.actual_at,
            self.gate,
            self.terminal,
            self.baggage_belt,
            self.stand,
            self.message,
        ])
        .to_string()
    }
}

pub fn build_alerts(flights: &[Flight], min_delay: i64, include_cancelled: bool) -> Vec<Alert> {
    let mut alerts: Vec<Alert> = flights
        .iter()
        .filter_map(|flight| {
            if flight.cancelled {
                if !include_cancelled {
                    return None;
                }
                let route = flight.route().join("/");
                return Some(Alert {
                    kind: "cancelled".to_string(),
                    severity: "critical".to_string(),
                    flight_number: flight.flight_number.clone(),
                    codeshares: flight.codeshares.clone(),
                    direction: flight.direction.clone(),
                    route: flight.route(),
                    scheduled_at: flight.scheduled_at.clone(),
                    delay_minutes: None,
                    estimated_at: None,
                    actual_at: None,
                    terminal: flight.terminal.clone(),
                    gate: flight.gate.clone(),
                    baggage_belt: flight.baggage_belt.clone(),
                    stand: flight.stand.clone(),
                    message: format!(
                        "{} to/from {} cancelled (scheduled {})",
                        flight.flight_number, route, flight.scheduled_time
                    ),
                });
            }

            let delay = flight.delay_minutes?;
            if delay < min_delay {
                return None;
            }
            let route = flight.route().join("/");
            let event = flight
                .actual_at
                .as_deref()
                .or(flight.estimated_at.as_deref())
                .and_then(|time| time.get(11..16))
                .unwrap_or("--:--");
            Some(Alert {
                kind: "delayed".to_string(),
                severity: if delay >= 60 {
                    "critical".to_string()
                } else {
                    "warning".to_string()
                },
                flight_number: flight.flight_number.clone(),
                codeshares: flight.codeshares.clone(),
                direction: flight.direction.clone(),
                route: flight.route(),
                scheduled_at: flight.scheduled_at.clone(),
                delay_minutes: Some(delay),
                estimated_at: flight.estimated_at.clone(),
                actual_at: flight.actual_at.clone(),
                terminal: flight.terminal.clone(),
                gate: flight.gate.clone(),
                baggage_belt: flight.baggage_belt.clone(),
                stand: flight.stand.clone(),
                message: format!(
                    "{} to/from {} delayed {} min (scheduled {}, now {})",
                    flight.flight_number, route, delay, flight.scheduled_time, event
                ),
            })
        })
        .collect();

    alerts.sort_by(|left, right| {
        right
            .kind
            .eq("cancelled")
            .cmp(&left.kind.eq("cancelled"))
            .then_with(|| {
                right
                    .delay_minutes
                    .unwrap_or(0)
                    .cmp(&left.delay_minutes.unwrap_or(0))
            })
    });
    alerts
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HourStat {
    pub hour: String,
    pub flights: usize,
    pub delayed: usize,
    pub cancelled: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub total: usize,
    pub cancelled: usize,
    pub delayed: usize,
    pub on_time: usize,
    pub early: usize,
    pub scheduled: usize,
    pub tracked: usize,
    pub average_delay_minutes: Option<f64>,
    pub max_delay_minutes: Option<i64>,
    pub on_time_percent: Option<f64>,
    pub by_hour: Vec<HourStat>,
}

pub fn compute_stats(flights: &[Flight]) -> Stats {
    let mut stats = Stats {
        total: flights.len(),
        ..Stats::default()
    };
    let mut delay_sum = 0i64;
    let mut delay_count = 0usize;
    let mut hours: HashMap<String, HourStat> = HashMap::new();

    for flight in flights {
        match flight.state.as_str() {
            "cancelled" => stats.cancelled += 1,
            "delayed" => stats.delayed += 1,
            "on_time" => stats.on_time += 1,
            "early" => stats.early += 1,
            _ => stats.scheduled += 1,
        }
        if !flight.cancelled
            && let Some(delay) = flight.delay_minutes
        {
            delay_sum += delay;
            delay_count += 1;
            stats.max_delay_minutes = Some(
                stats
                    .max_delay_minutes
                    .map(|current| current.max(delay))
                    .unwrap_or(delay),
            );
        }

        let hour = flight
            .scheduled_time
            .get(0..2)
            .filter(|value| value.chars().all(|character| character.is_ascii_digit()))
            .map(|value| format!("{value}:00"))
            .unwrap_or_else(|| "--:--".to_string());
        let entry = hours.entry(hour.clone()).or_insert_with(|| HourStat {
            hour,
            ..HourStat::default()
        });
        entry.flights += 1;
        if flight.state == "delayed" {
            entry.delayed += 1;
        }
        if flight.cancelled {
            entry.cancelled += 1;
        }
    }

    stats.tracked = delay_count;
    if delay_count > 0 {
        stats.average_delay_minutes =
            Some((delay_sum as f64 / delay_count as f64 * 10.0).round() / 10.0);
        stats.on_time_percent = Some(
            ((stats.on_time + stats.early) as f64 / delay_count as f64 * 1000.0).round() / 10.0,
        );
    }
    stats.by_hour = hours.into_values().collect();
    stats
        .by_hour
        .sort_by(|left, right| left.hour.cmp(&right.hour));
    stats
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub kind: String,
    pub flight_number: String,
    pub scheduled_at: String,
    pub message: String,
}

pub fn flight_key(flight: &Flight) -> String {
    format!(
        "{}|{}|{}|{}",
        flight.flight_number, flight.direction, flight.date, flight.scheduled_time
    )
}

/// Compare two polls and report new disruptions and changed operational fields.
pub fn diff_snapshots(
    previous: &HashMap<String, Flight>,
    current: &[Flight],
    min_delay: i64,
) -> Vec<Change> {
    let mut changes = Vec::new();

    for flight in current {
        let key = flight_key(flight);
        let Some(old) = previous.get(&key) else {
            let disrupted = flight.cancelled
                || flight
                    .delay_minutes
                    .map(|delay| delay >= min_delay)
                    .unwrap_or(false);
            if disrupted {
                changes.push(Change {
                    kind: "new".to_string(),
                    flight_number: flight.flight_number.clone(),
                    scheduled_at: flight.scheduled_at.clone(),
                    message: format!(
                        "{} appeared as {} (scheduled {})",
                        flight.flight_number, flight.state, flight.scheduled_time
                    ),
                });
            }
            continue;
        };

        if flight.cancelled && !old.cancelled {
            changes.push(Change {
                kind: "cancelled".to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} cancelled (scheduled {})",
                    flight.flight_number, flight.scheduled_time
                ),
            });
        } else if flight.status != old.status {
            let kind = if flight.delay_minutes.unwrap_or(0) > old.delay_minutes.unwrap_or(0) {
                "delay"
            } else {
                "status"
            };
            changes.push(Change {
                kind: kind.to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} status {} -> {}",
                    flight.flight_number,
                    old.status.as_deref().unwrap_or("-"),
                    flight.status.as_deref().unwrap_or("-")
                ),
            });
        }

        if flight.gate != old.gate {
            changes.push(Change {
                kind: "gate".to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} gate {} -> {}",
                    flight.flight_number,
                    old.gate.as_deref().unwrap_or("-"),
                    flight.gate.as_deref().unwrap_or("-")
                ),
            });
        }
        if flight.terminal != old.terminal {
            changes.push(Change {
                kind: "terminal".to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} terminal {} -> {}",
                    flight.flight_number,
                    old.terminal.as_deref().unwrap_or("-"),
                    flight.terminal.as_deref().unwrap_or("-")
                ),
            });
        }
        if flight.baggage_belt != old.baggage_belt {
            changes.push(Change {
                kind: "baggage".to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} baggage belt {} -> {}",
                    flight.flight_number,
                    old.baggage_belt.as_deref().unwrap_or("-"),
                    flight.baggage_belt.as_deref().unwrap_or("-")
                ),
            });
        }
        if flight.arrival_hall != old.arrival_hall {
            changes.push(Change {
                kind: "hall".to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} arrival hall {} -> {}",
                    flight.flight_number,
                    old.arrival_hall.as_deref().unwrap_or("-"),
                    flight.arrival_hall.as_deref().unwrap_or("-")
                ),
            });
        }
        if flight.stand != old.stand {
            changes.push(Change {
                kind: "gate".to_string(),
                flight_number: flight.flight_number.clone(),
                scheduled_at: flight.scheduled_at.clone(),
                message: format!(
                    "{} stand {} -> {}",
                    flight.flight_number,
                    old.stand.as_deref().unwrap_or("-"),
                    flight.stand.as_deref().unwrap_or("-")
                ),
            });
        }
    }

    changes.sort_by(|left, right| left.scheduled_at.cmp(&right.scheduled_at));
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn parses_next_day_departure_with_delay() {
        let board_date = date(2026, 11, 20);
        let scheduled = board_date.and_hms_opt(23, 0, 0).unwrap();
        let parsed = parse_status(Some("Dep 00:00 (21/11/2026)"), board_date, scheduled);
        assert_eq!(parsed.status_type, "departed");
        assert_eq!(
            parsed.actual.unwrap().to_rfc3339(),
            "2026-11-21T00:00:00+08:00"
        );
    }

    #[test]
    fn builds_arrival_guidance_with_gate_and_belt() {
        assert_eq!(
            guidance_for(
                Direction::Arrival,
                Some("T1"),
                Some("30"),
                None,
                None,
                Some("12"),
                Some("A")
            )
            .as_deref(),
            Some("Arrive at Terminal 1, gate 30; baggage belt 12, arrival hall A")
        );
        assert_eq!(
            guidance_for(
                Direction::Arrival,
                Some("T1"),
                None,
                None,
                Some("S25"),
                None,
                None
            )
            .as_deref(),
            Some("Arrive at Terminal 1, stand S25")
        );
        assert_eq!(
            guidance_for(Direction::Arrival, None, None, None, None, Some("12"), None).as_deref(),
            Some("Arrive at HKG; baggage belt 12")
        );
    }

    #[test]
    fn parses_arrival_at_gate() {
        let board_date = date(2026, 11, 21);
        let scheduled = board_date.and_hms_opt(23, 15, 0).unwrap();
        let parsed = parse_status(Some("At gate 02:31 (21/11/2026)"), board_date, scheduled);
        assert_eq!(parsed.status_type, "arrived");
        assert_eq!(
            parsed.actual.unwrap().to_rfc3339(),
            "2026-11-21T02:31:00+08:00"
        );
    }

    #[test]
    fn rolls_over_midnight_without_explicit_date() {
        let board_date = date(2026, 11, 21);
        let scheduled = board_date.and_hms_opt(23, 55, 0).unwrap();
        let parsed = parse_status(Some("Est at 00:10"), board_date, scheduled);
        assert_eq!(parsed.status_type, "estimated");
        assert_eq!(parsed.estimated.unwrap().date_naive(), date(2026, 11, 22));
    }

    #[test]
    fn detects_cancellation() {
        let board_date = date(2026, 11, 21);
        let scheduled = board_date.and_hms_opt(10, 0, 0).unwrap();
        let parsed = parse_status(Some("Cancelled"), board_date, scheduled);
        assert!(parsed.cancelled);
        assert_eq!(parsed.status_type, "cancelled");
    }

    #[test]
    fn filters_by_codeshare_and_delay() {
        let flight = Flight {
            flight_number: "NH 814".to_string(),
            airline: "ANA".to_string(),
            airline_name: Some("All Nippon Airways".to_string()),
            codeshares: vec!["NZ 4144".to_string(), "UA 7932".to_string()],
            direction: "departure".to_string(),
            date: "2026-11-21".to_string(),
            scheduled_time: "00:15".to_string(),
            scheduled_at: "2026-11-21T00:15:00+08:00".to_string(),
            origin: vec![],
            destination: vec!["HND".to_string()],
            origin_details: vec![],
            destination_details: vec![],
            status: Some("Dep 00:35".to_string()),
            status_type: "departed".to_string(),
            status_published: true,
            estimated_at: None,
            actual_at: Some("2026-11-21T00:35:00+08:00".to_string()),
            delay_minutes: Some(20),
            delay_basis: Some("actual".to_string()),
            state: "delayed".to_string(),
            cancelled: false,
            terminal: Some("T1".to_string()),
            gate: Some("48".to_string()),
            check_in_aisle: Some("E".to_string()),
            baggage_belt: None,
            arrival_hall: None,
            stand: None,
            guidance: None,
            fetched_at: None,
        };

        let filter = FlightFilter {
            flight: Some("UA7932".to_string()),
            min_delay: Some(15),
            limit: 10,
            ..FlightFilter::default()
        };
        assert_eq!(filter.apply(std::slice::from_ref(&flight)).len(), 1);

        let filter = FlightFilter {
            flight: Some("UA7932".to_string()),
            min_delay: Some(30),
            limit: 10,
            ..FlightFilter::default()
        };
        assert!(filter.apply(&[flight]).is_empty());
    }

    #[test]
    fn parses_contract_fixture() {
        let body = include_str!("../tests/fixtures/departures.json");
        let board = board_from_body(body, date(2026, 11, 21), Direction::Departure, false)
            .expect("fixture should parse");
        assert_eq!(board.date, "2026-11-21");
        assert_eq!(board.flights.len(), 4);
        assert!(board.fetched_at.starts_with("20"));

        let cx = board
            .flights
            .iter()
            .find(|flight| flight.flight_number == "CX 251")
            .unwrap();
        assert_eq!(cx.delay_minutes, Some(60));
        assert_eq!(cx.delay_basis.as_deref(), Some("actual"));
        assert_eq!(cx.state, "delayed");
        assert_eq!(cx.terminal.as_deref(), Some("T1"));
        assert_eq!(
            cx.guidance.as_deref(),
            Some("Depart from Terminal 1, check-in aisle C, gate 30")
        );
        assert_eq!(cx.destination_details[0].city.as_deref(), Some("London"));
        assert_eq!(cx.airline_name.as_deref(), Some("Cathay Pacific"));

        let nh = board
            .flights
            .iter()
            .find(|flight| flight.flight_number == "NH 814")
            .unwrap();
        assert_eq!(nh.delay_minutes, Some(20));
        assert_eq!(nh.delay_basis.as_deref(), Some("estimated"));
        assert!(nh.codeshares.contains(&"UA 7932".to_string()));

        let cancelled = board
            .flights
            .iter()
            .find(|flight| flight.flight_number == "HX 307")
            .unwrap();
        assert!(cancelled.cancelled);
        assert_eq!(cancelled.state, "cancelled");
        assert_eq!(cancelled.guidance.as_deref(), Some("Flight cancelled"));
    }

    #[test]
    fn computes_stats_and_alerts() {
        let mut flights = Vec::new();
        for (number, delay, state) in [("CX 251", 60, "delayed"), ("CX 255", 0, "on_time")] {
            flights.push(Flight {
                flight_number: number.to_string(),
                airline: "CPA".to_string(),
                airline_name: Some("Cathay Pacific".to_string()),
                codeshares: vec![],
                direction: "departure".to_string(),
                date: "2026-11-21".to_string(),
                scheduled_time: "00:15".to_string(),
                scheduled_at: "2026-11-21T00:15:00+08:00".to_string(),
                origin: vec![],
                destination: vec!["LHR".to_string()],
                origin_details: vec![],
                destination_details: vec![],
                status: None,
                status_type: "departed".to_string(),
                status_published: false,
                estimated_at: None,
                actual_at: None,
                delay_minutes: Some(delay),
                delay_basis: Some("actual".to_string()),
                state: state.to_string(),
                cancelled: false,
                terminal: Some("T1".to_string()),
                gate: Some("3".to_string()),
                check_in_aisle: Some("C".to_string()),
                baggage_belt: None,
                arrival_hall: None,
                stand: None,
                guidance: None,
                fetched_at: None,
            });
        }
        let stats = compute_stats(&flights);
        assert_eq!(stats.total, 2);
        assert_eq!(stats.delayed, 1);
        assert_eq!(stats.average_delay_minutes, Some(30.0));
        assert_eq!(build_alerts(&flights, 15, true).len(), 1);
    }
}
