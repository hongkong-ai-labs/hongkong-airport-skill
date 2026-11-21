//! Plain-text and CSV rendering. JSON output is produced directly in the CLI.

use anyhow::{Result, anyhow};

use crate::analytics::{GroupReport, RangeReport, group};
use crate::api::{Alert, Change, Flight, Stats};
use crate::reference;
use crate::store::StoredEvent;
use crate::weather::{WeatherReport, WeatherWarning};

fn pad(value: &str, width: usize) -> String {
    format!("{value:<width$}", width = width)
}

fn delay_label(delay: Option<i64>) -> String {
    match delay {
        Some(delay) if delay > 0 => format!("+{delay}m"),
        Some(delay) if delay < 0 => format!("{delay}m"),
        Some(_) => "0m".to_string(),
        None => "-".to_string(),
    }
}

fn state_style(state: &str, delay: Option<i64>) -> Option<&'static str> {
    match state {
        "cancelled" => Some("\x1b[31m"),
        "delayed" => Some("\x1b[33m"),
        "early" => Some("\x1b[32m"),
        "on_time" => Some(if delay.unwrap_or(0) > 0 {
            "\x1b[33m"
        } else {
            "\x1b[32m"
        }),
        _ => None,
    }
}

fn paint(value: &str, code: Option<&'static str>, enabled: bool) -> String {
    match (code, enabled) {
        (Some(code), true) => format!("{code}{value}\x1b[0m"),
        _ => value.to_string(),
    }
}

fn route_label(flight: &Flight) -> String {
    let codes = flight.route();
    if codes.is_empty() {
        "-".to_string()
    } else {
        codes
            .iter()
            .map(|code| reference::airport_label(code))
            .collect::<Vec<_>>()
            .join("/")
    }
}

pub fn flights_table(
    flights: &[Flight],
    direction: &str,
    date: &str,
    last_updated: Option<&str>,
    total_on_board: usize,
    color: bool,
) -> String {
    let headers = [
        "TIME", "FLIGHT", "ROUTE", "STATUS", "DELAY", "GATE", "TERM", "EXTRA",
    ];

    let rows: Vec<Vec<String>> = flights
        .iter()
        .map(|flight| {
            let extra = if flight.direction == "arrival" {
                flight
                    .baggage_belt
                    .as_deref()
                    .map(|belt| format!("belt {belt}"))
                    .or_else(|| {
                        flight
                            .arrival_hall
                            .as_deref()
                            .map(|hall| format!("hall {hall}"))
                    })
                    .unwrap_or_else(|| "-".to_string())
            } else {
                flight
                    .check_in_aisle
                    .as_deref()
                    .map(|aisle| format!("aisle {aisle}"))
                    .unwrap_or_else(|| "-".to_string())
            };

            vec![
                flight.scheduled_time.clone(),
                flight.flight_number.clone(),
                route_label(flight),
                flight
                    .status
                    .clone()
                    .unwrap_or_else(|| "Scheduled".to_string()),
                delay_label(flight.delay_minutes),
                flight
                    .gate
                    .clone()
                    .or_else(|| flight.stand.clone())
                    .unwrap_or_else(|| "-".to_string()),
                flight.terminal.clone().unwrap_or_else(|| "-".to_string()),
                extra,
            ]
        })
        .collect();

    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let header_line = headers
        .iter()
        .enumerate()
        .map(|(index, header)| pad(header, widths[index]))
        .collect::<Vec<_>>()
        .join("  ");
    let separator = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>()
        .join("  ");

    let mut output = String::new();
    output.push_str(&format!(
        "HKG {direction}  |  {date}  |  {} flight(s)\n",
        flights.len()
    ));
    output.push_str(header_line.trim_end());
    output.push('\n');
    output.push_str(&separator);
    output.push('\n');
    for (index, row) in rows.iter().enumerate() {
        let flight = &flights[index];
        let mut cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(index, cell)| pad(cell, widths[index]))
            .collect();
        if color {
            let code = state_style(&flight.state, flight.delay_minutes);
            cells[3] = paint(&cells[3], code, color);
            cells[4] = paint(&cells[4], code, color);
        }
        let line = cells.join("  ");
        output.push_str(line.trim_end());
        output.push('\n');
    }
    if flights.is_empty() {
        output.push_str("(no matching flights)\n");
    }
    output.push_str(&format!(
        "\nUpdated {}{}  |  showing {}/{} on the board\n",
        last_updated.unwrap_or("unknown"),
        if last_updated.is_some() { " HKT" } else { "" },
        flights.len(),
        total_on_board
    ));
    output
}

