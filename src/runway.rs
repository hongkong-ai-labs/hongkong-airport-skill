//! Runway in use and airport wind, parsed from the Civil Aviation Department's
//! public ATIS page.
//!
//! CAD publishes the arrival and departure ATIS text on `hkatc.gov.hk`; each
//! bulletin names the runway in use and the current wind. This module parses
//! those bulletins and derives the wind components for the arrival runway. It
//! reports what the ATIS says and never guesses an unstated runway.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, TimeZone, Utc};
use serde::Serialize;
use std::time::Duration;

use crate::access::Source;
use crate::api::{hk_now, hk_offset};
use crate::store::{RunwayChange, RunwayDay};

pub const ATIS_URL: &str = "https://atis.cad.gov.hk/ATIS/ATISweb/atis.php";
pub const ATIS_SOURCE: &str = "https://www.hkatc.gov.hk/";
pub const AIP_SOURCE: &str = "https://www.ais.gov.hk/eaip/";
pub const HKO_WIND_URL: &str =
    "https://data.weather.gov.hk/weatherAPI/hko_data/regional-weather/latest_10min_wind.csv";

const HKO_STATION: &str = "Chek Lap Kok";
const NOTE: &str = "Runway in use comes from the official CAD ATIS; arrivals and departures can use different runways. Wind components are computed from the reported wind for the arrival runway, not measured at the threshold. When the ATIS page is unavailable, only the HKO 10-minute wind is reported and no runway is claimed.";

#[derive(Debug, Clone, Serialize)]
pub struct WindReport {
    pub direction_degrees: Option<u16>,
    pub speed_knots: u16,
    pub headwind_knots: Option<i16>,
    pub crosswind_knots: Option<i16>,
    pub crosswind_from: Option<&'static str>,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AtisReport {
    pub kind: &'static str,
    pub letter: String,
    pub issued_at: String,
    pub runway: Option<String>,
    pub direction: Option<&'static str>,
    pub wind: Option<WindReport>,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindObservation {
    pub station: String,
    pub direction_degrees: Option<u16>,
    pub direction_compass: Option<String>,
    pub speed_knots: u16,
    pub gust_knots: Option<u16>,
    pub observed_at: String,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunwayStatus {
    pub mode: &'static str,
    pub fetched_at: String,
    pub atis_available: bool,
    pub arrival: Option<AtisReport>,
    pub departure: Option<AtisReport>,
    /// Only set when the ATIS is unavailable and the HKO wind is used instead.
    pub wind: Option<WindObservation>,
    /// Runway the wind favours under the AIP preferential-use rule.
    pub wind_favours: Option<&'static str>,
    pub error: Option<String>,
    pub note: &'static str,
    pub source: Source,
}

pub async fn fetch() -> Result<RunwayStatus> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("building the airport weather HTTP client")?;
    match fetch_atis(&http, Utc::now()).await {
        Ok(status) => Ok(status),
        Err(atis_error) => wind_only(&http, atis_error).await,
    }
}

async fn fetch_atis(http: &reqwest::Client, now: DateTime<Utc>) -> Result<RunwayStatus> {
    let response = http
        .get(ATIS_URL)
        .send()
        .await
        .context("fetching the CAD ATIS page")?;
    let status = response.status();
    let body = response.text().await.context("reading the CAD ATIS page")?;
    if !status.is_success() {
        return Err(anyhow!("CAD ATIS returned HTTP {}", status.as_u16()));
    }
    parse_atis(&body, now)
}

async fn wind_only(http: &reqwest::Client, atis_error: anyhow::Error) -> Result<RunwayStatus> {
    let wind = match fetch_hko_wind(http).await {
        Ok(wind) => wind,
        Err(wind_error) => {
            return Err(anyhow!(
                "CAD ATIS unavailable ({atis_error:#}); HKO wind unavailable ({wind_error:#})"
            ));
        }
    };
    let wind_favours = wind
        .direction_degrees
        .and_then(|direction| favoured_runway(direction, wind.speed_knots));
    Ok(RunwayStatus {
        mode: "runway_status",
        fetched_at: hk_now().to_rfc3339_opts(SecondsFormat::Secs, false),
        atis_available: false,
        arrival: None,
        departure: None,
        wind: Some(wind),
        wind_favours,
        error: Some(format!("{atis_error:#}")),
        note: NOTE,
        source: Source {
            name: "Civil Aviation Department ATIS".to_string(),
            url: ATIS_SOURCE.to_string(),
        },
    })
}

async fn fetch_hko_wind(http: &reqwest::Client) -> Result<WindObservation> {
    let response = http
        .get(HKO_WIND_URL)
        .send()
        .await
        .context("fetching the HKO 10-minute wind file")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("reading the HKO 10-minute wind file")?;
    if !status.is_success() {
        return Err(anyhow!("HKO wind file returned HTTP {}", status.as_u16()));
    }
    parse_hko_wind(&body).ok_or_else(|| anyhow!("no {HKO_STATION} row in the HKO wind file"))
}

fn parse_hko_wind(text: &str) -> Option<WindObservation> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(text.as_bytes());
    for record in reader.records().flatten() {
        if record.get(1).map(str::trim) != Some(HKO_STATION) {
            continue;
        }
        let observed_at = parse_hko_datetime(record.get(0).map(str::trim).unwrap_or_default())?;
        let compass = record
            .get(2)
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("N/A"));
        let speed_kmh = record
            .get(3)
            .and_then(|value| value.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        let gust_kmh = record
            .get(4)
            .and_then(|value| value.trim().parse::<f64>().ok());
        return Some(WindObservation {
            station: HKO_STATION.to_string(),
            direction_degrees: compass.and_then(compass_degrees),
            direction_compass: compass.map(str::to_string),
            speed_knots: kmh_to_knots(speed_kmh),
            gust_knots: gust_kmh.map(kmh_to_knots),
            observed_at,
            source: Source {
                name: "Hong Kong Observatory 10-minute wind (Chek Lap Kok)".to_string(),
                url: HKO_WIND_URL.to_string(),
            },
        });
    }
    None
}

