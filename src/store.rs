//! Local SQLite store for observed flight state and change history.
//!
//! The airport feed only publishes the current state of a flight. To answer
//! "what happened to CX 251 today?" we have to remember what we saw. Every poll
//! updates one row per flight and appends an event only when something changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::api::{self, Change, Flight};

#[derive(Debug, Clone, Serialize)]
pub struct StoredEvent {
    pub id: i64,
    pub observed_at: String,
    pub kind: String,
    pub flight_number: String,
    pub board_date: String,
    pub direction: String,
    pub scheduled_at: String,
    pub status: Option<String>,
    pub state: Option<String>,
    pub delay_minutes: Option<i64>,
    pub gate: Option<String>,
    pub terminal: Option<String>,
    pub baggage_belt: Option<String>,
    pub stand: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunwayChange {
    pub observed_at: String,
    pub arrival: String,
    pub departure: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunwayDay {
    pub date: String,
    pub switches: usize,
    pub first_arrival: String,
    pub first_departure: String,
    pub last_arrival: String,
    pub last_departure: String,
}

pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    pub fn open_default() -> Result<Self> {
        Self::open(&db_path())
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating data directory {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("opening history database {}", path.display()))?;
        let _ = connection.busy_timeout(std::time::Duration::from_secs(5));
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.initialise()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.initialise()?;
        Ok(store)
    }

    fn initialise(&self) -> Result<()> {
        let connection = self.connection.lock().expect("store lock poisoned");
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS flight_state (
                flight_key TEXT PRIMARY KEY,
                board_date TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                flight_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS flight_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                observed_at TEXT NOT NULL,
                flight_key TEXT NOT NULL,
                flight_number TEXT NOT NULL,
                board_date TEXT NOT NULL,
                direction TEXT NOT NULL,
                scheduled_at TEXT NOT NULL,
                kind TEXT NOT NULL,
                summary TEXT NOT NULL,
                status TEXT,
                state TEXT,
                delay_minutes INTEGER,
                gate TEXT,
                terminal TEXT,
                baggage_belt TEXT,
                stand TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_events_time ON flight_events(observed_at);
            CREATE INDEX IF NOT EXISTS idx_events_flight
                ON flight_events(flight_number, board_date);
            CREATE TABLE IF NOT EXISTS alert_state (
                alert_key TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL,
                notified_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS runway_changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                observed_at TEXT NOT NULL,
                arrival TEXT NOT NULL,
                departure TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_runway_time ON runway_changes(observed_at);
            ",
        )?;
        Ok(())
    }

    /// Update the current-state table and append events for anything that changed.
    pub fn record(
        &self,
        observed_at: &str,
        flights: &[Flight],
        min_delay: i64,
    ) -> Result<Vec<Change>> {
        let Some(board_date) = flights.first().map(|flight| flight.date.clone()) else {
            return Ok(Vec::new());
        };
        let mut connection = self.connection.lock().expect("store lock poisoned");
        let transaction = connection.transaction()?;

        let mut previous: HashMap<String, (String, Flight)> = HashMap::new();
        {
            let mut statement = transaction.prepare(
                "SELECT flight_key, fingerprint, flight_json FROM flight_state WHERE board_date = ?1",
            )?;
            let rows = statement.query_map(params![board_date], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (key, fingerprint, json) = row?;
                if let Ok(flight) = serde_json::from_str::<Flight>(&json) {
                    previous.insert(key, (fingerprint, flight));
                }
            }
        }

        let mut changes = Vec::new();
        {
            let mut insert_event = transaction.prepare(
                "INSERT INTO flight_events (
                    observed_at, flight_key, flight_number, board_date, direction,
                    scheduled_at, kind, summary, status, state, delay_minutes,
                    gate, terminal, baggage_belt, stand
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            let mut upsert_state = transaction.prepare(
                "INSERT INTO flight_state (flight_key, board_date, fingerprint, observed_at, flight_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(flight_key) DO UPDATE SET
                    board_date = excluded.board_date,
                    fingerprint = excluded.fingerprint,
                    observed_at = excluded.observed_at,
                    flight_json = excluded.flight_json",
            )?;

            for flight in flights {
                let key = api::flight_key(flight);
                let fingerprint = fingerprint(flight);
                match previous.get(&key) {
                    None => {
                        insert_event.execute(params![
                            observed_at,
                            key,
                            flight.flight_number,
                            flight.date,
                            flight.direction,
                            flight.scheduled_at,
                            "first_seen",
                            format!(
                                "First observed as {} (scheduled {})",
                                flight.state, flight.scheduled_time
                            ),
                            flight.status,
                            flight.state,
                            flight.delay_minutes,
                            flight.gate,
                            flight.terminal,
                            flight.baggage_belt,
                            flight.stand,
                        ])?;
                    }
                    Some((old_fingerprint, old_flight)) => {
                        if old_fingerprint != &fingerprint {
                            let mut prior = HashMap::new();
                            prior.insert(key.clone(), old_flight.clone());
                            let flight_changes = api::diff_snapshots(
                                &prior,
                                std::slice::from_ref(flight),
                                min_delay,
                            );
                            if flight_changes.is_empty() {
                                insert_event.execute(params![
                                    observed_at,
                                    key,
                                    flight.flight_number,
                                    flight.date,
                                    flight.direction,
                                    flight.scheduled_at,
                                    "updated",
                                    format!(
                                        "{} updated (delay {})",
                                        flight.flight_number,
                                        flight
                                            .delay_minutes
                                            .map(|value| format!("{value:+} min"))
                                            .unwrap_or_else(|| "unknown".to_string())
                                    ),
                                    flight.status,
                                    flight.state,
                                    flight.delay_minutes,
                                    flight.gate,
                                    flight.terminal,
                                    flight.baggage_belt,
                                    flight.stand,
                                ])?;
                            } else {
                                for change in &flight_changes {
                                    insert_event.execute(params![
                                        observed_at,
                                        key,
                                        flight.flight_number,
                                        flight.date,
                                        flight.direction,
                                        flight.scheduled_at,
                                        change.kind,
                                        change.message,
                                        flight.status,
                                        flight.state,
                                        flight.delay_minutes,
                                        flight.gate,
                                        flight.terminal,
                                        flight.baggage_belt,
                                        flight.stand,
                                    ])?;
                                }
                            }
                            changes.extend(flight_changes);
                        }
                    }
                }

                let json = serde_json::to_string(flight)?;
                upsert_state.execute(params![key, flight.date, fingerprint, observed_at, json])?;
            }
        }

        transaction.execute(
            "DELETE FROM flight_state WHERE board_date <> ?1",
            params![board_date],
        )?;
        transaction.commit()?;
        Ok(changes)
    }

    pub fn events_for_flight(
        &self,
        flight_number: &str,
        board_date: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let mut statement = connection.prepare(
            "SELECT id, observed_at, kind, flight_number, board_date, direction,
                    scheduled_at, status, state, delay_minutes, gate, terminal,
                    baggage_belt, stand, summary
             FROM flight_events
             WHERE REPLACE(flight_number, ' ', '') = REPLACE(?1, ' ', '') COLLATE NOCASE
               AND (?2 IS NULL OR board_date = ?2)
             ORDER BY observed_at ASC, id ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![flight_number, board_date, limit as i64],
            event_from_row,
        )?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Events strictly newer than `after`. First-seen rows are noise for change
    /// feeds, so they are excluded unless requested.
    pub fn events_since(
        &self,
        after: &str,
        include_first_seen: bool,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let mut statement = connection.prepare(
            "SELECT id, observed_at, kind, flight_number, board_date, direction,
                    scheduled_at, status, state, delay_minutes, gate, terminal,
                    baggage_belt, stand, summary
             FROM flight_events
             WHERE observed_at > ?1
               AND (?2 = 1 OR kind <> 'first_seen')
             ORDER BY observed_at ASC, id ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![after, include_first_seen as i64, limit as i64],
            event_from_row,
        )?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub fn should_notify(&self, alert_key: &str, fingerprint: &str) -> Result<bool> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let existing: Option<String> = connection
            .query_row(
                "SELECT fingerprint FROM alert_state WHERE alert_key = ?1",
                params![alert_key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(existing.as_deref() != Some(fingerprint))
    }

    pub fn mark_notified(
        &self,
        alert_key: &str,
        fingerprint: &str,
        notified_at: &str,
    ) -> Result<()> {
        let connection = self.connection.lock().expect("store lock poisoned");
        connection.execute(
            "INSERT INTO alert_state (alert_key, fingerprint, notified_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(alert_key) DO UPDATE SET
                fingerprint = excluded.fingerprint,
                notified_at = excluded.notified_at",
            params![alert_key, fingerprint, notified_at],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn event_count(&self) -> Result<usize> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM flight_events", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Remember the runway in use, returning true when it differs from the last row.
    pub fn record_runway(&self, observed_at: &str, arrival: &str, departure: &str) -> Result<bool> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let previous: Option<(String, String)> = connection
            .query_row(
                "SELECT arrival, departure FROM runway_changes ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if previous
            .as_ref()
            .is_some_and(|(last_arrival, last_departure)| {
                last_arrival == arrival && last_departure == departure
            })
        {
            return Ok(false);
        }
        connection.execute(
            "INSERT INTO runway_changes (observed_at, arrival, departure) VALUES (?1, ?2, ?3)",
            params![observed_at, arrival, departure],
        )?;
        Ok(true)
    }

    /// Locally observed runway switches, oldest first.
    pub fn runway_history(&self, limit: usize) -> Result<Vec<RunwayChange>> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let limit = if limit == 0 { i64::MAX } else { limit as i64 };
        let mut statement = connection.prepare(
            "SELECT observed_at, arrival, departure FROM runway_changes ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            Ok(RunwayChange {
                observed_at: row.get(0)?,
                arrival: row.get(1)?,
                departure: row.get(2)?,
            })
        })?;
        let mut changes = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        changes.reverse();
        Ok(changes)
    }

    /// Observed runway switches grouped by local date, oldest day first.
    /// `limit` caps the number of days; 0 returns every recorded day.
    pub fn runway_daily(&self, limit: usize) -> Result<Vec<RunwayDay>> {
        let changes = self.runway_history(0)?;
        let mut days: Vec<RunwayDay> = Vec::new();
        for change in changes {
            let date = change
                .observed_at
                .get(0..10)
                .unwrap_or(&change.observed_at)
                .to_string();
            match days.last_mut() {
                Some(day) if day.date == date => {
                    day.switches += 1;
                    day.last_arrival = change.arrival;
                    day.last_departure = change.departure;
                }
                _ => days.push(RunwayDay {
                    date,
                    switches: 1,
                    first_arrival: change.arrival.clone(),
                    first_departure: change.departure.clone(),
                    last_arrival: change.arrival,
                    last_departure: change.departure,
                }),
            }
        }
        if limit > 0 && days.len() > limit {
            days = days.split_off(days.len() - limit);
        }
        Ok(days)
    }

    pub fn prune(&self, keep_days: i64) -> Result<usize> {
        let connection = self.connection.lock().expect("store lock poisoned");
        let cutoff = (api::hk_now() - chrono::Duration::days(keep_days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let removed = connection.execute(
            "DELETE FROM flight_events WHERE observed_at < ?1",
            params![cutoff],
        )?;
        let removed_runway = connection.execute(
            "DELETE FROM runway_changes WHERE observed_at < ?1",
            params![cutoff],
        )?;
        Ok(removed + removed_runway)
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok(StoredEvent {
        id: row.get(0)?,
        observed_at: row.get(1)?,
        kind: row.get(2)?,
        flight_number: row.get(3)?,
        board_date: row.get(4)?,
        direction: row.get(5)?,
        scheduled_at: row.get(6)?,
        status: row.get(7)?,
        state: row.get(8)?,
        delay_minutes: row.get(9)?,
        gate: row.get(10)?,
        terminal: row.get(11)?,
        baggage_belt: row.get(12)?,
        stand: row.get(13)?,
        summary: row.get(14)?,
    })
}

fn fingerprint(flight: &Flight) -> String {
    serde_json::json!([
        flight.status,
        flight.status_type,
        flight.state,
        flight.cancelled,
        flight.delay_minutes,
        flight.delay_basis,
        flight.estimated_at,
        flight.actual_at,
        flight.gate,
        flight.terminal,
        flight.check_in_aisle,
        flight.baggage_belt,
        flight.arrival_hall,
        flight.stand,
    ])
    .to_string()
}

pub fn data_dir() -> PathBuf {
    if let Ok(value) = std::env::var("HKIA_DATA_DIR")
        && !value.trim().is_empty()
    {
        return PathBuf::from(value);
    }
    if let Ok(value) = std::env::var("XDG_DATA_HOME")
        && !value.trim().is_empty()
    {
        return PathBuf::from(value).join("hkia");
    }
    home_dir().join(".local").join("share").join("hkia")
}

pub fn db_path() -> PathBuf {
    data_dir().join("hkia.db")
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_flights() -> Vec<Flight> {
        let body = include_str!("../tests/fixtures/departures.json");
        api::board_from_body(
            body,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
            api::Direction::Departure,
            false,
        )
        .unwrap()
        .flights
    }

    #[test]
    fn records_runway_switches_once() {
        let store = Store::open_in_memory().unwrap();
        assert!(
            store
                .record_runway("2026-11-22T01:00:00+08:00", "07C", "07R")
                .unwrap()
        );
        assert!(
            !store
                .record_runway("2026-11-22T01:01:00+08:00", "07C", "07R")
                .unwrap()
        );
        assert!(
            store
                .record_runway("2026-11-22T02:00:00+08:00", "25R", "25C")
                .unwrap()
        );
        let history = store.runway_history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].arrival, "07C");
        assert_eq!(history[0].departure, "07R");
        assert_eq!(history[1].arrival, "25R");
        assert_eq!(store.runway_history(1).unwrap().len(), 1);
        assert_eq!(store.runway_history(0).unwrap().len(), 2);

        let daily = store.runway_daily(0).unwrap();
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].date, "2026-11-22");
        assert_eq!(daily[0].switches, 2);
        assert_eq!(daily[0].first_arrival, "07C");
        assert_eq!(daily[0].last_arrival, "25R");
        assert_eq!(daily[0].last_departure, "25C");

        assert!(
            store
                .record_runway("2026-11-23T05:00:00+08:00", "07L", "07C")
                .unwrap()
        );
        let daily = store.runway_daily(0).unwrap();
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[1].date, "2026-11-23");
        assert_eq!(store.runway_daily(1).unwrap().len(), 1);
        assert_eq!(store.runway_daily(1).unwrap()[0].date, "2026-11-23");
    }

    #[test]
    fn records_timeline_and_dedupes_notifications() {
        let store = Store::open_in_memory().unwrap();
        let mut flights = fixture_flights();
        let first = store
            .record("2026-11-21T20:00:00+08:00", &flights, 15)
            .unwrap();
        assert!(first.is_empty());
        assert_eq!(store.event_count().unwrap(), 4);

        let cx = flights
            .iter_mut()
            .find(|flight| flight.flight_number == "CX 251")
            .unwrap();
        cx.status = Some("Dep 00:30 (21/11/2026)".to_string());
        cx.delay_minutes = Some(90);
        cx.state = "delayed".to_string();
        cx.gate = Some("31".to_string());

        let changes = store
            .record("2026-11-21T20:05:00+08:00", &flights, 15)
            .unwrap();
        assert!(changes.iter().any(|change| change.kind == "delay"));
        assert!(changes.iter().any(|change| change.kind == "gate"));

        let events = store
            .events_for_flight("CX251", Some("2026-11-21"), 20)
            .unwrap();
        assert!(events.len() >= 3);
        assert_eq!(events.first().unwrap().kind, "first_seen");

        assert!(store.should_notify("alert-key", "one").unwrap());
        store
            .mark_notified("alert-key", "one", "2026-11-21T20:06:00+08:00")
            .unwrap();
        assert!(!store.should_notify("alert-key", "one").unwrap());
        assert!(store.should_notify("alert-key", "two").unwrap());
    }
}