pub fn guidance_lines(flights: &[Flight]) -> String {
    let mut output = String::new();
    for flight in flights {
        if let Some(guidance) = &flight.guidance {
            output.push_str(&format!(
                "{} {}: {}\n",
                flight.flight_number,
                route_label(flight),
                guidance
            ));
        }
    }
    output
}

pub fn alerts_list(
    alerts: &[Alert],
    direction: &str,
    date: &str,
    last_updated: Option<&str>,
) -> String {
    let mut output = String::new();
    let scope = if direction == "flights" {
        "all flights"
    } else {
        direction
    };
    output.push_str(&format!(
        "HKG alerts ({scope})  |  {date}  |  {} alert(s)\n",
        alerts.len()
    ));
    if alerts.is_empty() {
        output.push_str("No cancellations or delays at the current threshold.\n");
    }
    for alert in alerts {
        let marker = if alert.severity == "critical" {
            "[CRIT]"
        } else {
            "[WARN]"
        };
        output.push_str(&format!(
            "{marker} {:<8} {:<9} {:<12} {}\n",
            alert.flight_number,
            alert.route.join("/"),
            alert.kind,
            alert.message
        ));
    }
    output.push_str(&format!(
        "\nUpdated {} HKT\n",
        last_updated.unwrap_or("unknown")
    ));
    output
}

pub fn weather_lines(report: &WeatherReport) -> String {
    if report.warnings.is_empty() {
        return format!("Weather: no active HKO warnings ({})\n", report.source);
    }
    let mut output = format!("Weather warnings ({})\n", report.source);
    for warning in &report.warnings {
        output.push_str(&warning_line(warning));
    }
    output
}

fn warning_line(warning: &WeatherWarning) -> String {
    let code = warning.code.as_deref().unwrap_or("warning");
    let text = warning
        .contents
        .first()
        .cloned()
        .unwrap_or_else(|| code.to_string());
    format!(
        "  {code}{}: {text}\n",
        warning
            .updated_at
            .as_deref()
            .map(|time| format!(" ({time})"))
            .unwrap_or_default()
    )
}

pub fn changes_list(changes: &[Change], timestamp: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "-- {timestamp} HKT -- {} change(s)\n",
        changes.len()
    ));
    for change in changes {
        output.push_str(&format!(
            "  {:<10} {:<8} {}\n",
            change.kind, change.flight_number, change.message
        ));
    }
    output
}

pub fn stats_table(
    stats: &Stats,
    direction: &str,
    date: &str,
    last_updated: Option<&str>,
) -> String {
    let mut output = String::new();
    output.push_str(&format!("HKG {direction}  |  {date}\n"));
    output.push_str(&format!(
        "Total {}  |  cancelled {}  |  delayed {}  |  on time {}  |  early {}  |  scheduled {}\n",
        stats.total, stats.cancelled, stats.delayed, stats.on_time, stats.early, stats.scheduled
    ));
    match (stats.average_delay_minutes, stats.on_time_percent) {
        (Some(average), Some(on_time)) => output.push_str(&format!(
            "Average delay {:+.1} min  |  max {} min  |  on-time rate {:.1}% of tracked flights\n",
            average,
            stats
                .max_delay_minutes
                .map(|value| format!("{value:+}"))
                .unwrap_or_else(|| "-".to_string()),
            on_time
        )),
        _ => output
            .push_str("No estimated or actual times published yet; delay stats unavailable.\n"),
    }
    if !stats.by_hour.is_empty() {
        output.push_str("\nHour   flights  delayed  cancelled\n");
        for hour in &stats.by_hour {
            output.push_str(&format!(
                "{:<6} {:>7}  {:>7}  {:>9}\n",
                hour.hour, hour.flights, hour.delayed, hour.cancelled
            ));
        }
    }
    output.push_str(&format!(
        "\nUpdated {} HKT\n",
        last_updated.unwrap_or("unknown")
    ));
    output
}