fn parse_hko_datetime(value: &str) -> Option<String> {
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d%H%M").ok()?;
    Some(
        hk_offset()
            .from_local_datetime(&naive)
            .single()?
            .to_rfc3339_opts(SecondsFormat::Secs, false),
    )
}

fn kmh_to_knots(speed: f64) -> u16 {
    (speed * 0.539_956_8).round() as u16
}

fn compass_degrees(value: &str) -> Option<u16> {
    match value.trim().to_ascii_lowercase().as_str() {
        "n" | "north" => Some(0),
        "ne" | "northeast" => Some(45),
        "e" | "east" => Some(90),
        "se" | "southeast" => Some(135),
        "s" | "south" => Some(180),
        "sw" | "southwest" => Some(225),
        "w" | "west" => Some(270),
        "nw" | "northwest" => Some(315),
        _ => None,
    }
}

fn parse_atis(html: &str, now: DateTime<Utc>) -> Result<RunwayStatus> {
    let lines = strip_html(html);
    let mut arrival = None;
    let mut departure = None;
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        let kind = if line.starts_with("VHHH ARR ATIS") {
            Some("arrival")
        } else if line.starts_with("VHHH DEP ATIS") {
            Some("departure")
        } else {
            None
        };
        let Some(kind) = kind else {
            index += 1;
            continue;
        };
        let mut block = Vec::new();
        index += 1;
        while index < lines.len() {
            let next = &lines[index];
            if next.starts_with("VHHH ")
                || next.starts_with("Remarks:")
                || next.starts_with("ACKNOWLEDGE")
            {
                break;
            }
            block.push(next.clone());
            index += 1;
        }
        match kind {
            "arrival" => arrival = parse_block(kind, line, &block, now),
            _ => departure = parse_block(kind, line, &block, now),
        }
    }
    if arrival.is_none() && departure.is_none() {
        return Err(anyhow!(
            "no ATIS bulletin found on the CAD page; the page layout may have changed"
        ));
    }
    Ok(RunwayStatus {
        mode: "runway_status",
        fetched_at: hk_now().to_rfc3339_opts(SecondsFormat::Secs, false),
        atis_available: true,
        arrival,
        departure,
        wind: None,
        wind_favours: None,
        error: None,
        note: NOTE,
        source: Source {
            name: "Civil Aviation Department ATIS".to_string(),
            url: ATIS_SOURCE.to_string(),
        },
    })
}

