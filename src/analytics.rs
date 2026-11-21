//! Multi-day historical analytics over the airport's past-flight endpoint.

use std::collections::HashMap;

use anyhow::{Result, bail};
use chrono::{Datelike, NaiveDate};
use futures::StreamExt;
use serde::Serialize;

use crate::api::{self, Client, Direction, Flight};
use crate::reference;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub flights: usize,
    pub cancelled: usize,
    pub delayed: usize,
    pub on_time: usize,
    pub early: usize,
    pub scheduled: usize,
    pub tracked: usize,
    pub average_delay_minutes: Option<f64>,
    pub max_delay_minutes: Option<i64>,
    pub on_time_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayReport {
    pub date: String,
    #[serde(flatten)]
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupReport {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize)]
pub struct RangeReport {
    pub from: String,
    pub to: String,
    pub days_requested: usize,
    pub days_with_data: usize,
    pub failures: Vec<String>,
    #[serde(flatten)]
    pub summary: Summary,
    pub by_day: Vec<DayReport>,
    pub by_airline: Vec<GroupReport>,
    pub by_route: Vec<GroupReport>,
    pub by_weekday: Vec<GroupReport>,
    pub by_hour: Vec<GroupReport>,
}

#[derive(Default)]
struct Bucket {
    flights: usize,
    cancelled: usize,
    delayed: usize,
    on_time: usize,
    early: usize,
    scheduled: usize,
    tracked: usize,
    delay_sum: i64,
    max_delay: Option<i64>,
}

impl Bucket {
    fn add(&mut self, flight: &Flight) {
        self.flights += 1;
        match flight.state.as_str() {
            "cancelled" => self.cancelled += 1,
            "delayed" => self.delayed += 1,
            "on_time" => self.on_time += 1,
            "early" => self.early += 1,
            _ => self.scheduled += 1,
        }
        if !flight.cancelled
            && let Some(delay) = flight.delay_minutes
        {
            self.tracked += 1;
            self.delay_sum += delay;
            self.max_delay = Some(
                self.max_delay
                    .map(|value| value.max(delay))
                    .unwrap_or(delay),
            );
        }
    }

    fn finish(&self) -> Summary {
        Summary {
            flights: self.flights,
            cancelled: self.cancelled,
            delayed: self.delayed,
            on_time: self.on_time,
            early: self.early,
            scheduled: self.scheduled,
            tracked: self.tracked,
            average_delay_minutes: (self.tracked > 0)
                .then(|| (self.delay_sum as f64 / self.tracked as f64 * 10.0).round() / 10.0),
            max_delay_minutes: self.max_delay,
            on_time_percent: (self.tracked > 0).then(|| {
                ((self.on_time + self.early) as f64 / self.tracked as f64 * 1000.0).round() / 10.0
            }),
        }
    }
}

type DayFetch = (NaiveDate, Result<(Vec<Flight>, Option<String>)>);

