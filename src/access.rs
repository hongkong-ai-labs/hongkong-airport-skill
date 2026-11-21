//! Airport access: live Airport Express trains, in-town check-in reference,
//! airport buses and a small journey planner.
//!
//! Live train data comes from the MTR Next Train API on data.gov.hk. Live bus
//! ETAs come from the Citybus, Long Win/KMB and New Lantao operator APIs.
//! Check-in, fare and bus facts are embedded as dated reference data generated
//! from official pages and CSVs; every static response carries `checked_on`
//! and its source URLs.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::{
    DateTime, Duration as ChronoDuration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime,
    SecondsFormat, TimeZone, Timelike,
};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::api::{hk_now, hk_offset};

pub const AEL_API_URL: &str = "https://rt.data.gov.hk/v1/transport/mtr/getSchedule.php";
pub const AEL_API_SOURCE: &str = "https://data.gov.hk/en-data/dataset/mtr-data2-nexttrain-data";
pub const AEL_TIMETABLE_SOURCE: &str =
    "https://www.mtr.com.hk/en/customer/services/timetable_index.html";
pub const AEL_FREQUENCY_SOURCE: &str =
    "https://www.mtr.com.hk/en/customer/services/train_service_index.html";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub url: String,
}

fn source(name: &str, url: &str) -> Source {
    Source {
        name: name.to_string(),
        url: url.to_string(),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum BusDirection {
    #[value(name = "to-airport", alias = "to", alias = "airport")]
    ToAirport,
    #[value(name = "from-airport", alias = "from", alias = "city")]
    FromAirport,
    #[value(name = "both", alias = "all")]
    Both,
}

impl BusDirection {
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AelFare {
    pub from: String,
    pub to: String,
    pub adult_octopus_hkd: f64,
    pub adult_single_hkd: f64,
    pub child_octopus_hkd: f64,
    pub child_single_hkd: f64,
    pub round_trip_30d_hkd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct AelFareFile {
    checked_on: String,
    source: Source,
    fares: Vec<AelFare>,
}

static AEL_FARES: OnceLock<AelFareFile> = OnceLock::new();

fn ael_fare_file() -> &'static AelFareFile {
    AEL_FARES.get_or_init(|| {
        serde_json::from_str(include_str!("../data/access_ael_fares.json"))
            .expect("embedded access_ael_fares.json is valid")
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct AelService {
    pub frequency: &'static str,
    pub payment: &'static str,
    pub checked_on: String,
    pub journey_minutes: Vec<JourneyTime>,
    pub service_windows: Vec<ServiceWindow>,
    pub fares: Vec<AelFare>,
}

pub fn ael_service() -> AelService {
    let file = ael_fare_file();
    AelService {
        frequency: "About every 10 minutes",
        payment: "Octopus, contactless bank card (adult fare only), QR code, or Airport Express QR Code ticket",
        checked_on: file.checked_on.clone(),
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
        fares: file.fares.clone(),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AelStationStatus {
    pub station: &'static str,
    pub delayed: bool,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AelStatus {
    pub mode: &'static str,
    pub delayed: bool,
    pub checked_at: String,
    pub stations: Vec<AelStationStatus>,
    pub errors: Vec<String>,
    pub message: String,
    pub source: Source,
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

    let fare_source = ael_fare_file().source.clone();
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
            source("MTR Next Train API (data.gov.hk)", AEL_API_SOURCE),
            source("MTR Airport Express timetable", AEL_TIMETABLE_SOURCE),
            fare_source,
            source("MTR service hours and frequency", AEL_FREQUENCY_SOURCE),
        ],
    })
}

#[derive(Debug, Clone, Deserialize)]
struct CheckinFile {
    checked_on: String,
    source: Source,
    airlines: Vec<CheckinAirline>,
}

static CHECKIN: OnceLock<CheckinFile> = OnceLock::new();

fn checkin_file() -> &'static CheckinFile {
    CHECKIN.get_or_init(|| {
        serde_json::from_str(include_str!("../data/access_checkin.json"))
            .expect("embedded access_checkin.json is valid")
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CheckinAirline {
    pub iata: String,
    pub icao: String,
    pub name: String,
    pub hong_kong_station: Option<String>,
    pub kowloon_station: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckinStation {
    pub code: &'static str,
    pub name: &'static str,
    pub hours: Option<String>,
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
    pub checked_on: String,
}

fn find_checkin_airline(query: &str) -> Option<CheckinAirline> {
    let normalized = query.trim().to_ascii_uppercase().replace([' ', '-'], "");
    checkin_file()
        .airlines
        .iter()
        .find(|airline| {
            airline.iata == normalized
                || airline.icao == normalized
                || airline.name.to_ascii_uppercase().replace([' ', '-'], "") == normalized
        })
        .cloned()
}

pub fn checkin(query: &str) -> CheckinResponse {
    let airline = find_checkin_airline(query);
    let (participating, stations) = match &airline {
        Some(airline) => {
            let stations = vec![
                CheckinStation {
                    code: "HOK",
                    name: "Hong Kong",
                    hours: airline.hong_kong_station.clone(),
                    available: airline.hong_kong_station.is_some(),
                },
                CheckinStation {
                    code: "KOW",
                    name: "Kowloon",
                    hours: airline.kowloon_station.clone(),
                    available: airline.kowloon_station.is_some(),
                },
            ];
            (true, stations)
        }
        None => (false, Vec::new()),
    };
    let file = checkin_file();
    let names = file
        .airlines
        .iter()
        .map(|airline| airline.iata.as_str())
        .collect::<Vec<_>>()
        .join(", ");
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
                "{query} is not in MTR's published in-town check-in list ({names}). Check with the operating airline."
            ))
        },
        source: file.source.clone(),
        checked_on: file.checked_on.clone(),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LiveStop {
    pub stop_id: String,
    pub stop_name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusLiveStops {
    pub to_airport: Option<LiveStop>,
    pub from_airport: Option<LiveStop>,
    pub nlb_to_airport_route_id: Option<u64>,
    pub nlb_from_airport_route_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusRoute {
    pub route: String,
    pub operator: String,
    pub destination: String,
    pub to_airport_hours: Option<String>,
    pub from_airport_hours: Option<String>,
    pub fare_to_airport_hkd: Option<f64>,
    pub fare_from_airport_hkd: Option<f64>,
    pub fare_to_airport_note: Option<String>,
    pub fare_from_airport_note: Option<String>,
    pub notes_to_airport: Vec<String>,
    pub notes_from_airport: Vec<String>,
    pub route_url: String,
    pub live: BusLiveStops,
}

#[derive(Debug, Clone, Deserialize)]
struct BusFile {
    checked_on: String,
    source: Source,
    routes: Vec<BusRoute>,
}

static BUS_DATA: OnceLock<BusFile> = OnceLock::new();

fn bus_file() -> &'static BusFile {
    BUS_DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../data/access_bus.json"))
            .expect("embedded access_bus.json is valid")
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct BusEta {
    pub at: String,
    pub minutes: i64,
    pub destination: String,
    pub remark: Option<String>,
    pub wheelchair: Option<bool>,
    pub gps: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BusLive {
    pub direction: String,
    pub operator: String,
    pub stop_id: String,
    pub stop_name: String,
    pub available: bool,
    pub fetched_at: String,
    pub etas: Vec<BusEta>,
    pub error: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BusResponse {
    pub mode: &'static str,
    pub direction: String,
    pub route_count: usize,
    pub routes: Vec<BusRoute>,
    pub live: Vec<BusLive>,
    pub source: Source,
    pub checked_on: String,
    pub note: &'static str,
}

pub fn bus_with_direction(route: Option<&str>, direction: BusDirection) -> Result<BusResponse> {
    let file = bus_file();
    let routes = match route {
        Some(input) => {
            let normalized = input.trim().to_ascii_uppercase().replace(' ', "");
            let found = file
                .routes
                .iter()
                .filter(|route| route.route == normalized)
                .cloned()
                .collect::<Vec<_>>();
            if found.is_empty() {
                return Err(anyhow!(
                    "unknown airport bus route '{input}'; try A11, A21, A31 or A41"
                ));
            }
            found
        }
        None => file.routes.clone(),
    };
    Ok(BusResponse {
        mode: "airport_bus",
        direction: direction.as_str().to_string(),
        route_count: routes.len(),
        routes,
        live: Vec::new(),
        source: file.source.clone(),
        checked_on: file.checked_on.clone(),
        note: "Times are local departure windows. To-airport times are from the city terminus; from-airport times are from HKIA. Fares are adult weekday full fares; holiday notes are shown where published. Live ETAs are at the city terminus for to-airport and Airport GTC for from-airport.",
    })
}

#[derive(Debug, Deserialize)]
struct RawBusEta {
    eta: Option<String>,
    dir: Option<String>,
    dest_en: Option<String>,
    rmk_en: Option<String>,
    #[serde(rename = "wheelChair")]
    wheelchair: Option<i32>,
    #[serde(rename = "noGPS")]
    no_gps: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct BusEtaEnvelope {
    #[serde(default)]
    data: Vec<RawBusEta>,
}

#[derive(Debug, Deserialize)]
struct RawNlbEta {
    #[serde(rename = "estimatedArrivalTime")]
    at: String,
    #[serde(default)]
    departed: Option<i32>,
    #[serde(rename = "noGPS")]
    no_gps: Option<i32>,
    #[serde(rename = "wheelChair")]
    wheelchair: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct NlbEtaEnvelope {
    #[serde(default, rename = "estimatedArrivals")]
    estimated_arrivals: Vec<RawNlbEta>,
    #[serde(default)]
    message: String,
}

pub struct AccessClient {
    http: reqwest::Client,
    ttl: Duration,
    ael_cache: Mutex<HashMap<String, (Instant, AelResponse)>>,
    bus_cache: Mutex<HashMap<String, (Instant, BusLive)>>,
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
            ael_cache: Mutex::new(HashMap::new()),
            bus_cache: Mutex::new(HashMap::new()),
        })
    }

    pub async fn ael(&self, station: Station, direction: AelDirection) -> Result<AelResponse> {
        let key = format!("{}|{}", station.code, direction.as_str());
        if !self.ttl.is_zero()
            && let Some((fetched_at, cached)) = self
                .ael_cache
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
            self.ael_cache
                .lock()
                .expect("access cache lock poisoned")
                .insert(key, (Instant::now(), parsed.clone()));
        }
        Ok(parsed)
    }

    pub async fn ael_status(&self) -> Result<AelStatus> {
        let (hok, air) = tokio::join!(
            self.ael(
                parse_station("HOK").expect("HOK is a station"),
                AelDirection::ToAirport
            ),
            self.ael(
                parse_station("AIR").expect("AIR is a station"),
                AelDirection::FromAirport
            )
        );
        let mut stations = Vec::new();
        let mut errors = Vec::new();
        for (code, result) in [("HOK", hok), ("AIR", air)] {
            match result {
                Ok(response) => stations.push(AelStationStatus {
                    station: code,
                    delayed: response.delayed,
                    generated_at: response.generated_at,
                }),
                Err(error) => errors.push(format!("{code}: {error}")),
            }
        }
        if stations.is_empty() {
            return Err(anyhow!(
                "MTR line status unavailable: {}",
                errors.join("; ")
            ));
        }
        let delayed = stations.iter().any(|station| station.delayed);
        Ok(AelStatus {
            mode: "airport_express_status",
            delayed,
            checked_at: hk_now().to_rfc3339_opts(SecondsFormat::Secs, false),
            stations,
            errors,
            message: if delayed {
                "MTR reports an Airport Express delay.".to_string()
            } else {
                "No Airport Express delay reported.".to_string()
            },
            source: source("MTR Next Train API (data.gov.hk)", AEL_API_SOURCE),
        })
    }

    pub async fn bus_live(&self, route: &BusRoute, direction: BusDirection) -> Vec<BusLive> {
        let directions = match direction {
            BusDirection::Both => vec![BusDirection::ToAirport, BusDirection::FromAirport],
            one => vec![one],
        };
        let mut live = Vec::new();
        for one in directions {
            live.push(self.bus_live_one(route, one).await);
        }
        live
    }

    async fn bus_live_one(&self, route: &BusRoute, direction: BusDirection) -> BusLive {
        let key = format!("{}|{}", route.route, direction.as_str());
        if !self.ttl.is_zero()
            && let Some((fetched_at, cached)) = self
                .bus_cache
                .lock()
                .expect("access cache lock poisoned")
                .get(&key)
                .cloned()
            && fetched_at.elapsed() < self.ttl
        {
            return cached;
        }
        let live = self.fetch_bus_live(route, direction).await;
        if !self.ttl.is_zero() {
            self.bus_cache
                .lock()
                .expect("access cache lock poisoned")
                .insert(key, (Instant::now(), live.clone()));
        }
        live
    }

    async fn fetch_bus_live(&self, route: &BusRoute, direction: BusDirection) -> BusLive {
        let empty =
            |stop_id: String, stop_name: String, source_url: String, error: String| BusLive {
                direction: direction.as_str().to_string(),
                operator: route.operator.clone(),
                stop_id,
                stop_name,
                available: false,
                fetched_at: hk_now().to_rfc3339_opts(SecondsFormat::Secs, false),
                etas: Vec::new(),
                error: Some(error),
                source: source_url,
            };
        let (stop, source_url) = match direction {
            BusDirection::ToAirport => match &route.live.to_airport {
                Some(stop) => (stop, route.route_url.clone()),
                None => {
                    return empty(
                        String::new(),
                        String::new(),
                        route.route_url.clone(),
                        "No to-airport service is published for this route.".to_string(),
                    );
                }
            },
            BusDirection::FromAirport => match &route.live.from_airport {
                Some(stop) => (stop, route.route_url.clone()),
                None => {
                    return empty(
                        String::new(),
                        String::new(),
                        route.route_url.clone(),
                        "No from-airport service is published for this route.".to_string(),
                    );
                }
            },
            BusDirection::Both => unreachable!("both is expanded before fetching"),
        };

        let result = self.fetch_bus_etas(route, direction, stop).await;

        match result {
            Ok(etas) => BusLive {
                direction: direction.as_str().to_string(),
                operator: route.operator.clone(),
                stop_id: stop.stop_id.clone(),
                stop_name: stop.stop_name.clone(),
                available: !etas.is_empty(),
                fetched_at: hk_now().to_rfc3339_opts(SecondsFormat::Secs, false),
                etas,
                error: None,
                source: source_url,
            },
            Err(error) => empty(
                stop.stop_id.clone(),
                stop.stop_name.clone(),
                source_url,
                format!("Live ETA unavailable: {error}"),
            ),
        }
    }

    async fn fetch_bus_etas(
        &self,
        route: &BusRoute,
        direction: BusDirection,
        stop: &LiveStop,
    ) -> Result<Vec<BusEta>> {
        match route.operator.as_str() {
            "Citybus" | "Long Win" => {
                let url = if route.operator == "Citybus" {
                    format!(
                        "https://rt.data.gov.hk/v2/transport/citybus/eta/CTB/{}/{}",
                        stop.stop_id, route.route
                    )
                } else {
                    format!(
                        "https://data.etabus.gov.hk/v1/transport/kmb/eta/{}/{}/1",
                        stop.stop_id, route.route
                    )
                };
                let response = self.http.get(&url).send().await?;
                let status = response.status();
                let body = response.text().await?;
                if !status.is_success() {
                    return Err(anyhow!("ETA API {url} returned HTTP {}", status.as_u16()));
                }
                let envelope: BusEtaEnvelope = serde_json::from_str(&body)?;
                let expected_dir = match direction {
                    BusDirection::ToAirport => "O",
                    BusDirection::FromAirport => "I",
                    BusDirection::Both => "",
                };
                let mut etas = Vec::new();
                for eta in envelope.data {
                    if let Some(dir) = eta.dir.as_deref()
                        && dir != expected_dir
                    {
                        continue;
                    }
                    if let Some(parsed) = bus_eta(
                        eta.eta.as_deref(),
                        eta.dest_en.as_deref(),
                        eta.rmk_en.as_deref(),
                        eta.wheelchair,
                        eta.no_gps,
                        None,
                    ) {
                        etas.push(parsed);
                    }
                }
                etas.sort_by(|left, right| left.at.cmp(&right.at));
                Ok(etas)
            }
            "New Lantao" => {
                let route_id = match direction {
                    BusDirection::ToAirport => route.live.nlb_to_airport_route_id,
                    BusDirection::FromAirport => route.live.nlb_from_airport_route_id,
                    BusDirection::Both => None,
                };
                let Some(route_id) = route_id else {
                    return Err(anyhow!("No NLB route ID is available for this direction."));
                };
                let url = format!(
                    "https://rt.data.gov.hk/v2/transport/nlb/stop.php?action=estimatedArrivals&routeId={route_id}&stopId={}&language=en",
                    stop.stop_id
                );
                let response = self.http.get(&url).send().await?;
                let status = response.status();
                let body = response.text().await?;
                if !status.is_success() {
                    return Err(anyhow!("ETA API {url} returned HTTP {}", status.as_u16()));
                }
                let envelope: NlbEtaEnvelope = serde_json::from_str(&body)?;
                let mut etas = Vec::new();
                for eta in envelope.estimated_arrivals {
                    if eta.departed.unwrap_or(0) == 1 {
                        continue;
                    }
                    if let Some(parsed) = bus_eta(
                        Some(&eta.at),
                        None,
                        None,
                        eta.wheelchair,
                        eta.no_gps,
                        Some(route.destination.as_str()),
                    ) {
                        etas.push(parsed);
                    }
                }
                if etas.is_empty() && !envelope.message.trim().is_empty() {
                    return Err(anyhow!("{}", envelope.message));
                }
                etas.sort_by(|left, right| left.at.cmp(&right.at));
                Ok(etas)
            }
            other => Err(anyhow!("unsupported bus operator '{other}'")),
        }
    }
}

fn bus_eta(
    eta: Option<&str>,
    destination: Option<&str>,
    remark: Option<&str>,
    wheelchair: Option<i32>,
    no_gps: Option<i32>,
    fallback_destination: Option<&str>,
) -> Option<BusEta> {
    let eta = eta?;
    let at = parse_hk_datetime(eta).or_else(|| {
        NaiveDateTime::parse_from_str(eta.trim(), "%Y-%m-%d %H:%M:%S")
            .ok()
            .and_then(|naive| hk_offset().from_local_datetime(&naive).single())
    })?;
    let minutes = (at - hk_now()).num_minutes();
    if minutes < -1 {
        return None;
    }
    Some(BusEta {
        at: at.to_rfc3339_opts(SecondsFormat::Secs, false),
        minutes: minutes.max(0),
        destination: destination
            .filter(|value| !value.trim().is_empty())
            .or(fallback_destination)
            .unwrap_or("Unknown destination")
            .to_string(),
        remark: remark
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string()),
        wheelchair: wheelchair.map(|value| value == 1),
        gps: no_gps.map(|value| value == 0),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct TripFlight {
    pub date: String,
    pub departure_at: String,
    pub terminal: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TripAssumptions {
    pub check_in_cutoff_minutes: u32,
    pub buffer_minutes: u32,
    pub journey_minutes: u32,
    pub headway_minutes: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AelTrip {
    pub mode: &'static str,
    pub station: Station,
    pub airline: Option<CheckinAirline>,
    pub flight: TripFlight,
    pub assumptions: TripAssumptions,
    pub airport_arrival_deadline: String,
    pub latest_departure: Option<String>,
    pub arrival_at_airport: Option<String>,
    pub available: bool,
    pub in_town_checkin_hours: Option<String>,
    pub message: Option<String>,
    pub notes: Vec<String>,
    pub checked_on: String,
    pub sources: Vec<Source>,
}

pub fn parse_time(input: &str) -> Result<NaiveTime> {
    let input = input.trim();
    NaiveTime::parse_from_str(input, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(input, "%H%M"))
        .map_err(|_| anyhow!("invalid flight time '{input}'; use HH:MM (24-hour Hong Kong time)"))
}

fn minutes_of_day(value: &str) -> Option<u32> {
    let time = NaiveTime::parse_from_str(value, "%H:%M").ok()?;
    Some(time.hour() * 60 + time.minute())
}

fn within_service_window(time: NaiveTime, first: &str, last: &str) -> bool {
    let Some(first) = minutes_of_day(first) else {
        return false;
    };
    let Some(last) = minutes_of_day(last) else {
        return false;
    };
    let time = time.hour() * 60 + time.minute();
    if first <= last {
        time >= first && time <= last
    } else {
        time >= first || time <= last
    }
}

pub fn plan_ael_trip(
    station: Station,
    airline_query: Option<&str>,
    date: NaiveDate,
    time: NaiveTime,
    terminal: Option<String>,
    buffer_minutes: u32,
    now: DateTime<FixedOffset>,
) -> Result<AelTrip> {
    if !matches!(station.code, "HOK" | "KOW" | "TSY") {
        return Err(anyhow!(
            "the trip planner starts at HOK, KOW or TSY; AIR and AWE are airport-side stations"
        ));
    }
    let flight_naive = date.and_time(time);
    let flight_at = hk_offset()
        .from_local_datetime(&flight_naive)
        .single()
        .ok_or_else(|| anyhow!("could not resolve the flight time in Hong Kong time"))?;
    if flight_at <= now {
        return Err(anyhow!(
            "flight time {} is in the past; pass --date tomorrow or a future date",
            flight_at.to_rfc3339_opts(SecondsFormat::Secs, false)
        ));
    }

    let service = ael_service();
    let journey = service
        .journey_minutes
        .iter()
        .find(|journey| journey.from == station.code && journey.to == "AIR")
        .map(|journey| journey.minutes)
        .ok_or_else(|| anyhow!("no published AEL journey time for {}", station.code))?;
    let window = service
        .service_windows
        .iter()
        .find(|window| window.from == station.code && window.to == "AIR")
        .ok_or_else(|| anyhow!("no published AEL service window for {}", station.code))?;

    let check_in_cutoff = ChronoDuration::minutes(90 + i64::from(buffer_minutes));
    let airport_deadline = flight_at - check_in_cutoff;
    let target = airport_deadline - ChronoDuration::minutes(i64::from(journey));
    let offset = if station.code == "KOW" { 3 } else { 0 };
    let minute_of_day = target.hour() * 60 + target.minute();
    let delta = (minute_of_day as i64 - offset).rem_euclid(10);
    let latest = target - ChronoDuration::minutes(delta);
    let latest_time = latest.time();
    let available = within_service_window(latest_time, window.first, window.last);

    let airline = airline_query.and_then(|query| checkin(query).airline);
    let in_town_checkin_hours = airline.as_ref().and_then(|airline| {
        if station.code == "HOK" {
            airline.hong_kong_station.clone()
        } else if station.code == "KOW" {
            airline.kowloon_station.clone()
        } else {
            None
        }
    });

    let mut notes = vec![
        "Uses 90 minutes before scheduled departure as the airport check-in deadline; add --buffer for extra time.".to_string(),
        "The latest train is rounded down to the published 10-minute headway; late-night headway can be up to 12 minutes, so check the live AEL board.".to_string(),
        "No security, immigration, or walking time is included.".to_string(),
        "In-town check-in availability does not shorten the airport buffer in this plan.".to_string(),
    ];
    if let Some(terminal) = terminal.as_deref() {
        notes.push(format!(
            "Terminal {terminal} is shown for context; the Airport Express arrives at Airport Station."
        ));
    }
    if airline.is_none() && airline_query.is_some() {
        notes.push("Airline not found in MTR's published in-town check-in list.".to_string());
    }

    let message = if available {
        None
    } else {
        Some(format!(
            "The calculated latest departure {} is outside the published {} to Airport service window {}–{}. Use a taxi or an earlier train.",
            latest.format("%H:%M"),
            station.name,
            window.first,
            window.last
        ))
    };

    Ok(AelTrip {
        mode: "airport_express_trip",
        station,
        airline,
        flight: TripFlight {
            date: date.to_string(),
            departure_at: flight_at.to_rfc3339_opts(SecondsFormat::Secs, false),
            terminal,
        },
        assumptions: TripAssumptions {
            check_in_cutoff_minutes: 90,
            buffer_minutes,
            journey_minutes: journey,
            headway_minutes: 10,
        },
        airport_arrival_deadline: airport_deadline.to_rfc3339_opts(SecondsFormat::Secs, false),
        latest_departure: available.then(|| latest.to_rfc3339_opts(SecondsFormat::Secs, false)),
        arrival_at_airport: available.then(|| {
            (latest + ChronoDuration::minutes(i64::from(journey)))
                .to_rfc3339_opts(SecondsFormat::Secs, false)
        }),
        available,
        in_town_checkin_hours,
        message,
        notes,
        checked_on: service.checked_on.clone(),
        sources: vec![
            source("MTR Airport Express timetable", AEL_TIMETABLE_SOURCE),
            checkin_file().source.clone(),
        ],
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
        .find(|fare| fare.from == response.station.code && fare.to == "AIR")
    {
        let round_trip = fare
            .round_trip_30d_hkd
            .map(|value| format!(" / HK${value:.0} round trip"))
            .unwrap_or_default();
        output.push_str(&format!(
            "\nAdult HK${:.0} Octopus/card/QR, HK${:.0} single-ticket{round_trip}; child HK${:.1}/HK${:.1}.\n",
            fare.adult_octopus_hkd,
            fare.adult_single_hkd,
            fare.child_octopus_hkd,
            fare.child_single_hkd
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
                    station
                        .hours
                        .as_deref()
                        .unwrap_or("Currently not available")
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
    let both = response.direction == "both";
    let mut output = if both {
        format!(
            "HKG airport buses  |  {} route(s)  |  hours from HKIA and city\n",
            response.route_count
        )
    } else {
        format!(
            "HKG airport buses  |  {} route(s)  |  {}\n",
            response.route_count, response.direction
        )
    };
    if both {
        output.push_str("ROUTE  OPERATOR   DESTINATION                         TO AIRPORT       FROM AIRPORT     FARE\n");
    } else if response.direction == "to-airport" {
        output.push_str(
            "ROUTE  OPERATOR   DESTINATION                         TO AIRPORT       FARE\n",
        );
    } else {
        output.push_str(
            "ROUTE  OPERATOR   DESTINATION                         FROM AIRPORT     FARE\n",
        );
    }
    for route in &response.routes {
        let to_hours = route.to_airport_hours.as_deref().unwrap_or("-");
        let from_hours = route.from_airport_hours.as_deref().unwrap_or("-");
        let fare = match (route.fare_to_airport_hkd, route.fare_from_airport_hkd) {
            (Some(to), Some(from)) if (to - from).abs() > f64::EPSILON => {
                format!("{}/{}", fare_label(to), fare_label(from))
            }
            (Some(to), _) => fare_label(to),
            (_, Some(from)) => fare_label(from),
            _ => "-".to_string(),
        };
        if both {
            output.push_str(&format!(
                "{:<6} {:<10} {:<37} {:<15} {:<16} {}\n",
                route.route,
                route.operator,
                truncate(&route.destination, 37),
                to_hours,
                from_hours,
                fare
            ));
        } else if response.direction == "to-airport" {
            output.push_str(&format!(
                "{:<6} {:<10} {:<37} {:<15} {}\n",
                route.route,
                route.operator,
                truncate(&route.destination, 37),
                to_hours,
                fare
            ));
        } else {
            output.push_str(&format!(
                "{:<6} {:<10} {:<37} {:<16} {}\n",
                route.route,
                route.operator,
                truncate(&route.destination, 37),
                from_hours,
                fare
            ));
        }
        for note in if both {
            route
                .notes_to_airport
                .iter()
                .chain(route.notes_from_airport.iter())
                .collect::<Vec<_>>()
        } else if response.direction == "to-airport" {
            route.notes_to_airport.iter().collect()
        } else {
            route.notes_from_airport.iter().collect()
        } {
            output.push_str(&format!("       note: {note}\n"));
        }
        if let Some(note) = route.fare_to_airport_note.as_deref()
            && (both || response.direction == "to-airport")
        {
            output.push_str(&format!("       to-airport fare: {note}\n"));
        }
        if let Some(note) = route.fare_from_airport_note.as_deref()
            && (both || response.direction == "from-airport")
        {
            output.push_str(&format!("       from-airport fare: {note}\n"));
        }
    }
    if !response.live.is_empty() {
        output.push_str("\nLive ETAs\n");
        for live in &response.live {
            if let Some(error) = &live.error {
                output.push_str(&format!(
                    "{} at {} ({}): {error}\n",
                    live.direction, live.stop_name, live.operator
                ));
                continue;
            }
            if live.etas.is_empty() {
                output.push_str(&format!(
                    "{} at {} ({}): no live ETA returned\n",
                    live.direction, live.stop_name, live.operator
                ));
                continue;
            }
            output.push_str(&format!(
                "{} at {} ({}):\n",
                live.direction, live.stop_name, live.operator
            ));
            for eta in &live.etas {
                let remark = eta
                    .remark
                    .as_deref()
                    .map(|remark| format!(" [{remark}]"))
                    .unwrap_or_default();
                output.push_str(&format!(
                    "  {} ({} min) -> {}{}\n",
                    time_part(&eta.at),
                    eta.minutes,
                    eta.destination,
                    remark
                ));
            }
        }
    }
    output.push_str(&format!("\n{}\n", response.note));
    output.push_str(&format!(
        "Source: {} (checked {}).\n",
        response.source.url, response.checked_on
    ));
    output
}

pub fn trip_table(response: &AelTrip) -> String {
    let mut output = format!(
        "HKG Airport Express trip  |  {}  |  flight {} HKT\n",
        response.station.name,
        time_part(&response.flight.departure_at)
    );
    if let Some(terminal) = &response.flight.terminal {
        output.push_str(&format!("Terminal: {terminal}\n"));
    }
    if let Some(airline) = &response.airline {
        output.push_str(&format!(
            "Airline: {} ({}/{})\n",
            airline.name, airline.iata, airline.icao
        ));
    }
    output.push_str(&format!(
        "Airport deadline: {} (90 min cutoff + {} min buffer)\n",
        time_part(&response.airport_arrival_deadline),
        response.assumptions.buffer_minutes
    ));
    if let Some(latest) = &response.latest_departure {
        output.push_str(&format!(
            "Latest departure: {}  |  arrive Airport {}\n",
            time_part(latest),
            time_part(response.arrival_at_airport.as_deref().unwrap_or(""))
        ));
    } else if let Some(message) = &response.message {
        output.push_str(message);
        output.push('\n');
    }
    if let Some(hours) = &response.in_town_checkin_hours {
        output.push_str(&format!("In-town check-in at this station: {hours}\n"));
    }
    for note in &response.notes {
        output.push_str(&format!("- {note}\n"));
    }
    output.push_str(&format!("Checked: {}.\n", response.checked_on));
    output
}

pub fn status_table(status: &AelStatus) -> String {
    let state = if status.delayed { "DELAYED" } else { "normal" };
    let mut output = format!("Airport Express: {state}\n");
    for station in &status.stations {
        output.push_str(&format!(
            "- {}: {}\n",
            station.station,
            if station.delayed {
                "delay reported"
            } else {
                "normal"
            }
        ));
    }
    for error in &status.errors {
        output.push_str(&format!("- status error: {error}\n"));
    }
    output.push_str(&format!("{}\n", status.message));
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
            station.hours.clone().unwrap_or_default(),
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
            "fare_to_airport_hkd",
            "fare_from_airport_hkd",
            "to_airport_hours",
            "from_airport_hours",
            "fare_to_airport_note",
            "fare_from_airport_note",
            "notes",
        ])?;
    }
    for route in &response.routes {
        writer.write_record([
            route.route.clone(),
            route.operator.clone(),
            route.destination.clone(),
            route
                .fare_to_airport_hkd
                .map(|value| value.to_string())
                .unwrap_or_default(),
            route
                .fare_from_airport_hkd
                .map(|value| value.to_string())
                .unwrap_or_default(),
            route.to_airport_hours.clone().unwrap_or_default(),
            route.from_airport_hours.clone().unwrap_or_default(),
            route.fare_to_airport_note.clone().unwrap_or_default(),
            route.fare_from_airport_note.clone().unwrap_or_default(),
            route
                .notes_to_airport
                .iter()
                .chain(route.notes_from_airport.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("; "),
        ])?;
    }
    finish_csv(writer)
}

pub fn trip_csv(response: &AelTrip, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record([
            "station",
            "airline",
            "flight_date",
            "flight_departure_at",
            "terminal",
            "airport_arrival_deadline",
            "latest_departure",
            "arrival_at_airport",
            "available",
            "check_in_cutoff_minutes",
            "buffer_minutes",
            "journey_minutes",
            "headway_minutes",
            "in_town_checkin_hours",
            "checked_on",
        ])?;
    }
    writer.write_record([
        response.station.code.to_string(),
        response
            .airline
            .as_ref()
            .map(|airline| airline.iata.clone())
            .unwrap_or_default(),
        response.flight.date.clone(),
        response.flight.departure_at.clone(),
        response.flight.terminal.clone().unwrap_or_default(),
        response.airport_arrival_deadline.clone(),
        response.latest_departure.clone().unwrap_or_default(),
        response.arrival_at_airport.clone().unwrap_or_default(),
        response.available.to_string(),
        response.assumptions.check_in_cutoff_minutes.to_string(),
        response.assumptions.buffer_minutes.to_string(),
        response.assumptions.journey_minutes.to_string(),
        response.assumptions.headway_minutes.to_string(),
        response.in_town_checkin_hours.clone().unwrap_or_default(),
        response.checked_on.clone(),
    ])?;
    finish_csv(writer)
}

pub fn status_csv(status: &AelStatus, header: bool) -> Result<String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    if header {
        writer.write_record(["station", "delayed", "checked_at", "message"])?;
    }
    for station in &status.stations {
        writer.write_record([
            station.station.to_string(),
            station.delayed.to_string(),
            status.checked_at.clone(),
            status.message.clone(),
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
        assert_eq!(by_iata.stations[1].hours.as_deref(), Some("06:00-15:00"));
        let by_icao = checkin("CPA");
        assert!(by_icao.participating);
        let unknown = checkin("ZZ");
        assert!(!unknown.participating);
        assert!(unknown.stations.is_empty());
    }

    #[test]
    fn bus_lookup_has_both_directions_and_special_fare_note() {
        let response = bus_with_direction(None, BusDirection::Both).unwrap();
        assert_eq!(response.routes.len(), 30);
        let route = bus_with_direction(Some(" a11 "), BusDirection::Both).unwrap();
        assert_eq!(route.routes.len(), 1);
        assert_eq!(route.routes[0].operator, "Citybus");
        assert!(route.routes[0].to_airport_hours.is_some());
        assert!(route.routes[0].from_airport_hours.is_some());
        let a35 = bus_with_direction(Some("A35"), BusDirection::Both).unwrap();
        assert_eq!(
            a35.routes[0].fare_from_airport_note.as_deref(),
            Some("HK$30 on Sundays and public holidays")
        );
        assert_eq!(
            a35.routes[0].fare_to_airport_note.as_deref(),
            Some("HK$32.4 on Sundays and public holidays")
        );
        assert!(bus_with_direction(Some("Z99"), BusDirection::Both).is_err());
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

    #[test]
    fn trip_planner_uses_90_minutes_and_rounds_down() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let time = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
        let now = hk_offset()
            .with_ymd_and_hms(2026, 11, 22, 12, 0, 0)
            .single()
            .unwrap();
        let trip = plan_ael_trip(
            parse_station("HOK").unwrap(),
            Some("CX"),
            date,
            time,
            Some("T1".to_string()),
            0,
            now,
        )
        .unwrap();
        assert!(trip.available);
        assert_eq!(trip.assumptions.journey_minutes, 24);
        assert_eq!(time_part(&trip.airport_arrival_deadline), "08:00");
        assert_eq!(
            time_part(trip.latest_departure.as_deref().unwrap()),
            "07:30"
        );
        assert_eq!(
            trip.arrival_at_airport.as_deref().map(time_part),
            Some("07:54".to_string())
        );
    }

    #[test]
    fn trip_planner_honours_explicit_buffer() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let time = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
        let now = hk_offset()
            .with_ymd_and_hms(2026, 11, 22, 12, 0, 0)
            .single()
            .unwrap();
        let trip = plan_ael_trip(
            parse_station("KOW").unwrap(),
            None,
            date,
            time,
            None,
            10,
            now,
        )
        .unwrap();
        assert_eq!(time_part(&trip.airport_arrival_deadline), "07:50");
        assert_eq!(
            time_part(trip.latest_departure.as_deref().unwrap()),
            "07:23"
        );
    }

    #[test]
    fn status_csv_lists_each_station() {
        let generated_at = "2026-11-22T00:00:00+08:00".to_string();
        let status = AelStatus {
            mode: "airport_express_status",
            delayed: true,
            checked_at: generated_at.clone(),
            stations: vec![
                AelStationStatus {
                    station: "HOK",
                    delayed: false,
                    generated_at: generated_at.clone(),
                },
                AelStationStatus {
                    station: "AIR",
                    delayed: true,
                    generated_at,
                },
            ],
            errors: Vec::new(),
            message: "MTR reports an Airport Express delay.".to_string(),
            source: source("MTR Next Train API", AEL_API_SOURCE),
        };
        let csv = status_csv(&status, true).unwrap();
        assert!(csv.starts_with("station,delayed,checked_at,message"));
        assert_eq!(csv.lines().count(), 3);
        assert!(csv.contains("AIR,true"));
        assert!(status_csv(&status, false).unwrap().lines().count() == 2);
    }
}