fn parse_block(
    kind: &'static str,
    header: &str,
    block: &[String],
    now: DateTime<Utc>,
) -> Option<AtisReport> {
    let tokens = header.split_whitespace().collect::<Vec<_>>();
    // VHHH ARR ATIS D 1609Z.
    let letter = tokens.get(3)?.trim_matches('.').to_string();
    let issued_at = issued_at(tokens.get(4)?.trim_matches('.'), now)?;
    let runway = block.iter().find_map(|line| {
        let mut parts = line.split_whitespace();
        while let Some(part) = parts.next() {
            if part == "RWY" {
                return parts.next().map(|runway| {
                    runway
                        .trim_matches(|c: char| c == '.' || c == ',')
                        .to_string()
                });
            }
        }
        None
    });
    let text = block.join(" ");
    let wind = parse_wind(&text).map(|wind| {
        let components = wind
            .direction_degrees
            .zip(runway.as_deref())
            .and_then(|(direction, runway)| components(direction, wind.speed_knots, runway));
        WindReport {
            direction_degrees: wind.direction_degrees,
            speed_knots: wind.speed_knots,
            headwind_knots: components.map(|(headwind, _, _)| headwind),
            crosswind_knots: components.map(|(_, crosswind, _)| crosswind),
            crosswind_from: components.and_then(|(_, _, side)| side),
            raw: wind.raw,
        }
    });
    Some(AtisReport {
        kind,
        letter,
        issued_at,
        direction: runway.as_deref().and_then(|runway| match kind {
            "arrival" => arrival_direction(runway),
            _ => departure_direction(runway),
        }),
        runway,
        wind,
        raw: text,
    })
}

struct RawWind {
    direction_degrees: Option<u16>,
    speed_knots: u16,
    raw: String,
}

fn parse_wind(text: &str) -> Option<RawWind> {
    for token in text.split_whitespace() {
        let token = token.trim_matches(|c: char| c == '.' || c == ',');
        if let Some((direction, rest)) = token.split_once('/') {
            let Ok(direction) = direction.parse::<u16>() else {
                continue;
            };
            let Some(speed) = rest
                .strip_suffix("KT")
                .and_then(|speed| speed.parse::<u16>().ok())
            else {
                continue;
            };
            return Some(RawWind {
                direction_degrees: Some(direction % 360),
                speed_knots: speed,
                raw: token.to_string(),
            });
        }
        if let Some(speed) = token
            .strip_prefix("VRB")
            .and_then(|rest| rest.strip_suffix("KT"))
            .and_then(|speed| speed.parse::<u16>().ok())
        {
            return Some(RawWind {
                direction_degrees: None,
                speed_knots: speed,
                raw: token.to_string(),
            });
        }
    }
    None
}

fn issued_at(token: &str, now: DateTime<Utc>) -> Option<String> {
    let token = token.trim_end_matches('Z');
    if token.len() != 4 || !token.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour = token[..2].parse::<u32>().ok()?;
    let minute = token[2..].parse::<u32>().ok()?;
    let naive = now.date_naive().and_hms_opt(hour, minute, 0)?;
    let mut at = Utc.from_utc_datetime(&naive);
    if at - now > ChronoDuration::hours(2) {
        at -= ChronoDuration::days(1);
    }
    Some(
        at.with_timezone(&hk_offset())
            .to_rfc3339_opts(SecondsFormat::Secs, false),
    )
}

fn runway_heading(runway: &str) -> Option<u16> {
    let digits = runway
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    if digits.len() != 2 {
        return None;
    }
    let number = digits.parse::<u16>().ok()?;
    Some(number * 10)
}

fn arrival_direction(runway: &str) -> Option<&'static str> {
    let heading = runway_heading(runway)?;
    if heading < 180 {
        Some("eastbound — approaches from the west")
    } else {
        Some("westbound — approaches from the east")
    }
}

fn departure_direction(runway: &str) -> Option<&'static str> {
    let heading = runway_heading(runway)?;
    if heading < 180 {
        Some("eastbound — climb-out to the east")
    } else {
        Some("westbound — climb-out to the west")
    }
}

/// Headwind (positive) or tailwind (negative), crosswind magnitude and the
/// side the wind comes from, in whole knots.
fn components(
    direction: u16,
    speed: u16,
    runway: &str,
) -> Option<(i16, i16, Option<&'static str>)> {
    let heading = f64::from(runway_heading(runway)?);
    let relative = (f64::from(direction) - heading + 540.0) % 360.0 - 180.0;
    let speed = f64::from(speed);
    let headwind = (speed * relative.to_radians().cos()).round() as i16;
    let crosswind = (speed * relative.to_radians().sin()).round().abs() as i16;
    let side = if crosswind == 0 {
        None
    } else if relative >= 0.0 {
        Some("right")
    } else {
        Some("left")
    };
    Some((headwind, crosswind, side))
}