pub async fn range_report(
    client: &Client,
    from: NaiveDate,
    to: NaiveDate,
    direction: Option<Direction>,
    cargo: bool,
    lang: &str,
) -> Result<RangeReport> {
    if from > to {
        bail!("--from must be on or before --to");
    }
    let days = (to - from).num_days() + 1;
    if days > 92 {
        bail!("date range is too long ({days} days); the airport accepts D-91 through D+14");
    }

    let dates: Vec<NaiveDate> = (0..days)
        .map(|offset| from + chrono::Duration::days(offset))
        .collect();
    let today = api::today_hk();
    let results: Vec<DayFetch> = futures::stream::iter(dates.into_iter().map(|date| async move {
        let past = date < today;
        let result = api::fetch_flights_at(client, date, direction, cargo, lang, past).await;
        (date, result)
    }))
    .buffer_unordered(4)
    .collect()
    .await;

    let mut overall = Bucket::default();
    let mut day_buckets: HashMap<NaiveDate, Bucket> = HashMap::new();
    let mut airline_buckets: HashMap<String, Bucket> = HashMap::new();
    let mut route_buckets: HashMap<String, Bucket> = HashMap::new();
    let mut weekday_buckets: HashMap<String, Bucket> = HashMap::new();
    let mut hour_buckets: HashMap<String, Bucket> = HashMap::new();
    let mut failures = Vec::new();
    let mut days_with_data = 0usize;

    for (date, result) in results {
        match result {
            Ok((flights, _)) => {
                days_with_data += 1;
                for flight in &flights {
                    overall.add(flight);
                    day_buckets.entry(date).or_default().add(flight);
                    airline_buckets
                        .entry(flight.airline.clone())
                        .or_default()
                        .add(flight);
                    let route = flight.route().join("/");
                    route_buckets
                        .entry(if route.is_empty() {
                            "unknown".to_string()
                        } else {
                            route
                        })
                        .or_default()
                        .add(flight);
                    if let Ok(day) = NaiveDate::parse_from_str(&flight.date, "%Y-%m-%d") {
                        weekday_buckets
                            .entry(day.weekday().to_string())
                            .or_default()
                            .add(flight);
                    }
                    let hour = flight
                        .scheduled_time
                        .get(0..2)
                        .filter(|value| value.chars().all(|character| character.is_ascii_digit()))
                        .map(|value| format!("{value}:00"))
                        .unwrap_or_else(|| "--:--".to_string());
                    hour_buckets.entry(hour).or_default().add(flight);
                }
            }
            Err(error) => failures.push(format!("{date}: {error}")),
        }
    }

    let mut by_day: Vec<DayReport> = day_buckets
        .into_iter()
        .map(|(date, bucket)| DayReport {
            date: date.to_string(),
            summary: bucket.finish(),
        })
        .collect();
    by_day.sort_by(|left, right| left.date.cmp(&right.date));

    let mut by_airline: Vec<GroupReport> = airline_buckets
        .into_iter()
        .map(|(key, bucket)| GroupReport {
            label: reference::airline_name(&key),
            key,
            summary: bucket.finish(),
        })
        .collect();
    by_airline.sort_by(|left, right| {
        right
            .summary
            .flights
            .cmp(&left.summary.flights)
            .then_with(|| left.key.cmp(&right.key))
    });

    let mut by_route: Vec<GroupReport> = route_buckets
        .into_iter()
        .map(|(key, bucket)| GroupReport {
            key,
            label: None,
            summary: bucket.finish(),
        })
        .collect();
    by_route.sort_by(|left, right| {
        right
            .summary
            .flights
            .cmp(&left.summary.flights)
            .then_with(|| left.key.cmp(&right.key))
    });

    let weekday_order = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    let mut by_weekday: Vec<GroupReport> = weekday_buckets
        .into_iter()
        .map(|(key, bucket)| GroupReport {
            key,
            label: None,
            summary: bucket.finish(),
        })
        .collect();
    by_weekday.sort_by_key(|group| {
        weekday_order
            .iter()
            .position(|day| *day == group.key)
            .unwrap_or(usize::MAX)
    });

    let mut by_hour: Vec<GroupReport> = hour_buckets
        .into_iter()
        .map(|(key, bucket)| GroupReport {
            key,
            label: None,
            summary: bucket.finish(),
        })
        .collect();
    by_hour.sort_by(|left, right| left.key.cmp(&right.key));

    Ok(RangeReport {
        from: from.to_string(),
        to: to.to_string(),
        days_requested: days as usize,
        days_with_data,
        failures,
        summary: overall.finish(),
        by_day,
        by_airline,
        by_route,
        by_weekday,
        by_hour,
    })
}

pub fn group<'a>(report: &'a RangeReport, name: &str) -> Option<&'a [GroupReport]> {
    match name.trim().to_ascii_lowercase().as_str() {
        "airline" | "airlines" => Some(&report.by_airline),
        "route" | "routes" => Some(&report.by_route),
        "hour" | "hours" => Some(&report.by_hour),
        "weekday" | "weekdays" | "day" => Some(&report.by_weekday),
        _ => None,
    }
}