pub fn history_table(events: &[StoredEvent], flight_number: &str, date: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "HKG observed history  |  {}  |  {}  |  {} event(s)\n",
        flight_number,
        date,
        events.len()
    ));
    if events.is_empty() {
        output.push_str(
            "No observations stored yet. Run `hkia watch`, `hkia alerts`, or this command again.\n",
        );
        return output;
    }
    output.push_str("OBSERVED AT                KIND        STATE      DELAY  GATE  SUMMARY\n");
    for event in events {
        output.push_str(&format!(
            "{:<25}  {:<10}  {:<9}  {:<5}  {:<4}  {}\n",
            event.observed_at,
            event.kind,
            event.state.as_deref().unwrap_or("-"),
            delay_label(event.delay_minutes),
            event.gate.as_deref().unwrap_or("-"),
            event.summary
        ));
    }
    output
}

pub fn range_table(report: &RangeReport, group_by: Option<&str>) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "HKG history  |  {} to {}  |  {}/{} days with data\n",
        report.from, report.to, report.days_with_data, report.days_requested
    ));
    output.push_str(&summary_line(&report.summary));
    output.push_str("\nDay         flights  delay  cancel  avg delay  on-time\n");
    for day in &report.by_day {
        output.push_str(&format!(
            "{:<10}  {:>7}  {:>5}  {:>6}  {:>9}  {:>7}\n",
            day.date,
            day.summary.flights,
            day.summary.delayed,
            day.summary.cancelled,
            day.summary
                .average_delay_minutes
                .map(|value| format!("{value:+.1}"))
                .unwrap_or_else(|| "-".to_string()),
            day.summary
                .on_time_percent
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| "-".to_string())
        ));
    }

    let (title, groups): (&str, &[GroupReport]) = match group_by {
        Some(name) => match group(report, name) {
            Some(groups) => (name, groups),
            None => ("airline", &report.by_airline),
        },
        None => ("airline", &report.by_airline),
    };
    output.push_str(&format!("\nBy {title} (top 15)\n"));
    output.push_str("KEY              flights  delay  cancel  avg delay  on-time\n");
    for group in groups.iter().take(15) {
        let label = match &group.label {
            Some(label) => format!("{} ({})", label, group.key),
            None => group.key.clone(),
        };
        output.push_str(&format!(
            "{:<16}  {:>7}  {:>5}  {:>6}  {:>9}  {:>7}\n",
            truncate(&label, 16),
            group.summary.flights,
            group.summary.delayed,
            group.summary.cancelled,
            group
                .summary
                .average_delay_minutes
                .map(|value| format!("{value:+.1}"))
                .unwrap_or_else(|| "-".to_string()),
            group
                .summary
                .on_time_percent
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| "-".to_string())
        ));
    }

    if !report.failures.is_empty() {
        output.push_str(&format!("\n{} day(s) failed:\n", report.failures.len()));
        for failure in report.failures.iter().take(10) {
            output.push_str(&format!("  {failure}\n"));
        }
    }
    output
}