fn favoured_runway(direction: u16, speed: u16) -> Option<&'static str> {
    // AIP VHHH AD 2.22: runway 07 is in use whenever the tailwind is 5 kt or less.
    let (headwind, _, _) = components(direction, speed, "07")?;
    Some(if headwind >= -5 { "07" } else { "25" })
}

pub fn history_table(changes: &[RunwayChange]) -> String {
    if changes.is_empty() {
        return "No runway switches recorded yet. Run `hkia watch` to observe them.\n".to_string();
    }
    let mut output = format!(
        "Observed runway switches  |  {} row(s)  |  local store\n",
        changes.len()
    );
    for change in changes {
        let stamp = change
            .observed_at
            .get(0..16)
            .unwrap_or(&change.observed_at)
            .replace('T', " ");
        output.push_str(&format!(
            "{stamp}  arrivals {}  departures {}\n",
            change.arrival, change.departure
        ));
    }
    output
}

pub fn daily_table(days: &[RunwayDay]) -> String {
    if days.is_empty() {
        return "No runway switches recorded yet. Run `hkia watch` to observe them.\n".to_string();
    }
    let mut output = format!(
        "Observed runway switches by day  |  {} day(s)  |  local store\n",
        days.len()
    );
    output.push_str("DATE        SWITCHES  FIRST        LAST\n");
    for day in days {
        let first = format!("{}/{}", day.first_arrival, day.first_departure);
        let last = format!("{}/{}", day.last_arrival, day.last_departure);
        output.push_str(&format!(
            "{:<11} {:<9} {:<12} {}\n",
            day.date, day.switches, first, last
        ));
    }
    output
}

