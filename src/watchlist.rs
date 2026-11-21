//! User watchlist and notification configuration.
//!
//! Stored as TOML at `$HKIA_CONFIG`, `$XDG_CONFIG_HOME/hkia/config.toml`, or
//! `~/.config/hkia/config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::api::{Direction, Flight};
use crate::store::home_dir;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub watchlist: Watchlist,
    #[serde(default)]
    pub notify: NotifyConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Watchlist {
    #[serde(default)]
    pub flights: Vec<String>,
    /// Airport IATA codes matched against either endpoint of the route.
    #[serde(default)]
    pub airports: Vec<String>,
    /// Operating airline ICAO codes.
    #[serde(default)]
    pub airlines: Vec<String>,
    /// Optional `departure` or `arrival` restriction.
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub min_delay: Option<i64>,
    #[serde(default)]
    pub include_cancelled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyConfig {
    #[serde(default)]
    pub desktop: bool,
    #[serde(default)]
    pub webhook: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Flight,
    Airport,
    Airline,
}

impl Watchlist {
    pub fn is_empty(&self) -> bool {
        self.flights.is_empty() && self.airports.is_empty() && self.airlines.is_empty()
    }

    pub fn direction(&self) -> Option<Direction> {
        self.direction.as_deref().and_then(Direction::parse)
    }

    pub fn min_delay(&self) -> i64 {
        self.min_delay.unwrap_or(15)
    }

    pub fn include_cancelled(&self) -> bool {
        self.include_cancelled.unwrap_or(true)
    }

    pub fn matches(&self, flight: &Flight) -> bool {
        if let Some(direction) = self.direction()
            && flight.direction != direction.as_str()
        {
            return false;
        }

        let has_selectors =
            !self.flights.is_empty() || !self.airports.is_empty() || !self.airlines.is_empty();
        if !has_selectors {
            return true;
        }

        let flight_key = canonical(&flight.flight_number);
        let flight_match = self
            .flights
            .iter()
            .any(|value| canonical(value) == flight_key)
            || flight.codeshares.iter().any(|code| {
                self.flights
                    .iter()
                    .any(|value| canonical(value) == canonical(code))
            });
        if flight_match {
            return true;
        }

        let airport_match = self.airports.iter().any(|code| {
            flight
                .origin
                .iter()
                .chain(flight.destination.iter())
                .any(|candidate| candidate.eq_ignore_ascii_case(code))
        });
        if airport_match {
            return true;
        }

        self.airlines
            .iter()
            .any(|code| flight.airline.eq_ignore_ascii_case(code))
    }

    pub fn filter(&self, flights: &[Flight]) -> Vec<Flight> {
        flights
            .iter()
            .filter(|flight| self.matches(flight))
            .cloned()
            .collect()
    }

    pub fn add(&mut self, kind: ItemKind, value: &str) -> Result<bool> {
        let value = clean_value(value);
        if value.is_empty() {
            bail!("watchlist value cannot be empty");
        }
        let list = match kind {
            ItemKind::Flight => &mut self.flights,
            ItemKind::Airport => &mut self.airports,
            ItemKind::Airline => &mut self.airlines,
        };
        let normalized = match kind {
            ItemKind::Flight => value.to_ascii_uppercase(),
            ItemKind::Airport | ItemKind::Airline => value.to_ascii_uppercase(),
        };
        if list
            .iter()
            .any(|existing| same_selector(kind, existing, &normalized))
        {
            return Ok(false);
        }
        list.push(normalized);
        list.sort();
        Ok(true)
    }

    pub fn remove(&mut self, kind: ItemKind, value: &str) -> bool {
        let value = clean_value(value);
        let list = match kind {
            ItemKind::Flight => &mut self.flights,
            ItemKind::Airport => &mut self.airports,
            ItemKind::Airline => &mut self.airlines,
        };
        let before = list.len();
        list.retain(|existing| !same_selector(kind, existing, &value));
        list.len() != before
    }
}

fn same_selector(kind: ItemKind, left: &str, right: &str) -> bool {
    match kind {
        ItemKind::Flight => canonical(left) == canonical(right),
        ItemKind::Airport | ItemKind::Airline => left.eq_ignore_ascii_case(right),
    }
}

fn clean_value(value: &str) -> String {
    value.trim().to_string()
}

fn canonical(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialising config")?;
        std::fs::write(path, text).with_context(|| format!("writing config {}", path.display()))
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(value) = std::env::var("HKIA_CONFIG")
        && !value.trim().is_empty()
    {
        return PathBuf::from(value);
    }
    if let Ok(value) = std::env::var("XDG_CONFIG_HOME")
        && !value.trim().is_empty()
    {
        return PathBuf::from(value).join("hkia").join("config.toml");
    }
    home_dir().join(".config").join("hkia").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_deduplicates_and_matches_codeshares() {
        let mut watchlist = Watchlist::default();
        assert!(watchlist.add(ItemKind::Flight, "cx251").unwrap());
        assert!(!watchlist.add(ItemKind::Flight, "CX 251").unwrap());
        assert_eq!(watchlist.flights, vec!["CX251"]);
        assert!(!watchlist.is_empty());
    }

    #[test]
    fn empty_watchlist_matches_everything() {
        let watchlist = Watchlist::default();
        assert!(watchlist.is_empty());
    }

    #[test]
    fn config_round_trips_toml() {
        let mut config = Config::default();
        config.watchlist.add(ItemKind::Flight, "CX 251").unwrap();
        config.watchlist.add(ItemKind::Airport, "lhr").unwrap();
        config.notify.webhook = Some("https://example.test/hook".to_string());
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.watchlist.flights, vec!["CX 251"]);
        assert_eq!(parsed.watchlist.airports, vec!["LHR"]);
        assert_eq!(
            parsed.notify.webhook.as_deref(),
            Some("https://example.test/hook")
        );
    }
}
