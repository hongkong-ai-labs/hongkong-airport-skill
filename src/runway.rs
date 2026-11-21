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

pub const ATIS_URL: &str = "https://atis.cad.gov.hk/ATIS/ATISweb/atis.php";
pub const ATIS_SOURCE: &str = "https://www.hkatc.gov.hk/";
pub const AIP_SOURCE: &str = "https://www.ais.gov.hk/eaip/";

const NOTE: &str = "Runway in use comes from the official CAD ATIS; arrivals and departures can use different runways. Wind components are computed from the reported wind for the arrival runway, not measured at the threshold.";

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
pub struct RunwayStatus {
    pub mode: &'static str,
    pub fetched_at: String,
    pub arrival: Option<AtisReport>,
    pub departure: Option<AtisReport>,
    pub note: &'static str,
    pub source: Source,
}

pub async fn fetch() -> Result<RunwayStatus> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("building the ATIS HTTP client")?;
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
    parse_atis(&body, Utc::now())
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
        arrival,
        departure,
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

pub fn runway_table(status: &RunwayStatus) -> String {
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
        status.fetched_at.get(11..16).unwrap_or(&status.fetched_at)
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
            report.issued_at.get(11..16).unwrap_or(&report.issued_at)
        ));
    }
    output.push_str(&format!("\n{}\n", status.note));
    output.push_str(&format!(
        "Source: {} ({}); AIP: {}.\n",
        status.source.name, status.source.url, AIP_SOURCE
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
        ])?;
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
