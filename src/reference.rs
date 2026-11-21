//! Static airport and airline reference data.
//!
//! Airport names come from OurAirports (public domain). Airline names come from
//! OpenFlights (ODbL); see `NOTICE` for attribution. Both datasets are embedded at
//! build time so the tool works offline once installed.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AirportInfo {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

type AirportRow = [String; 3];

static AIRPORTS: OnceLock<HashMap<String, AirportRow>> = OnceLock::new();
static AIRLINES: OnceLock<HashMap<String, String>> = OnceLock::new();

fn airports() -> &'static HashMap<String, AirportRow> {
    AIRPORTS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/airports.json"))
            .expect("embedded airports.json is valid")
    })
}

fn airlines() -> &'static HashMap<String, String> {
    AIRLINES.get_or_init(|| {
        serde_json::from_str(include_str!("../data/airlines.json"))
            .expect("embedded airlines.json is valid")
    })
}

pub fn airport(code: &str) -> Option<AirportInfo> {
    let code = code.trim().to_ascii_uppercase();
    let row = airports().get(&code)?;
    Some(AirportInfo {
        code,
        name: row[0].clone(),
        city: Some(row[1].clone()).filter(|value| !value.is_empty()),
        country: Some(row[2].clone()).filter(|value| !value.is_empty()),
    })
}

pub fn airline_name(icao: &str) -> Option<String> {
    airlines().get(&icao.trim().to_ascii_uppercase()).cloned()
}

/// Compact label for tables, e.g. `London (LHR)` or `LHR` when unknown.
pub fn airport_label(code: &str) -> String {
    match airport(code) {
        Some(info) => match info.city {
            Some(city) if !city.is_empty() && !city.eq_ignore_ascii_case(&info.code) => {
                format!("{city} ({})", info.code)
            }
            _ => info.code,
        },
        None => code.trim().to_ascii_uppercase(),
    }
}
