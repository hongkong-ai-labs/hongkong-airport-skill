//! Airport access: live Airport Express trains, in-town check-in reference and
//! airport bus route reference.
//!
//! The live train data comes from the MTR Next Train API on data.gov.hk.
//! Check-in and bus facts are deliberately bundled as dated reference data:
//! they change without a machine-readable feed, so every response carries
//! `checked_on` and source URLs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, FixedOffset, NaiveDateTime, SecondsFormat, TimeZone};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::api::{hk_now, hk_offset};

pub const CHECKED_ON: &str = "2026-11-22";
pub const AEL_API_URL: &str = "https://rt.data.gov.hk/v1/transport/mtr/getSchedule.php";
pub const AEL_API_SOURCE: &str = "https://data.gov.hk/en-data/dataset/mtr-data2-nexttrain-data";
pub const AEL_TIMETABLE_SOURCE: &str =
    "https://www.mtr.com.hk/en/customer/services/timetable_index.html";
pub const AEL_FARES_SOURCE: &str = "https://www.mtr.com.hk/en/customer/tickets/tf_index.html";
pub const AEL_FREQUENCY_SOURCE: &str =
    "https://www.mtr.com.hk/en/customer/services/train_service_index.html";
pub const CHECKIN_SOURCE: &str = "https://www.mtr.com.hk/en/customer/services/complom_checkin.html";
pub const BUS_SOURCE: &str =
    "https://www.hongkongairport.com/en/transport/to-from-airport/public-buses.page";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Source {
    pub name: &'static str,
    pub url: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Station {
    pub code: &'static str,
    pub name: &'static str,
}

const STATIONS: [Station; 5] = [
    Station {
        code: "HOK",
        name: "Hong Kong",
    },
    Station {
        code: "KOW",
        name: "Kowloon",
    },
    Station {
        code: "TSY",
        name: "Tsing Yi",
    },
    Station {
        code: "AIR",
        name: "Airport",
    },
    Station {
        code: "AWE",
        name: "AsiaWorld-Expo",
    },
];

pub fn parse_station(input: &str) -> Option<Station> {
    let normalized = input.trim().to_ascii_uppercase().replace([' ', '_'], "-");
    STATIONS.iter().copied().find(|station| {
        station.code == normalized
            || station.name.to_ascii_uppercase().replace(' ', "-") == normalized
            || (station.code == "AWE" && normalized == "ASIAWORLD-EXPO")
    })
}

fn station_name(code: &str) -> &'static str {
    STATIONS
        .iter()
        .find(|station| station.code == code)
        .map(|station| station.name)
        .unwrap_or("Unknown station")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AelDirection {
    #[value(name = "to-airport", alias = "to", alias = "airport")]
    ToAirport,
    #[value(name = "from-airport", alias = "from", alias = "city")]
    FromAirport,
    #[value(name = "both", alias = "all")]
    Both,
}

impl AelDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToAirport => "to-airport",
            Self::FromAirport => "from-airport",
            Self::Both => "both",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "to-airport" | "toairport" | "to" | "airport" | "outbound" => Some(Self::ToAirport),
            "from-airport" | "fromairport" | "from" | "city" | "inbound" => Some(Self::FromAirport),
            "both" | "all" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn default_for(station: Station) -> Self {
        match station.code {
            "HOK" | "KOW" | "TSY" => Self::ToAirport,
            "AIR" | "AWE" => Self::FromAirport,
            _ => Self::Both,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AelTrain {
    pub sequence: u32,
    pub destination_code: String,
    pub destination_name: &'static str,
    pub platform: String,
    pub departs_at: String,
    pub departs_in_minutes: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct JourneyTime {
    pub from: &'static str,
    pub to: &'static str,
    pub minutes: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceWindow {
    pub from: &'static str,
    pub to: &'static str,
    pub first: &'static str,
    pub last: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AelFare {
    pub station: &'static str,
    pub adult_octopus_hkd: f64,
    pub adult_single_hkd: f64,
    pub child_octopus_hkd: f64,
    pub round_trip_30d_hkd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AelService {
    pub frequency: &'static str,
    pub payment: &'static str,
    pub checked_on: &'static str,
    pub journey_minutes: Vec<JourneyTime>,
    pub service_windows: Vec<ServiceWindow>,
    pub fares: Vec<AelFare>,
}

pub fn ael_service() -> AelService {
    AelService {
        frequency: "About every 10 minutes",
        payment: "Octopus, contactless bank card (adult fare only), QR code, or Airport Express QR Code ticket",
        checked_on: CHECKED_ON,
        journey_minutes: vec![
            JourneyTime {
                from: "HOK",
                to: "AIR",
                minutes: 24,
            },
            JourneyTime {
                from: "KOW",
                to: "AIR",
                minutes: 22,
            },
            JourneyTime {
                from: "TSY",
                to: "AIR",
                minutes: 14,
            },
            JourneyTime {
                from: "HOK",
                to: "AWE",
                minutes: 28,
            },
            JourneyTime {
                from: "KOW",
                to: "AWE",
                minutes: 25,
            },
            JourneyTime {
                from: "TSY",
                to: "AWE",
                minutes: 18,
            },
            JourneyTime {
                from: "AIR",
                to: "AWE",
                minutes: 2,
            },
        ],
        service_windows: vec![
            ServiceWindow {
                from: "HOK",
                to: "AIR",
                first: "05:50",
                last: "00:48",
            },
            ServiceWindow {
                from: "KOW",
                to: "AIR",
                first: "05:53",
                last: "00:52",
            },
            ServiceWindow {
                from: "TSY",
                to: "AIR",
                first: "06:00",
                last: "00:59",
            },
            ServiceWindow {
                from: "AIR",
                to: "HOK",
                first: "05:54",
                last: "00:48",
            },
            ServiceWindow {
                from: "AIR",
                to: "AWE",
                first: "06:15",
                last: "01:13",
            },
            ServiceWindow {
                from: "AWE",
                to: "HOK",
                first: "06:00",
                last: "00:45",
            },
        ],
        fares: vec![
            AelFare {
                station: "HOK",
                adult_octopus_hkd: 120.0,
                adult_single_hkd: 130.0,
                child_octopus_hkd: 60.0,
                round_trip_30d_hkd: Some(215.0),
            },
            AelFare {
                station: "KOW",
                adult_octopus_hkd: 105.0,
                adult_single_hkd: 115.0,
                child_octopus_hkd: 52.5,
                round_trip_30d_hkd: Some(195.0),
            },
            AelFare {
                station: "TSY",
                adult_octopus_hkd: 73.0,
                adult_single_hkd: 80.0,
                child_octopus_hkd: 36.5,
                round_trip_30d_hkd: Some(130.0),
            },
            AelFare {
                station: "AIR-AWE",
                adult_octopus_hkd: 6.5,
                adult_single_hkd: 7.0,
                child_octopus_hkd: 3.2,
                round_trip_30d_hkd: None,
            },
        ],
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AelResponse {
    pub mode: &'static str,
    pub station: Station,
    pub direction: String,
    pub fetched_at: String,
    pub generated_at: String,
    pub delayed: bool,
    pub note: Option<String>,
    pub trains: Vec<AelTrain>,
    pub service: AelService,
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
struct RawAelResponse {
    #[serde(default)]
    status: i32,
    #[serde(default)]
    message: String,
    #[serde(default)]
    curr_time: String,
    #[serde(default)]
    sys_time: String,
    #[serde(default)]
    isdelay: String,
    #[serde(default)]
    data: HashMap<String, RawStationSchedule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawStationSchedule {
    #[serde(rename = "UP", default)]
    up: Vec<RawTrain>,
    #[serde(rename = "DOWN", default)]
    down: Vec<RawTrain>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTrain {
    #[serde(default)]
    seq: String,
    #[serde(default)]
    dest: String,
    #[serde(default)]
    plat: String,
    #[serde(default)]
    time: String,
    #[serde(default)]
    ttnt: String,
    #[serde(default)]
    valid: String,
}

fn parse_hk_datetime(value: &str) -> Option<DateTime<FixedOffset>> {
    NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|naive| hk_offset().from_local_datetime(&naive).single())
}

fn parse_ael_body(
    body: &str,
    station: Station,
    direction: AelDirection,
    now: DateTime<FixedOffset>,
) -> Result<AelResponse> {
    let raw: RawAelResponse =
        serde_json::from_str(body).context("decoding the MTR Next Train response")?;
    if raw.status != 1 {
        return Err(anyhow!(
            "MTR Next Train API error: {}",
            if raw.message.trim().is_empty() {
                "unknown error"
            } else {
                raw.message.trim()
            }
        ));
    }
    let key = format!("AEL-{}", station.code);
    let schedule = raw.data.get(&key).cloned().unwrap_or_default();
    let raw_trains: Vec<&RawTrain> = match (station.code, direction) {
        // At AsiaWorld-Expo, DOWN trains reach the Airport before continuing to
        // the city; UP never serves that station.
        ("AWE", AelDirection::ToAirport) => schedule.down.iter().collect(),
        // AIR is already the airport station; its UP trains go on to
        // AsiaWorld-Expo, not to the airport.
        ("AIR", AelDirection::ToAirport) => Vec::new(),
        (_, AelDirection::ToAirport) => schedule.up.iter().collect(),
        (_, AelDirection::FromAirport) => schedule.down.iter().collect(),
        (_, AelDirection::Both) => schedule.up.iter().chain(schedule.down.iter()).collect(),
    };

    let mut trains = Vec::new();
    for raw_train in raw_trains {
        if raw_train.valid == "N" {
            continue;
        }
        let Some(departs) = parse_hk_datetime(&raw_train.time) else {
            continue;
        };
        let minutes = raw_train
            .ttnt
            .trim()
            .parse::<u32>()
            .unwrap_or_else(|_| (departs - now).num_minutes().max(0) as u32);
        trains.push(AelTrain {
            sequence: raw_train.seq.trim().parse().unwrap_or(0),
            destination_code: raw_train.dest.trim().to_ascii_uppercase(),
            destination_name: station_name(&raw_train.dest),
            platform: raw_train.plat.trim().to_string(),
            departs_at: departs.to_rfc3339_opts(SecondsFormat::Secs, false),
            departs_in_minutes: minutes,
        });
    }
    trains.sort_by(|left, right| left.departs_at.cmp(&right.departs_at));

    let generated_at = parse_hk_datetime(&raw.curr_time)
        .or_else(|| parse_hk_datetime(&raw.sys_time))
        .unwrap_or(now)
        .to_rfc3339_opts(SecondsFormat::Secs, false);
    let note = if trains.is_empty() {
        if station.code == "AIR" && direction == AelDirection::ToAirport {
            Some(
                "AIR is the airport station; use from-airport for trains to the city, or station AWE for AsiaWorld-Expo."
                    .to_string(),
            )
        } else {
            Some(format!(
                "No upcoming {} trains are published for {}.",
                direction.as_str(),
                station.name
            ))
        }
    } else {
        None
    };

    Ok(AelResponse {
        mode: "airport_express",
        station,
        direction: direction.as_str().to_string(),
        fetched_at: now.to_rfc3339_opts(SecondsFormat::Secs, false),
        generated_at,
        delayed: raw.isdelay.eq_ignore_ascii_case("Y"),
        note,
        trains,
        service: ael_service(),
        sources: vec![
            Source {
                name: "MTR Next Train API (data.gov.hk)",
                url: AEL_API_SOURCE,
            },
            Source {
                name: "MTR Airport Express timetable",
                url: AEL_TIMETABLE_SOURCE,
            },
            Source {
                name: "MTR Airport Express fares",
                url: AEL_FARES_SOURCE,
            },
            Source {
                name: "MTR service hours and frequency",
                url: AEL_FREQUENCY_SOURCE,
            },
        ],
    })
}

pub struct AccessClient {
    http: reqwest::Client,
    ttl: Duration,
    cache: Mutex<HashMap<String, (Instant, AelResponse)>>,
}

impl AccessClient {
    pub fn new(ttl: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(format!("hkia-rust/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the airport access HTTP client")?;
        Ok(Self {
            http,
            ttl,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub async fn ael(&self, station: Station, direction: AelDirection) -> Result<AelResponse> {
        let key = format!("{}|{}", station.code, direction.as_str());
        if !self.ttl.is_zero()
            && let Some((fetched_at, cached)) = self
                .cache
                .lock()
                .expect("access cache lock poisoned")
                .get(&key)
                .cloned()
            && fetched_at.elapsed() < self.ttl
        {
            return Ok(cached);
        }

        let url = format!("{AEL_API_URL}?line=AEL&sta={}&lang=en", station.code);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .context("requesting MTR Airport Express departures")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("reading the MTR Next Train response")?;
        if !status.is_success() {
            return Err(anyhow!(
                "MTR Next Train API returned HTTP {}: {}",
                status.as_u16(),
                body.chars().take(300).collect::<String>()
            ));
        }
        let parsed = parse_ael_body(&body, station, direction, hk_now())?;
        if !self.ttl.is_zero() {
            self.cache
                .lock()
                .expect("access cache lock poisoned")
                .insert(key, (Instant::now(), parsed.clone()));
        }
        Ok(parsed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckinAirline {
    pub iata: &'static str,
    pub icao: &'static str,
    pub name: &'static str,
    pub hong_kong_station: Option<&'static str>,
    pub kowloon_station: Option<&'static str>,
}

const CHECKIN_AIRLINES: [CheckinAirline; 7] = [
    CheckinAirline {
        iata: "CX",
        icao: "CPA",
        name: "Cathay Pacific",
        hong_kong_station: Some("06:00-23:00"),
        kowloon_station: Some("06:00-15:00"),
    },
    CheckinAirline {
        iata: "CI",
        icao: "CAL",
        name: "China Airlines",
        hong_kong_station: Some("09:00-18:00"),
        kowloon_station: None,
    },
    CheckinAirline {
        iata: "BR",
        icao: "EVA",
        name: "EVA Air",
        hong_kong_station: Some("08:00-14:00"),
        kowloon_station: None,
    },
    CheckinAirline {
        iata: "HX",
        icao: "CRK",
        name: "Hong Kong Airlines",
        hong_kong_station: Some("06:00-19:00"),
        kowloon_station: Some("06:00-15:00"),
    },
    CheckinAirline {
        iata: "AE",
        icao: "MDA",
        name: "Mandarin Airlines",
        hong_kong_station: Some("09:00-18:00"),
        kowloon_station: None,
    },
    CheckinAirline {
        iata: "QF",
        icao: "QFA",
        name: "Qantas Airways",
        hong_kong_station: Some("07:00-18:00"),
        kowloon_station: Some("07:00-15:00"),
    },
    CheckinAirline {
        iata: "SQ",
        icao: "SIA",
        name: "Singapore Airlines",
        hong_kong_station: Some("09:00-17:20"),
        kowloon_station: None,
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct CheckinStation {
    pub code: &'static str,
    pub name: &'static str,
    pub hours: Option<&'static str>,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckinResponse {
    pub mode: &'static str,
    pub query: String,
    pub participating: bool,
    pub airline: Option<CheckinAirline>,
    pub stations: Vec<CheckinStation>,
    pub check_in_window: &'static str,
    pub eligibility: Vec<&'static str>,
    pub baggage: &'static str,
    pub message: Option<String>,
    pub source: Source,
    pub checked_on: &'static str,
}

fn find_checkin_airline(query: &str) -> Option<CheckinAirline> {
    let normalized = query.trim().to_ascii_uppercase().replace([' ', '-'], "");
    CHECKIN_AIRLINES.iter().copied().find(|airline| {
        airline.iata == normalized
            || airline.icao == normalized
            || airline.name.to_ascii_uppercase().replace([' ', '-'], "") == normalized
    })
}

pub fn checkin(query: &str) -> CheckinResponse {
    let airline = find_checkin_airline(query);
    let (participating, stations) = match airline {
        Some(airline) => {
            let stations = vec![
                CheckinStation {
                    code: "HOK",
                    name: "Hong Kong",
                    hours: airline.hong_kong_station,
                    available: airline.hong_kong_station.is_some(),
                },
                CheckinStation {
                    code: "KOW",
                    name: "Kowloon",
                    hours: airline.kowloon_station,
                    available: airline.kowloon_station.is_some(),
                },
            ];
            (true, stations)
        }
        None => (false, Vec::new()),
    };
    CheckinResponse {
        mode: "in_town_checkin",
        query: query.trim().to_string(),
        participating,
        airline,
        stations,
        check_in_window: "24 hours to at least 90 minutes before scheduled departure, within service hours",
        eligibility: vec![
            "Airport Express passengers only.",
            "Flight must be operated by the participating airline; codeshares operated by another airline are not eligible.",
            "A valid electronic travel document is required.",
            "Service hours may vary with the airline's flight schedule; confirm with the airline.",
        ],
        baggage: "Maximum 145 cm (length) x 100 cm (width) x 85 cm (height) and 32 kg.",
        message: if participating {
            None
        } else {
            Some(format!(
                "{query} is not in MTR's published in-town check-in list (CX, CI, BR, HX, AE, QF, SQ). Check with the operating airline."
            ))
        },
        source: Source {
            name: "MTR in-town check-in service",
            url: CHECKIN_SOURCE,
        },
        checked_on: CHECKED_ON,
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BusRoute {
    pub route: &'static str,
    pub operator: &'static str,
    pub destination: &'static str,
    pub fare_hkd: f64,
    pub fare_note: Option<&'static str>,
    pub from_airport_hours: &'static str,
}

const BUS_ROUTES: [BusRoute; 29] = [
    BusRoute {
        route: "A10",
        operator: "Citybus",
        destination: "Ap Lei Chau",
        fare_hkd: 49.7,
        fare_note: None,
        from_airport_hours: "06:50-00:20",
    },
    BusRoute {
        route: "A11",
        operator: "Citybus",
        destination: "North Point Ferry Pier",
        fare_hkd: 41.9,
        fare_note: None,
        from_airport_hours: "05:35-00:30",
    },
    BusRoute {
        route: "A12",
        operator: "Citybus",
        destination: "Siu Sai Wan",
        fare_hkd: 47.1,
        fare_note: None,
        from_airport_hours: "06:00-00:15",
    },
    BusRoute {
        route: "A17",
        operator: "Citybus",
        destination: "Sham Wan",
        fare_hkd: 47.1,
        fare_note: None,
        from_airport_hours: "21:30-23:30",
    },
    BusRoute {
        route: "A20",
        operator: "Citybus",
        destination: "Ho Man Tin",
        fare_hkd: 34.6,
        fare_note: None,
        from_airport_hours: "12:00-24:00",
    },
    BusRoute {
        route: "A21",
        operator: "Citybus",
        destination: "Hung Hom Station",
        fare_hkd: 34.6,
        fare_note: None,
        from_airport_hours: "05:30-24:00",
    },
    BusRoute {
        route: "A22",
        operator: "Citybus",
        destination: "Lam Tin Station",
        fare_hkd: 40.8,
        fare_note: None,
        from_airport_hours: "05:20-00:10",
    },
    BusRoute {
        route: "A23",
        operator: "Citybus",
        destination: "Tsz Wan Shan (North)",
        fare_hkd: 40.8,
        fare_note: None,
        from_airport_hours: "11:10-00:10",
    },
    BusRoute {
        route: "A25",
        operator: "Citybus",
        destination: "Kai Tak",
        fare_hkd: 40.8,
        fare_note: None,
        from_airport_hours: "10:00-00:30",
    },
    BusRoute {
        route: "A26",
        operator: "Citybus",
        destination: "Yau Tong",
        fare_hkd: 44.0,
        fare_note: None,
        from_airport_hours: "11:35-23:35",
    },
    BusRoute {
        route: "A28",
        operator: "Citybus",
        destination: "LOHAS Park",
        fare_hkd: 44.0,
        fare_note: None,
        from_airport_hours: "11:20-00:20",
    },
    BusRoute {
        route: "A29",
        operator: "Citybus",
        destination: "Tseung Kwan O (Po Lam)",
        fare_hkd: 44.0,
        fare_note: None,
        from_airport_hours: "06:10-00:10",
    },
    BusRoute {
        route: "A30",
        operator: "Long Win",
        destination: "Lei Muk Shue",
        fare_hkd: 19.8,
        fare_note: None,
        from_airport_hours: "06:00-24:00",
    },
    BusRoute {
        route: "A31",
        operator: "Long Win",
        destination: "Tsuen Wan (Nina Tower)",
        fare_hkd: 19.8,
        fare_note: None,
        from_airport_hours: "05:30-24:00",
    },
    BusRoute {
        route: "A32",
        operator: "Long Win",
        destination: "Kwai Chung Estate",
        fare_hkd: 19.8,
        fare_note: None,
        from_airport_hours: "06:15-23:55",
    },
    BusRoute {
        route: "A33",
        operator: "Long Win",
        destination: "Tuen Mun Bus Interchange",
        fare_hkd: 15.7,
        fare_note: None,
        from_airport_hours: "07:50-24:00",
    },
    BusRoute {
        route: "A33X",
        operator: "Long Win",
        destination: "Tuen Mun (Fu Tai)",
        fare_hkd: 15.7,
        fare_note: None,
        from_airport_hours: "05:45-24:00",
    },
    BusRoute {
        route: "A34",
        operator: "Long Win",
        destination: "Hung Shui Kiu (Hung Yuen Road)",
        fare_hkd: 15.7,
        fare_note: None,
        from_airport_hours: "09:00-24:00",
    },
    BusRoute {
        route: "A35",
        operator: "New Lantao",
        destination: "Mui Wo",
        fare_hkd: 17.8,
        fare_note: Some("HK$30 on Sundays and public holidays"),
        from_airport_hours: "06:15, 06:40, 08:30, 18:15, 23:30",
    },
    BusRoute {
        route: "A36",
        operator: "Long Win",
        destination: "Kam Sheung Road Station",
        fare_hkd: 19.8,
        fare_note: None,
        from_airport_hours: "05:15-24:00",
    },
    BusRoute {
        route: "A37",
        operator: "Long Win",
        destination: "Long Ping Station",
        fare_hkd: 19.8,
        fare_note: None,
        from_airport_hours: "09:00-24:00",
    },
    BusRoute {
        route: "A38",
        operator: "Long Win",
        destination: "Tsuen Wan (Allway Gardens)",
        fare_hkd: 19.8,
        fare_note: None,
        from_airport_hours: "09:05-23:55",
    },
    BusRoute {
        route: "A41",
        operator: "Long Win",
        destination: "Shek Mun Estate",
        fare_hkd: 23.4,
        fare_note: None,
        from_airport_hours: "05:35-24:00",
    },
    BusRoute {
        route: "A41P",
        operator: "Long Win",
        destination: "Wu Kai Sha Station",
        fare_hkd: 29.0,
        fare_note: None,
        from_airport_hours: "07:05-24:00",
    },
    BusRoute {
        route: "A42",
        operator: "Long Win",
        destination: "Wong Nai Tau",
        fare_hkd: 23.4,
        fare_note: None,
        from_airport_hours: "10:00-24:00",
    },
    BusRoute {
        route: "A43",
        operator: "Long Win",
        destination: "Fanling (Luen Wo Hui)",
        fare_hkd: 32.4,
        fare_note: None,
        from_airport_hours: "06:30-24:00",
    },
    BusRoute {
        route: "A43P",
        operator: "Long Win",
        destination: "Fanling (Luen Wo Hui)",
        fare_hkd: 32.4,
        fare_note: None,
        from_airport_hours: "12:30-24:00",
    },
    BusRoute {
        route: "A46",
        operator: "Long Win",
        destination: "Fo Tan (Royal Ascot)",
        fare_hkd: 23.4,
        fare_note: None,
        from_airport_hours: "05:35-24:00",
    },
    BusRoute {
        route: "A47X",
        operator: "Long Win",
        destination: "Tai Wo",
        fare_hkd: 29.0,
        fare_note: None,
        from_airport_hours: "10:05-24:00",
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct BusResponse {
    pub mode: &'static str,
    pub route_count: usize,
    pub routes: Vec<BusRoute>,
    pub source: Source,
    pub checked_on: &'static str,
    pub note: &'static str,
}

pub fn bus(route: Option<&str>) -> Result<BusResponse> {
    let routes = match route {
        Some(input) => {
            let normalized = input.trim().to_ascii_uppercase().replace(' ', "");
            let found = BUS_ROUTES
                .iter()
                .copied()
                .filter(|route| route.route == normalized)
                .collect::<Vec<_>>();
            if found.is_empty() {
                return Err(anyhow!(
                    "unknown airport bus route '{input}'; try A11, A21, A31 or A41"
                ));
            }
            found
        }
        None => BUS_ROUTES.to_vec(),
    };
    Ok(BusResponse {
        mode: "airport_bus",
        route_count: routes.len(),
        routes,
        source: Source {
            name: "HKIA public buses",
            url: BUS_SOURCE,
        },
        checked_on: CHECKED_ON,
        note: "Times are the airport departure window from HKIA; city-to-airport times and frequencies differ. Fares are adult full fares; check the operator on the day.",
    })
}

pub fn ael_table(response: &AelResponse) -> String {
    let mut output = format!(
        "HKG Airport Express  |  {} {}  |  {}  |  fetched {} HKT\n",
        response.station.code,
        response.station.name,
        response.direction,
        time_part(&response.fetched_at)
    );
    if let Some(note) = &response.note {
        output.push_str(note);
        output.push('\n');
    } else {
        output.push_str("NEXT  DESTINATION       PLATFORM  DEPARTS  IN\n");
        for train in &response.trains {
            output.push_str(&format!(
                "{:<5} {:<17} {:<9} {:<8} {} min\n",
                train.sequence,
                train.destination_name,
                train.platform,
                time_part(&train.departs_at),
                train.departs_in_minutes
            ));
        }
    }
    if let Some(fare) = response
        .service
        .fares
        .iter()
        .find(|fare| fare.station == response.station.code)
    {
        let round_trip = fare
            .round_trip_30d_hkd
            .map(|value| format!(" / HK${value:.0} round trip"))
            .unwrap_or_default();
        output.push_str(&format!(
            "\nAdult HK${:.0} Octopus/card/QR, HK${:.0} single-ticket{round_trip}.\n",
            fare.adult_octopus_hkd, fare.adult_single_hkd
        ));
    }
    if let Some(journey) = response
        .service
        .journey_minutes
        .iter()
        .find(|journey| journey.from == response.station.code && journey.to == "AIR")
    {
        output.push_str(&format!(
            "Journey to Airport: about {} min; {}.\n",
            journey.minutes, response.service.frequency
        ));
    }
    if response.delayed {
        output.push_str("MTR reports a service delay; check the airport's train information.\n");
    }
    output.push_str(&format!(
        "\nSource: {} / timetable checked {}.\n",
        AEL_API_SOURCE, response.service.checked_on
    ));
    output
}

pub fn checkin_table(response: &CheckinResponse) -> String {
    let mut output = String::new();
    match &response.airline {
        Some(airline) => {
            output.push_str(&format!(
                "In-town check-in  |  {} ({}/{})\n",
                airline.name, airline.iata, airline.icao
            ));
            output.push_str("STATION       HOURS\n");
            for station in &response.stations {
                output.push_str(&format!(
                    "{:<13} {}\n",
                    station.name,
                    station.hours.unwrap_or("Currently not available")
                ));
            }
            output.push_str(&format!("\nWindow: {}.\n", response.check_in_window));
            for item in &response.eligibility {
                output.push_str(&format!("- {item}\n"));
            }
            output.push_str(&format!("Baggage: {}\n", response.baggage));
        }
        None => {
            output.push_str(&format!("In-town check-in  |  {}\n", response.query));
            if let Some(message) = &response.message {
                output.push_str(message);
                output.push('\n');
            }
        }
    }
    output.push_str(&format!(
        "\nSource: {} (checked {}).\n",
        response.source.url, response.checked_on
    ));
    output
}

pub fn bus_table(response: &BusResponse) -> String {
    let mut output = format!(
        "HKG airport buses  |  {} route(s)  |  hours from Airport\n",
        response.route_count
    );
    output
        .push_str("ROUTE  OPERATOR   DESTINATION                         FARE      FROM AIRPORT\n");
    for route in &response.routes {
        output.push_str(&format!(
            "{:<6} {:<10} {:<37} {:<8} {}\n",
            route.route,
            route.operator,
            truncate(route.destination, 37),
            fare_label(route.fare_hkd),
            route.from_airport_hours
        ));
        if let Some(note) = route.fare_note {
            output.push_str(&format!("       fare: {note}\n"));
        }
    }
    output.push_str(&format!("\n{}\n", response.note));
    output.push_str(&format!(
        "Source: {} (checked {}).\n",
        response.source.url, response.checked_on
    ));
    output
}

fn fare_label(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("HK${value:.0}")
    } else {
        format!("HK${value:.1}")
    }
}

fn time_part(timestamp: &str) -> String {
    timestamp.get(11..16).unwrap_or(timestamp).to_string()
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.to_string()
    } else {
        value
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

pub fn ael_csv(response: &AelResponse, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "sequence",
            "destination",
            "platform",
            "departs_at",
            "minutes",
        ])?;
    }
    for train in &response.trains {
        writer.write_record([
            train.sequence.to_string(),
            train.destination_name.to_string(),
            train.platform.clone(),
            train.departs_at.clone(),
            train.departs_in_minutes.to_string(),
        ])?;
    }
    finish_csv(writer)
}

pub fn checkin_csv(response: &CheckinResponse, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record(["station", "available", "hours"])?;
    }
    for station in &response.stations {
        writer.write_record([
            station.name.to_string(),
            station.available.to_string(),
            station.hours.unwrap_or("").to_string(),
        ])?;
    }
    finish_csv(writer)
}

pub fn bus_csv(response: &BusResponse, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "route",
            "operator",
            "destination",
            "fare_hkd",
            "fare_note",
            "from_airport_hours",
        ])?;
    }
    for route in &response.routes {
        writer.write_record([
            route.route.to_string(),
            route.operator.to_string(),
            route.destination.to_string(),
            route.fare_hkd.to_string(),
            route.fare_note.unwrap_or("").to_string(),
            route.from_airport_hours.to_string(),
        ])?;
    }
    finish_csv(writer)
}

fn finish_csv(writer: csv::Writer<Vec<u8>>) -> Result<String> {
    let bytes = writer
        .into_inner()
        .map_err(|error| anyhow!("writing CSV: {error}"))?;
    String::from_utf8(bytes).map_err(|error| anyhow!("CSV was not UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> DateTime<FixedOffset> {
        hk_offset()
            .with_ymd_and_hms(2026, 11, 22, 0, 10, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn parses_live_ael_response() {
        let body = r#"{
            "sys_time":"2026-11-22 00:10:10",
            "curr_time":"2026-11-22 00:10:10",
            "data":{"AEL-HOK":{"curr_time":"2026-11-22 00:10:10","UP":[
                {"seq":"1","dest":"AWE","plat":"1","time":"2026-11-22 00:11:00","ttnt":"1","valid":"Y"},
                {"seq":"2","dest":"AWE","plat":"1","time":"2026-11-22 00:24:00","ttnt":"13","valid":"Y"}
            ]}},
            "isdelay":"N","status":1,"message":"successful"
        }"#;
        let station = parse_station("HOK").unwrap();
        let response = parse_ael_body(body, station, AelDirection::ToAirport, fixed_now()).unwrap();
        assert_eq!(response.trains.len(), 2);
        assert_eq!(response.trains[0].destination_name, "AsiaWorld-Expo");
        assert_eq!(response.trains[0].departs_in_minutes, 1);
        assert!(!response.delayed);
    }

    #[test]
    fn airport_station_does_not_use_asiaworld_expo_as_to_airport() {
        let body = r#"{
            "data":{"AEL-AIR":{
                "UP":[{"seq":"1","dest":"AWE","plat":"1/3","time":"2026-11-22 00:14:00","ttnt":"0","valid":"Y"}],
                "DOWN":[{"seq":"1","dest":"HOK","plat":"2/4","time":"2026-11-22 00:14:00","ttnt":"0","valid":"Y"}]
            }},"status":1
        }"#;
        let station = parse_station("AIR").unwrap();
        let response = parse_ael_body(body, station, AelDirection::ToAirport, fixed_now()).unwrap();
        assert!(response.trains.is_empty());
        assert!(response.note.unwrap().contains("is the airport station"));
    }

    #[test]
    fn selects_both_directions_and_skips_invalid() {
        let body = r#"{
            "data":{"AEL-AIR":{
                "UP":[{"seq":"1","dest":"AWE","plat":"1/3","time":"2026-11-22 00:14:00","ttnt":"0","valid":"Y"}],
                "DOWN":[{"seq":"1","dest":"HOK","plat":"2/4","time":"2026-11-22 00:14:00","ttnt":"0","valid":"Y"},
                        {"seq":"2","dest":"HOK","plat":"2/4","time":"2026-11-22 00:26:00","ttnt":"13","valid":"N"}]
            }},
            "isdelay":"Y","status":1
        }"#;
        let station = parse_station("AIR").unwrap();
        let response = parse_ael_body(body, station, AelDirection::Both, fixed_now()).unwrap();
        assert_eq!(response.trains.len(), 2);
        assert!(response.delayed);
    }

    #[test]
    fn asiaworld_expo_to_airport_uses_down_trains() {
        let body = r#"{
            "data":{"AEL-AWE":{
                "DOWN":[{"seq":"1","dest":"HOK","plat":"1","time":"2026-11-22 00:14:00","ttnt":"0","valid":"Y"}]
            }},"status":1
        }"#;
        let station = parse_station("AWE").unwrap();
        let response = parse_ael_body(body, station, AelDirection::ToAirport, fixed_now()).unwrap();
        assert_eq!(response.trains.len(), 1);
        assert_eq!(response.trains[0].destination_code, "HOK");
    }

    #[test]
    fn checkin_matches_iata_and_icao() {
        let by_iata = checkin("CX");
        assert!(by_iata.participating);
        assert_eq!(by_iata.stations[1].hours, Some("06:00-15:00"));
        let by_icao = checkin("CPA");
        assert!(by_icao.participating);
        let unknown = checkin("ZZ");
        assert!(!unknown.participating);
        assert!(unknown.stations.is_empty());
    }

    #[test]
    fn bus_lookup_is_case_insensitive_and_has_special_fare_note() {
        assert_eq!(bus(None).unwrap().routes.len(), 29);
        let route = bus(Some(" a11 ")).unwrap();
        assert_eq!(route.routes.len(), 1);
        assert_eq!(route.routes[0].operator, "Citybus");
        let a35 = bus(Some("A35")).unwrap();
        assert_eq!(
            a35.routes[0].fare_note,
            Some("HK$30 on Sundays and public holidays")
        );
        assert!(bus(Some("Z99")).is_err());
    }

    #[test]
    fn station_aliases_and_direction_defaults() {
        let hok = parse_station("hong kong").unwrap();
        assert_eq!(hok.code, "HOK");
        assert_eq!(AelDirection::default_for(hok), AelDirection::ToAirport);
        let air = parse_station("airport").unwrap();
        assert_eq!(AelDirection::default_for(air), AelDirection::FromAirport);
        assert!(parse_station("not a station").is_none());
    }
}