fn summary_line(summary: &crate::analytics::Summary) -> String {
    format!(
        "Total {}  |  cancelled {}  |  delayed {}  |  on time {}  |  early {}  |  tracked {}\nAverage delay {}  |  max {}  |  on-time {}\n",
        summary.flights,
        summary.cancelled,
        summary.delayed,
        summary.on_time,
        summary.early,
        summary.tracked,
        summary
            .average_delay_minutes
            .map(|value| format!("{value:+.1} min"))
            .unwrap_or_else(|| "-".to_string()),
        summary
            .max_delay_minutes
            .map(|value| format!("{value:+} min"))
            .unwrap_or_else(|| "-".to_string()),
        summary
            .on_time_percent
            .map(|value| format!("{value:.1}%"))
            .unwrap_or_else(|| "-".to_string())
    )
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

pub fn flights_csv(flights: &[Flight], header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "time",
            "flight_number",
            "airline",
            "airline_name",
            "codeshares",
            "direction",
            "date",
            "scheduled_at",
            "status",
            "state",
            "delay_minutes",
            "delay_basis",
            "terminal",
            "gate",
            "stand",
            "check_in_aisle",
            "baggage_belt",
            "arrival_hall",
            "origin",
            "destination",
            "guidance",
        ])?;
    }
    for flight in flights {
        writer.write_record([
            flight.scheduled_time.clone(),
            flight.flight_number.clone(),
            flight.airline.clone(),
            flight.airline_name.clone().unwrap_or_default(),
            flight.codeshares.join(";"),
            flight.direction.clone(),
            flight.date.clone(),
            flight.scheduled_at.clone(),
            flight.status.clone().unwrap_or_default(),
            flight.state.clone(),
            flight
                .delay_minutes
                .map(|value| value.to_string())
                .unwrap_or_default(),
            flight.delay_basis.clone().unwrap_or_default(),
            flight.terminal.clone().unwrap_or_default(),
            flight.gate.clone().unwrap_or_default(),
            flight.stand.clone().unwrap_or_default(),
            flight.check_in_aisle.clone().unwrap_or_default(),
            flight.baggage_belt.clone().unwrap_or_default(),
            flight.arrival_hall.clone().unwrap_or_default(),
            flight.origin.join("/"),
            flight.destination.join("/"),
            flight.guidance.clone().unwrap_or_default(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

pub fn alerts_csv(alerts: &[Alert], header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "kind",
            "severity",
            "flight_number",
            "codeshares",
            "direction",
            "route",
            "scheduled_at",
            "delay_minutes",
            "estimated_at",
            "actual_at",
            "terminal",
            "gate",
            "stand",
            "baggage_belt",
            "message",
        ])?;
    }
    for alert in alerts {
        writer.write_record([
            alert.kind.clone(),
            alert.severity.clone(),
            alert.flight_number.clone(),
            alert.codeshares.join(";"),
            alert.direction.clone(),
            alert.route.join("/"),
            alert.scheduled_at.clone(),
            alert
                .delay_minutes
                .map(|value| value.to_string())
                .unwrap_or_default(),
            alert.estimated_at.clone().unwrap_or_default(),
            alert.actual_at.clone().unwrap_or_default(),
            alert.terminal.clone().unwrap_or_default(),
            alert.gate.clone().unwrap_or_default(),
            alert.stand.clone().unwrap_or_default(),
            alert.baggage_belt.clone().unwrap_or_default(),
            alert.message.clone(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

pub fn history_csv(events: &[StoredEvent], header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "observed_at",
            "kind",
            "flight_number",
            "board_date",
            "direction",
            "scheduled_at",
            "status",
            "state",
            "delay_minutes",
            "terminal",
            "gate",
            "stand",
            "baggage_belt",
            "summary",
        ])?;
    }
    for event in events {
        writer.write_record([
            event.observed_at.clone(),
            event.kind.clone(),
            event.flight_number.clone(),
            event.board_date.clone(),
            event.direction.clone(),
            event.scheduled_at.clone(),
            event.status.clone().unwrap_or_default(),
            event.state.clone().unwrap_or_default(),
            event
                .delay_minutes
                .map(|value| value.to_string())
                .unwrap_or_default(),
            event.terminal.clone().unwrap_or_default(),
            event.gate.clone().unwrap_or_default(),
            event.stand.clone().unwrap_or_default(),
            event.baggage_belt.clone().unwrap_or_default(),
            event.summary.clone(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

pub fn stats_csv(stats: &Stats, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record(["hour", "flights", "delayed", "cancelled"])?;
    }
    for hour in &stats.by_hour {
        writer.write_record([
            hour.hour.clone(),
            hour.flights.to_string(),
            hour.delayed.to_string(),
            hour.cancelled.to_string(),
        ])?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

pub fn range_csv(report: &RangeReport, group_by: Option<&str>, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    match group_by {
        Some(name) => {
            let groups = group(report, name).ok_or_else(|| {
                anyhow!("unknown group '{name}'; use airline, route, hour or weekday")
            })?;
            if header {
                writer.write_record([
                    "group",
                    "label",
                    "flights",
                    "cancelled",
                    "delayed",
                    "tracked",
                    "average_delay_minutes",
                    "on_time_percent",
                ])?;
            }
            for group in groups {
                writer.write_record([
                    group.key.clone(),
                    group.label.clone().unwrap_or_default(),
                    group.summary.flights.to_string(),
                    group.summary.cancelled.to_string(),
                    group.summary.delayed.to_string(),
                    group.summary.tracked.to_string(),
                    group
                        .summary
                        .average_delay_minutes
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    group
                        .summary
                        .on_time_percent
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ])?;
            }
        }
        None => {
            if header {
                writer.write_record([
                    "date",
                    "flights",
                    "cancelled",
                    "delayed",
                    "tracked",
                    "average_delay_minutes",
                    "on_time_percent",
                ])?;
            }
            for day in &report.by_day {
                writer.write_record([
                    day.date.clone(),
                    day.summary.flights.to_string(),
                    day.summary.cancelled.to_string(),
                    day.summary.delayed.to_string(),
                    day.summary.tracked.to_string(),
                    day.summary
                        .average_delay_minutes
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    day.summary
                        .on_time_percent
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ])?;
            }
        }
    }
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}