pub fn daily_csv(days: &[RunwayDay], header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "date",
            "switches",
            "first_arrival",
            "first_departure",
            "last_arrival",
            "last_departure",
        ])?;
    }
    for day in days {
        writer.write_record([
            day.date.clone(),
            day.switches.to_string(),
            day.first_arrival.clone(),
            day.first_departure.clone(),
            day.last_arrival.clone(),
            day.last_departure.clone(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

pub fn history_csv(changes: &[RunwayChange], header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record(["observed_at", "arrival", "departure"])?;
    }
    for change in changes {
        writer.write_record([
            change.observed_at.clone(),
            change.arrival.clone(),
            change.departure.clone(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

fn observation_label(wind: &WindObservation) -> String {
    if wind.speed_knots == 0 {
        return "calm".to_string();
    }
    match (wind.direction_degrees, wind.direction_compass.as_deref()) {
        (Some(direction), Some(compass)) => {
            format!("{compass} ({direction:03}°)/{:02} kt", wind.speed_knots)
        }
        (Some(direction), None) => format!("{direction:03}°/{:02} kt", wind.speed_knots),
        (None, Some(compass)) => format!("{compass}/{:02} kt", wind.speed_knots),
        (None, None) => format!("variable/{:02} kt", wind.speed_knots),
    }
}

fn wind_label(wind: &WindReport) -> String {
    match wind.direction_degrees {
        Some(direction) => format!("{direction:03}°/{:02} kt", wind.speed_knots),
        None => format!("variable/{:02} kt", wind.speed_knots),
    }
}

fn wind_line(report: &AtisReport) -> Option<String> {
    let wind = report.wind.as_ref()?;
    let runway = report.runway.as_deref().unwrap_or("runway");
    let mut line = format!("Wind {} on {}", wind_label(wind), runway);
    if let Some(headwind) = wind.headwind_knots {
        line.push_str(&format!(
            ": {} kt {}",
            headwind.abs(),
            if headwind >= 0 {
                "headwind"
            } else {
                "tailwind"
            }
        ));
    }
    if let Some(crosswind) = wind.crosswind_knots
        && crosswind > 0
    {
        line.push_str(&format!(
            ", {crosswind} kt crosswind from the {}",
            wind.crosswind_from.unwrap_or("side")
        ));
    }
    Some(line)
}

fn time_part(timestamp: &str) -> &str {
    timestamp.get(11..16).unwrap_or(timestamp)
}

pub fn runway_table(status: &RunwayStatus) -> String {
    if !status.atis_available {
        return wind_only_table(status);
    }
    let label = |report: &Option<AtisReport>| {
        report
            .as_ref()
            .and_then(|report| report.runway.clone())
            .unwrap_or_else(|| "unknown".to_string())
    };
    let mut output = format!(
        "HKG runway in use  |  arrivals {}  |  departures {}  |  fetched {} HKT\n",
        label(&status.arrival),
        label(&status.departure),
        time_part(&status.fetched_at)
    );
    for report in [&status.arrival, &status.departure].into_iter().flatten() {
        let title = if report.kind == "arrival" {
            "Arrival"
        } else {
            "Departure"
        };
        output.push_str(&format!(
            "{title}: RWY {} — {}\n",
            report.runway.as_deref().unwrap_or("unknown"),
            report.direction.unwrap_or("direction not published")
        ));
        if let Some(line) = wind_line(report) {
            output.push_str(&format!("{line}\n"));
        }
        output.push_str(&format!(
            "ATIS {} issued {} HKT\n",
            report.letter,
            time_part(&report.issued_at)
        ));
    }
    output.push_str(&format!("\n{}\n", status.note));
    output.push_str(&format!(
        "Source: {} ({}); AIP: {}.\n",
        status.source.name, status.source.url, AIP_SOURCE
    ));
    output
}

fn wind_only_table(status: &RunwayStatus) -> String {
    let mut output = format!(
        "HKG runway  |  ATIS unavailable — runway not confirmed  |  fetched {} HKT\n",
        time_part(&status.fetched_at)
    );
    if let Some(wind) = &status.wind {
        let gust = wind
            .gust_knots
            .map(|gust| format!(" (gust {gust} kt)"))
            .unwrap_or_default();
        output.push_str(&format!(
            "HKO wind at {}: {}{gust}, observed {} HKT\n",
            wind.station,
            observation_label(wind),
            time_part(&wind.observed_at)
        ));
        match status.wind_favours {
            Some(runway) => output.push_str(&format!(
                "Wind favours RWY {runway} under the AIP rule (runway 07 when the tailwind is 5 kt or less) — an indication only, not the runway in use.\n"
            )),
            None => output.push_str("Wind is calm or variable; no runway is indicated.\n"),
        }
    }
    if let Some(error) = &status.error {
        output.push_str(&format!("ATIS error: {error}\n"));
    }
    output.push_str(&format!("\n{}\n", status.note));
    output.push_str(&format!(
        "Source: HKO 10-minute wind ({HKO_WIND_URL}); AIP: {AIP_SOURCE}.\n"
    ));
    output
}

pub fn runway_csv(status: &RunwayStatus, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "kind",
            "runway",
            "direction",
            "wind",
            "headwind_knots",
            "crosswind_knots",
            "crosswind_from",
            "atis_letter",
            "issued_at",
            "wind_favours",
        ])?;
    }
    if !status.atis_available {
        if let Some(wind) = &status.wind {
            writer.write_record([
                "wind".to_string(),
                String::new(),
                String::new(),
                observation_label(wind),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                wind.observed_at.clone(),
                status.wind_favours.unwrap_or_default().to_string(),
            ])?;
        }
        let bytes = writer
            .into_inner()
            .map_err(|error| anyhow!("writing CSV: {error}"))?;
        return String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"));
    }
    for report in [&status.arrival, &status.departure].into_iter().flatten() {
        writer.write_record([
            report.kind.to_string(),
            report.runway.clone().unwrap_or_default(),
            report.direction.unwrap_or_default().to_string(),
            report.wind.as_ref().map(wind_label).unwrap_or_default(),
            report
                .wind
                .as_ref()
                .and_then(|wind| wind.headwind_knots)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            report
                .wind
                .as_ref()
                .and_then(|wind| wind.crosswind_knots)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            report
                .wind
                .as_ref()
                .and_then(|wind| wind.crosswind_from)
                .unwrap_or_default()
                .to_string(),
            report.letter.clone(),
            report.issued_at.clone(),
            String::new(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

fn strip_html(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut text = String::with_capacity(html.len());
    let mut index = 0;
    while index < html.len() {
        if lower[index..].starts_with("<script") {
            match lower[index..].find("</script>") {
                Some(end) => {
                    index += end + "</script>".len();
                    continue;
                }
                None => break,
            }
        }
        if lower[index..].starts_with("<style") {
            match lower[index..].find("</style>") {
                Some(end) => {
                    index += end + "</style>".len();
                    continue;
                }
                None => break,
            }
        }
        let Some(character) = html[index..].chars().next() else {
            break;
        };
        if character == '<' {
            match lower[index..].find('>') {
                Some(end) => {
                    index += end + 1;
                    text.push('\n');
                    continue;
                }
                None => break,
            }
        }
        text.push(character);
        index += character.len_utf8();
    }
    let text = text
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    text.lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        r#"
        <html><body>
        <h1>Automatic Terminal Information Service (ATIS)</h1>
        <script>var ignored = "<div>";</script>
        <div>ARRIVAL</div>
        <div>VHHH ARR ATIS D 1609Z.</div>
        <div>ARRIVALS, RWY 07C.</div>
        <div>EXP ILS APCH, RWY 07C.</div>
        <div>WIND 040/07KT VIS 10KM CLD FEW 4500FT T21 DP09 QNH 1024HPA=</div>
        <div>ACKNOWLEDGE INFO D ON FIRST CTC WITH APP.</div>
        <div>DEPARTURE</div>
        <div>VHHH DEP ATIS U 1610Z.</div>
        <div>DEPARTURES, RWY 07R.</div>
        <div>WIND RWY 07R VRB05KT VIS 10KM CLD FEW 4500FT T21 DP09 QNH 1024HPA=</div>
        <div>ACKNOWLEDGE INFO U ON FIRST CTC WITH DELIVERY .</div>
        <div>Remarks:</div>
        </body></html>
        "#
        .to_string()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 22, 16, 15, 0).unwrap()
    }

    #[test]
    fn parses_arrival_and_departure_atis() {
        let status = parse_atis(&fixture(), now()).unwrap();
        assert_eq!(status.mode, "runway_status");
        let arrival = status.arrival.unwrap();
        assert_eq!(arrival.kind, "arrival");
        assert_eq!(arrival.letter, "D");
        assert_eq!(arrival.runway.as_deref(), Some("07C"));
        assert_eq!(arrival.issued_at, "2026-11-23T00:09:00+08:00");
        assert_eq!(
            arrival.direction,
            Some("eastbound — approaches from the west")
        );
        let wind = arrival.wind.unwrap();
        assert_eq!(wind.direction_degrees, Some(40));
        assert_eq!(wind.speed_knots, 7);
        assert_eq!(wind.headwind_knots, Some(6));
        assert_eq!(wind.crosswind_knots, Some(3));
        assert_eq!(wind.crosswind_from, Some("left"));

        let departure = status.departure.unwrap();
        assert_eq!(departure.letter, "U");
        assert_eq!(departure.runway.as_deref(), Some("07R"));
        let wind = departure.wind.unwrap();
        assert_eq!(wind.direction_degrees, None);
        assert_eq!(wind.speed_knots, 5);
        assert_eq!(wind.headwind_knots, None);
    }

    #[test]
    fn reports_tailwind_when_the_wind_is_behind() {
        let (headwind, crosswind, side) = components(250, 10, "07L").unwrap();
        assert_eq!(headwind, -10);
        assert_eq!(crosswind, 0);
        assert_eq!(side, None);

        let (headwind, _, _) = components(70, 12, "07C").unwrap();
        assert_eq!(headwind, 12);
    }

    const HKO_FIXTURE: &str = "Date time,Automatic Weather Station,10-Minute Mean Wind Direction(Compass points),10-Minute Mean Speed(km/hour),10-Minute Maximum Gust(km/hour)\n202611220100,Central Pier,East,30,42\n202611220100,Chek Lap Kok,Northeast,12,16\n";

    fn wind_only_status() -> RunwayStatus {
        let wind = parse_hko_wind(HKO_FIXTURE).unwrap();
        RunwayStatus {
            mode: "runway_status",
            fetched_at: "2026-11-22T01:10:00+08:00".to_string(),
            atis_available: false,
            arrival: None,
            departure: None,
            wind_favours: wind
                .direction_degrees
                .and_then(|direction| favoured_runway(direction, wind.speed_knots)),
            wind: Some(wind),
            error: Some("CAD ATIS returned HTTP 503".to_string()),
            note: NOTE,
            source: Source {
                name: "Civil Aviation Department ATIS".to_string(),
                url: ATIS_SOURCE.to_string(),
            },
        }
    }

    #[test]
    fn parses_the_hko_airport_wind() {
        let wind = parse_hko_wind(HKO_FIXTURE).unwrap();
        assert_eq!(wind.station, "Chek Lap Kok");
        assert_eq!(wind.direction_compass.as_deref(), Some("Northeast"));
        assert_eq!(wind.direction_degrees, Some(45));
        assert_eq!(wind.speed_knots, 6);
        assert_eq!(wind.gust_knots, Some(9));
        assert_eq!(wind.observed_at, "2026-11-22T01:00:00+08:00");
        assert!(parse_hko_wind("Date time,Station\n").is_none());
    }

    #[test]
    fn wind_favour_follows_the_aip_rule() {
        assert_eq!(favoured_runway(40, 7), Some("07"));
        assert_eq!(favoured_runway(250, 3), Some("07"));
        assert_eq!(favoured_runway(220, 20), Some("25"));
        assert_eq!(favoured_runway(250, 10), Some("25"));
    }

    #[test]
    fn wind_only_output_does_not_claim_a_runway() {
        let status = wind_only_status();
        let table = runway_table(&status);
        assert!(table.contains("ATIS unavailable"));
        assert!(table.contains("not confirmed"));
        assert!(table.contains("favours RWY 07"));
        assert!(table.contains("Northeast"));
        assert!(table.contains("HTTP 503"));

        let csv = runway_csv(&status, true).unwrap();
        assert!(csv.contains("wind,"));
        assert!(csv.contains("07"), "csv: {csv}");
        assert!(runway_csv(&status, false).unwrap().lines().count() == 1);
    }

    #[test]
    fn renders_history_table_and_csv() {
        let changes = vec![
            RunwayChange {
                observed_at: "2026-11-22T01:00:00+08:00".to_string(),
                arrival: "07C".to_string(),
                departure: "07R".to_string(),
            },
            RunwayChange {
                observed_at: "2026-11-22T04:30:00+08:00".to_string(),
                arrival: "25R".to_string(),
                departure: "25C".to_string(),
            },
        ];
        let table = history_table(&changes);
        assert!(table.contains("2 row(s)"));
        assert!(table.contains("2026-11-22 04:30  arrivals 25R"));
        assert!(history_table(&[]).contains("No runway switches"));
        let csv = history_csv(&changes, true).unwrap();
        assert!(csv.starts_with("observed_at,arrival,departure"));
        assert!(csv.contains("25R,25C"));
        assert_eq!(history_csv(&changes, false).unwrap().lines().count(), 2);

        let days = vec![
            RunwayDay {
                date: "2026-11-22".to_string(),
                switches: 2,
                first_arrival: "07C".to_string(),
                first_departure: "07R".to_string(),
                last_arrival: "25R".to_string(),
                last_departure: "25C".to_string(),
            },
            RunwayDay {
                date: "2026-11-23".to_string(),
                switches: 1,
                first_arrival: "07L".to_string(),
                first_departure: "07C".to_string(),
                last_arrival: "07L".to_string(),
                last_departure: "07C".to_string(),
            },
        ];
        let table = daily_table(&days);
        assert!(table.contains("2 day(s)"));
        assert!(table.contains("2026-11-22  2"));
        assert!(table.contains("25R/25C"));
        assert!(daily_table(&[]).contains("No runway switches"));
        let csv = daily_csv(&days, true).unwrap();
        assert!(csv.starts_with("date,switches,first_arrival"));
        assert!(csv.contains("2026-11-23,1,07L,07C,07L,07C"));
        assert_eq!(daily_csv(&days, false).unwrap().lines().count(), 2);
    }

    #[test]
    fn rejects_a_page_without_atis_blocks() {
        assert!(parse_atis("<html><body>maintenance</body></html>", now()).is_err());
    }

    #[test]
    fn renders_table_and_csv() {
        let status = parse_atis(&fixture(), now()).unwrap();
        let table = runway_table(&status);
        assert!(table.contains("arrivals 07C"));
        assert!(table.contains("departures 07R"));
        assert!(table.contains("6 kt headwind"));
        let csv = runway_csv(&status, true).unwrap();
        assert!(csv.starts_with("kind,runway,direction,wind"));
        assert!(csv.contains("arrival,07C"));
        assert!(csv.contains("departure,07R"));
    }
}
