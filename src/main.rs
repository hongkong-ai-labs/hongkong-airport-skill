mod access;
mod analytics;
mod api;
mod mcp;
mod notify;
mod reference;
mod render;
mod runway;
mod store;
mod watchlist;
mod weather;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::SecondsFormat;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::json;

use api::{
    Client, Direction, FlightFilter, SortKey, build_alerts, compute_stats, fetch_flights,
    resolve_date,
};
use store::Store;
use watchlist::{Config, ItemKind, Watchlist};
use weather::WeatherClient;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    fn enabled(self) -> bool {
        match self {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => std::io::stdout().is_terminal(),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "hkia",
    version,
    about = "Live Hong Kong International Airport flight data — CLI and MCP server",
    arg_required_else_help = true
)]
struct Cli {
    /// Print machine-readable JSON instead of tables
    #[arg(long, global = true)]
    json: bool,

    /// Print CSV instead of a table
    #[arg(long, global = true)]
    csv: bool,

    /// Omit the CSV header row
    #[arg(long, global = true)]
    no_header: bool,

    /// Colorize table output
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,

    /// Cache API responses for N seconds (0 disables)
    #[arg(long, global = true, default_value_t = 20)]
    cache_ttl: u64,

    /// Override the history database path
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Override the watchlist config path
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Departures board (shortcut for `flights --direction departure`)
    #[command(alias = "dep")]
    Departures(CommonArgs),
    /// Arrivals board (shortcut for `flights --direction arrival`)
    #[command(alias = "arr")]
    Arrivals(CommonArgs),
    /// Flight board with an explicit direction
    Flights(FlightsArgs),
    /// Look up one flight number across departures and arrivals
    Flight(FlightArgs),
    /// Delayed and cancelled flights only
    Delays(DelaysArgs),
    /// One-shot disruption alerts (cancellations and delays)
    Alerts(AlertArgs),
    /// Poll the board and print changes as they happen
    Watch(WatchArgs),
    /// On-time performance, historical ranges, and per-hour statistics
    Stats(StatsArgs),
    /// Show the observed status/gate timeline for a flight
    History(HistoryArgs),
    /// Airport Express trains, status and trip planning, in-town check-in and buses
    Access(AccessArgs),
    /// Which runway is in use, with the airport wind
    Runway,
    /// Manage the targeted watchlist config
    Watchlist(WatchlistArgs),
    /// Run the MCP server over stdio, streamable HTTP or SSE
    Mcp(mcp::McpArgs),
}

#[derive(Args, Clone, Debug, Default)]
struct CommonArgs {
    /// Date: YYYY-MM-DD, today, yesterday or tomorrow (Hong Kong time)
    #[arg(short, long)]
    date: Option<String>,
    /// Filter by flight number, including codeshares (e.g. CX251)
    #[arg(long)]
    flight: Option<String>,
    /// Filter by destination/origin IATA code (e.g. LHR)
    #[arg(long)]
    airport: Option<String>,
    /// Filter by operating airline ICAO code (e.g. CPA)
    #[arg(long)]
    airline: Option<String>,
    /// Filter by terminal (T1, T2)
    #[arg(long)]
    terminal: Option<String>,
    /// Filter by state: delayed, cancelled, on_time, early, scheduled, disrupted
    #[arg(long)]
    status: Option<String>,
    /// Only flights delayed at least N minutes
    #[arg(long)]
    delayed_over: Option<i64>,
    /// Sort rows: time, delay, flight, state
    #[arg(long)]
    sort: Option<String>,
    /// Maximum rows; 0 means all
    #[arg(short, long, default_value_t = 50)]
    limit: usize,
    /// Read the cargo board
    #[arg(long)]
    cargo: bool,
    /// Response language: en, zh_HK, zh_CN
    #[arg(long, default_value = "en")]
    lang: String,
}

impl CommonArgs {
    fn filter(&self) -> Result<FlightFilter> {
        let sort = match self.sort.as_deref() {
            Some(value) => SortKey::parse(value).ok_or_else(|| {
                anyhow::anyhow!("unknown sort '{value}'; use time, delay, flight or state")
            })?,
            None => SortKey::Scheduled,
        };
        Ok(FlightFilter {
            flight: self.flight.clone(),
            airport: self.airport.clone(),
            airline: self.airline.clone(),
            terminal: self.terminal.clone(),
            state: self.status.clone(),
            min_delay: self.delayed_over,
            sort,
            limit: self.limit,
        })
    }
}

#[derive(Args)]
struct FlightsArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Board direction
    #[arg(short = 'D', long, value_enum, default_value_t = Direction::Departure)]
    direction: Direction,
}

#[derive(Args)]
struct FlightArgs {
    /// Flight number, e.g. CX251 or "CX 251"
    number: String,
    /// Date: YYYY-MM-DD, today, yesterday or tomorrow (Hong Kong time)
    #[arg(short, long)]
    date: Option<String>,
    /// Response language: en, zh_HK, zh_CN
    #[arg(long, default_value = "en")]
    lang: String,
    /// Search the cargo board
    #[arg(long)]
    cargo: bool,
}

#[derive(Args)]
struct DelaysArgs {
    /// Restrict to one board; omit for both
    #[arg(short = 'D', long, value_enum)]
    direction: Option<Direction>,
    /// Date: YYYY-MM-DD, today, yesterday or tomorrow (Hong Kong time)
    #[arg(short, long)]
    date: Option<String>,
    /// Minimum delay in minutes
    #[arg(long, default_value_t = 15)]
    min_delay: i64,
    /// Maximum rows; 0 means all
    #[arg(short, long, default_value_t = 100)]
    limit: usize,
    /// Read the cargo board
    #[arg(long)]
    cargo: bool,
    /// Response language: en, zh_HK, zh_CN
    #[arg(long, default_value = "en")]
    lang: String,
}

#[derive(Args)]
struct AlertArgs {
    /// Restrict to one board; omit for both
    #[arg(short = 'D', long, value_enum)]
    direction: Option<Direction>,
    /// Date: YYYY-MM-DD, today, yesterday or tomorrow (Hong Kong time)
    #[arg(short, long)]
    date: Option<String>,
    /// Minimum delay in minutes to flag
    #[arg(long, default_value_t = 15)]
    min_delay: i64,
    /// Exclude cancellations
    #[arg(long)]
    no_cancelled: bool,
    /// Restrict alerts to the configured watchlist
    #[arg(long)]
    watchlist: bool,
    /// Include Hong Kong Observatory weather warnings
    #[arg(long)]
    weather: bool,
    /// Skip the Airport Express line-status check
    #[arg(long)]
    no_ael: bool,
    /// Send a desktop notification when alerts are found
    #[arg(long)]
    notify: bool,
    /// POST alerts to this webhook URL
    #[arg(long)]
    webhook: Option<String>,
    /// Notify even if the same alert was already sent
    #[arg(long)]
    force_notify: bool,
    /// Maximum alerts; 0 means all
    #[arg(short, long, default_value_t = 50)]
    limit: usize,
    /// Read the cargo board
    #[arg(long)]
    cargo: bool,
    /// Response language: en, zh_HK, zh_CN
    #[arg(long, default_value = "en")]
    lang: String,
}

#[derive(Args)]
struct WatchArgs {
    /// Restrict to one board; omit for both
    #[arg(short = 'D', long, value_enum)]
    direction: Option<Direction>,
    #[command(flatten)]
    common: CommonArgs,
    /// Seconds between polls
    #[arg(long, default_value_t = 60)]
    interval: u64,
    /// Minimum delay in minutes to flag
    #[arg(long, default_value_t = 15)]
    min_delay: i64,
    /// Restrict alerts to the configured watchlist
    #[arg(long)]
    watchlist: bool,
    /// Send a desktop notification for each change batch
    #[arg(long)]
    notify: bool,
    /// POST changes to this webhook URL
    #[arg(long)]
    webhook: Option<String>,
    /// Show Hong Kong Observatory warnings at baseline
    #[arg(long)]
    weather: bool,
    /// Skip the Airport Express line-status checks
    #[arg(long)]
    no_ael: bool,
    /// Skip the runway-in-use checks
    #[arg(long)]
    no_runway: bool,
}

#[derive(Args)]
struct StatsArgs {
    /// Restrict to one board; omit for both
    #[arg(short = 'D', long, value_enum)]
    direction: Option<Direction>,
    /// Date: YYYY-MM-DD, today, yesterday or tomorrow (Hong Kong time)
    #[arg(short, long)]
    date: Option<String>,
    /// Start of a historical range (YYYY-MM-DD)
    #[arg(long)]
    from: Option<String>,
    /// End of a historical range; defaults to today
    #[arg(long)]
    to: Option<String>,
    /// Group range output: airline, route, hour or weekday
    #[arg(long)]
    group_by: Option<String>,
    /// Read the cargo board
    #[arg(long)]
    cargo: bool,
    /// Response language: en, zh_HK, zh_CN
    #[arg(long, default_value = "en")]
    lang: String,
}

#[derive(Args)]
struct HistoryArgs {
    /// Flight number, e.g. CX251
    number: String,
    /// Date to record and query (defaults to today)
    #[arg(short, long)]
    date: Option<String>,
    /// Maximum events to return
    #[arg(short, long, default_value_t = 100)]
    limit: usize,
    /// Read the cargo board
    #[arg(long)]
    cargo: bool,
    /// Response language: en, zh_HK, zh_CN
    #[arg(long, default_value = "en")]
    lang: String,
}

#[derive(Args)]
struct AccessArgs {
    #[command(subcommand)]
    command: AccessCommand,
}

#[derive(Subcommand)]
enum AccessCommand {
    /// Live Airport Express departures from an MTR station
    Ael(AelArgs),
    /// In-town check-in hours and eligibility for an airline
    Checkin(CheckinArgs),
    /// Airport A-route buses, fares and airport departure hours
    Bus(BusArgs),
    /// Latest Airport Express departure for a flight
    Trip(TripArgs),
    /// Live Airport Express line status
    Status,
}

#[derive(Args)]
struct AelArgs {
    /// MTR station: HOK, KOW, TSY, AIR or AWE (default HOK)
    #[arg(long, default_value = "HOK")]
    station: String,
    /// Direction; defaults to to-airport from HOK/KOW/TSY and from-airport at AIR/AWE
    #[arg(long, value_enum)]
    direction: Option<access::AelDirection>,
}

#[derive(Args)]
struct CheckinArgs {
    /// Airline IATA or ICAO code, or name (e.g. CX, CPA, Cathay Pacific)
    #[arg(long)]
    airline: String,
}

#[derive(Args)]
struct BusArgs {
    /// Restrict to one route, e.g. A11; required for live ETAs
    #[arg(long)]
    route: Option<String>,
    /// Direction: to-airport, from-airport or both (default both)
    #[arg(long, value_enum, default_value = "both")]
    direction: access::BusDirection,
    /// Skip live operator ETA lookups
    #[arg(long)]
    no_live: bool,
}

#[derive(Args)]
struct TripArgs {
    /// Flight departure time in Hong Kong time, HH:MM
    #[arg(long)]
    time: String,
    /// Flight date (defaults to today)
    #[arg(long)]
    date: Option<String>,
    /// Airline IATA/ICAO code or name, for in-town check-in hours
    #[arg(long)]
    airline: Option<String>,
    /// Departure terminal, shown for context only, e.g. T1
    #[arg(long)]
    terminal: Option<String>,
    /// Extra minutes on top of the 90-minute check-in cutoff
    #[arg(long, default_value_t = 0)]
    buffer: u32,
    /// City station to start from: HOK, KOW or TSY (default HOK)
    #[arg(long, default_value = "HOK")]
    station: String,
}

#[derive(Args)]
struct WatchlistArgs {
    #[command(subcommand)]
    command: WatchlistCommand,
}

#[derive(Subcommand)]
enum WatchlistCommand {
    /// Show the watchlist and config path
    Show,
    /// Add a flight, airport or airline
    Add(WatchItemArgs),
    /// Remove a flight, airport or airline
    Remove(WatchItemArgs),
    /// Change thresholds, direction, or notification settings
    Set(WatchSetArgs),
    /// Remove all watchlist selectors
    Clear,
}

#[derive(Args)]
struct WatchItemArgs {
    #[arg(long)]
    flight: Option<String>,
    #[arg(long)]
    airport: Option<String>,
    #[arg(long)]
    airline: Option<String>,
}

#[derive(Args)]
struct WatchSetArgs {
    #[arg(long)]
    min_delay: Option<i64>,
    #[arg(long, value_enum)]
    direction: Option<Direction>,
    #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
    include_cancelled: Option<bool>,
    #[arg(long)]
    webhook: Option<String>,
    #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
    desktop: Option<bool>,
}

fn make_client(cache_ttl: u64) -> Result<Arc<Client>> {
    Ok(Arc::new(Client::new(Duration::from_secs(cache_ttl))?))
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn direction_label(direction: Option<Direction>) -> &'static str {
    match direction {
        None => "flights",
        Some(Direction::Departure) => "departures",
        Some(Direction::Arrival) => "arrivals",
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.json && cli.csv {
        bail!("--json and --csv cannot be combined");
    }
    match cli.command {
        Command::Departures(ref args) => {
            run_board(&cli, args, Direction::Departure).await?;
        }
        Command::Arrivals(ref args) => {
            run_board(&cli, args, Direction::Arrival).await?;
        }
        Command::Flights(ref args) => {
            run_board(&cli, &args.common, args.direction).await?;
        }
        Command::Flight(ref args) => {
            run_flight(&cli, args).await?;
        }
        Command::Delays(ref args) => {
            run_delays(&cli, args).await?;
        }
        Command::Alerts(ref args) => {
            run_alerts(&cli, args).await?;
        }
        Command::Watch(ref args) => {
            run_watch(&cli, args).await?;
        }
        Command::Stats(ref args) => {
            run_stats(&cli, args).await?;
        }
        Command::History(ref args) => {
            run_history(&cli, args).await?;
        }
        Command::Access(ref args) => {
            run_access(&cli, args).await?;
        }
        Command::Runway => {
            run_runway(&cli).await?;
        }
        Command::Watchlist(ref args) => {
            run_watchlist(&cli, args)?;
        }
        Command::Mcp(ref args) => {
            mcp::run(
                args.clone(),
                cli.cache_ttl,
                cli.db.clone(),
                cli.config.clone(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn run_access(cli: &Cli, args: &AccessArgs) -> Result<()> {
    match &args.command {
        AccessCommand::Ael(ael) => {
            let station = access::parse_station(&ael.station).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown station '{}'; use HOK, KOW, TSY, AIR or AWE",
                    ael.station
                )
            })?;
            let direction = ael
                .direction
                .unwrap_or_else(|| access::AelDirection::default_for(station));
            let client = access::AccessClient::new(Duration::from_secs(cli.cache_ttl))?;
            let response = client.ael(station, direction).await?;
            if cli.json {
                print_json(&serde_json::to_value(&response)?)?;
            } else if cli.csv {
                print!("{}", access::ael_csv(&response, !cli.no_header)?);
            } else {
                print!("{}", access::ael_table(&response));
            }
        }
        AccessCommand::Checkin(checkin) => {
            let response = access::checkin(&checkin.airline);
            if cli.json {
                print_json(&serde_json::to_value(&response)?)?;
            } else if cli.csv {
                print!("{}", access::checkin_csv(&response, !cli.no_header)?);
            } else {
                print!("{}", access::checkin_table(&response));
            }
        }
        AccessCommand::Bus(bus) => {
            let mut response = access::bus_with_direction(bus.route.as_deref(), bus.direction)?;
            if !bus.no_live && bus.route.is_some() {
                let client = access::AccessClient::new(Duration::from_secs(cli.cache_ttl))?;
                for route in response.routes.clone() {
                    response
                        .live
                        .extend(client.bus_live(&route, bus.direction).await);
                }
            }
            if cli.json {
                print_json(&serde_json::to_value(&response)?)?;
            } else if cli.csv {
                print!("{}", access::bus_csv(&response, !cli.no_header)?);
            } else {
                print!("{}", access::bus_table(&response));
            }
        }
        AccessCommand::Trip(trip) => {
            let station = access::parse_station(&trip.station).ok_or_else(|| {
                anyhow::anyhow!("unknown station '{}'; use HOK, KOW or TSY", trip.station)
            })?;
            let date = resolve_date(trip.date.as_deref())?;
            let time = access::parse_time(&trip.time)?;
            let response = access::plan_ael_trip(
                station,
                trip.airline.as_deref(),
                date,
                time,
                trip.terminal.clone(),
                trip.buffer,
                api::hk_now(),
            )?;
            if cli.json {
                print_json(&serde_json::to_value(&response)?)?;
            } else if cli.csv {
                print!("{}", access::trip_csv(&response, !cli.no_header)?);
            } else {
                print!("{}", access::trip_table(&response));
            }
        }
        AccessCommand::Status => {
            let client = access::AccessClient::new(Duration::from_secs(cli.cache_ttl))?;
            let status = client.ael_status().await?;
            if cli.json {
                print_json(&serde_json::to_value(&status)?)?;
            } else if cli.csv {
                print!("{}", access::status_csv(&status, !cli.no_header)?);
            } else {
                print!("{}", access::status_table(&status));
            }
        }
    }
    Ok(())
}

async fn run_runway(cli: &Cli) -> Result<()> {
    let status = runway::fetch().await?;
    if cli.json {
        print_json(&serde_json::to_value(&status)?)?;
    } else if cli.csv {
        print!("{}", runway::runway_csv(&status, !cli.no_header)?);
    } else {
        print!("{}", runway::runway_table(&status));
    }
    Ok(())
}

async fn run_board(cli: &Cli, common: &CommonArgs, direction: Direction) -> Result<()> {
    let client = make_client(cli.cache_ttl)?;
    let date = resolve_date(common.date.as_deref())?;
    let board = client
        .board(date, direction, common.cargo, &common.lang)
        .await?;
    let flights = common.filter()?.apply(&board.flights);

    if cli.json {
        print_json(&json!({
            "date": board.date,
            "direction": board.direction,
            "cargo": board.cargo,
            "lastUpdated": board.last_updated,
            "fetchedAt": board.fetched_at,
            "totalOnBoard": board.flights.len(),
            "count": flights.len(),
            "flights": flights,
        }))?;
    } else if cli.csv {
        print!("{}", render::flights_csv(&flights, !cli.no_header)?);
    } else {
        print!(
            "{}",
            render::flights_table(
                &flights,
                &format!("{}s", direction.as_str()),
                &board.date,
                board.last_updated.as_deref(),
                board.flights.len(),
                cli.color.enabled(),
            )
        );
    }
    Ok(())
}

async fn run_flight(cli: &Cli, args: &FlightArgs) -> Result<()> {
    let client = make_client(cli.cache_ttl)?;
    let date = resolve_date(args.date.as_deref())?;
    let (departures, arrivals) = tokio::join!(
        client.board(date, Direction::Departure, args.cargo, &args.lang),
        client.board(date, Direction::Arrival, args.cargo, &args.lang)
    );

    let mut flights = Vec::new();
    let mut last_updated = None;
    let mut errors = Vec::new();
    match departures {
        Ok(board) => {
            last_updated = board.last_updated.clone();
            flights.extend(board.flights.iter().cloned());
        }
        Err(error) => errors.push(format!("departures: {error}")),
    }
    match arrivals {
        Ok(board) => {
            if last_updated.is_none() {
                last_updated = board.last_updated.clone();
            }
            flights.extend(board.flights.iter().cloned());
        }
        Err(error) => errors.push(format!("arrivals: {error}")),
    }
    if flights.is_empty() && !errors.is_empty() {
        bail!("could not fetch HKIA data: {}", errors.join("; "));
    }

    let filter = FlightFilter {
        flight: Some(args.number.clone()),
        limit: 0,
        ..FlightFilter::default()
    };
    let matches = filter.apply(&flights);
    if matches.is_empty() {
        bail!(
            "no flight matching '{}' on {} (Hong Kong time)",
            args.number,
            date
        );
    }

    if cli.json {
        print_json(&json!({
            "date": date.to_string(),
            "flightNumber": args.number,
            "count": matches.len(),
            "lastUpdated": last_updated,
            "flights": matches,
        }))?;
    } else if cli.csv {
        print!("{}", render::flights_csv(&matches, !cli.no_header)?);
    } else {
        print!(
            "{}",
            render::flights_table(
                &matches,
                "flight",
                &date.to_string(),
                last_updated.as_deref(),
                matches.len(),
                cli.color.enabled(),
            )
        );
        let guidance = render::guidance_lines(&matches);
        if !guidance.is_empty() {
            print!("\n{guidance}");
        }
    }
    Ok(())
}

async fn run_delays(cli: &Cli, args: &DelaysArgs) -> Result<()> {
    let client = make_client(cli.cache_ttl)?;
    let date = resolve_date(args.date.as_deref())?;
    let (flights, last_updated) =
        fetch_flights(&client, date, args.direction, args.cargo, &args.lang).await?;
    let filter = FlightFilter {
        state: Some("delayed".to_string()),
        min_delay: Some(args.min_delay),
        limit: args.limit,
        ..FlightFilter::default()
    };
    let delayed = filter.apply(&flights);

    if cli.json {
        print_json(&json!({
            "date": date.to_string(),
            "direction": args.direction.map(|direction| direction.as_str()).unwrap_or("both"),
            "lastUpdated": last_updated,
            "minDelayMinutes": args.min_delay,
            "count": delayed.len(),
            "flights": delayed,
        }))?;
    } else if cli.csv {
        print!("{}", render::flights_csv(&delayed, !cli.no_header)?);
    } else {
        print!(
            "{}",
            render::flights_table(
                &delayed,
                direction_label(args.direction),
                &date.to_string(),
                last_updated.as_deref(),
                flights.len(),
                cli.color.enabled(),
            )
        );
    }
    Ok(())
}

fn open_store(cli: &Cli) -> Result<Store> {
    let store = match &cli.db {
        Some(path) => Store::open(path),
        None => Store::open_default(),
    }?;
    let _ = store.prune(90);
    Ok(store)
}

fn load_config(cli: &Cli) -> Result<Config> {
    match &cli.config {
        Some(path) => Config::load_from(path),
        None => Config::load(),
    }
}

fn save_config(cli: &Cli, config: &Config) -> Result<()> {
    match &cli.config {
        Some(path) => config.save_to(path),
        None => config.save(),
    }
}

async fn fetch_weather(lang: &str, enabled: bool) -> Option<weather::WeatherReport> {
    if !enabled {
        return None;
    }
    match WeatherClient::new() {
        Ok(client) => match client.report(lang).await {
            Ok(report) => Some(report),
            Err(error) => {
                eprintln!("warning: weather context unavailable: {error}");
                None
            }
        },
        Err(error) => {
            eprintln!("warning: weather context unavailable: {error}");
            None
        }
    }
}

async fn run_alerts(cli: &Cli, args: &AlertArgs) -> Result<()> {
    let client = make_client(cli.cache_ttl)?;
    let date = resolve_date(args.date.as_deref())?;
    let (mut flights, last_updated) =
        fetch_flights(&client, date, args.direction, args.cargo, &args.lang).await?;
    let config = load_config(cli)?;
    let watchlist_used = if args.watchlist {
        if config.watchlist.is_empty() {
            bail!("watchlist is empty; add a flight, airport or airline with `hkia watchlist add`");
        }
        flights = config.watchlist.filter(&flights);
        Some(config.watchlist.clone())
    } else {
        None
    };
    let minimum = watchlist_used
        .as_ref()
        .map(|watchlist| watchlist.min_delay())
        .unwrap_or(args.min_delay);
    let include_cancelled = watchlist_used
        .as_ref()
        .map(|watchlist| watchlist.include_cancelled())
        .unwrap_or(!args.no_cancelled);
    let mut alerts = build_alerts(&flights, minimum, include_cancelled);
    let total = alerts.len();
    if args.limit > 0 {
        alerts.truncate(args.limit);
    }
    let weather_report = fetch_weather(&args.lang, args.weather).await;
    let ael_status = if args.no_ael {
        None
    } else {
        match access::AccessClient::new(Duration::from_secs(cli.cache_ttl)) {
            Ok(client) => client.ael_status().await.ok(),
            Err(error) => {
                eprintln!("warning: {error}");
                None
            }
        }
    };
    let webhook = args.webhook.clone().or(config.notify.webhook.clone());
    let desktop = args.notify || config.notify.desktop;
    let mut notified = 0usize;

    if desktop || webhook.is_some() {
        let store = open_store(cli)?;
        let selected: Vec<api::Alert> = alerts
            .iter()
            .filter(|alert| {
                args.force_notify
                    || store
                        .should_notify(&alert.key(), &alert.fingerprint())
                        .unwrap_or(true)
            })
            .cloned()
            .collect();
        if !selected.is_empty() {
            let mut delivered = false;
            if desktop {
                let body = selected
                    .iter()
                    .take(4)
                    .map(|alert| format!("{} {}", alert.flight_number, alert.kind))
                    .collect::<Vec<_>>()
                    .join("; ");
                match notify::desktop(&format!("HKG: {} alert(s)", selected.len()), &body).await {
                    Ok(()) => delivered = true,
                    Err(error) => eprintln!("warning: {error}"),
                }
            }
            if let Some(url) = webhook {
                let payload = json!({
                    "source": "hkia",
                    "version": 1,
                    "kind": "alerts",
                    "timestamp": api::hk_now().to_rfc3339_opts(SecondsFormat::Secs, false),
                    "date": date.to_string(),
                    "minDelayMinutes": minimum,
                    "watchlist": watchlist_used,
                    "weather": weather_report,
                    "airportExpress": ael_status,
                    "alerts": selected,
                });
                match notify::webhook(&url, &payload).await {
                    Ok(()) => delivered = true,
                    Err(error) => eprintln!("warning: {error}"),
                }
            }
            if delivered {
                let timestamp = api::hk_now().to_rfc3339_opts(SecondsFormat::Secs, false);
                for alert in &selected {
                    store.mark_notified(&alert.key(), &alert.fingerprint(), &timestamp)?;
                }
                notified = selected.len();
            }
        }
    }

    if cli.json {
        print_json(&json!({
            "date": date.to_string(),
            "direction": args.direction.map(|direction| direction.as_str()).unwrap_or("both"),
            "lastUpdated": last_updated,
            "minDelayMinutes": minimum,
            "includeCancelled": include_cancelled,
            "watchlist": watchlist_used,
            "weather": weather_report,
            "airportExpress": ael_status,
            "notified": notified,
            "totalAlerts": total,
            "count": alerts.len(),
            "alerts": alerts,
        }))?;
    } else if cli.csv {
        print!("{}", render::alerts_csv(&alerts, !cli.no_header)?);
    } else {
        print!(
            "{}",
            render::alerts_list(
                &alerts,
                direction_label(args.direction),
                &date.to_string(),
                last_updated.as_deref(),
            )
        );
        if let Some(weather) = &weather_report {
            print!("\n{}", render::weather_lines(weather));
        }
        if let Some(status) = &ael_status {
            print!("\n{}", access::status_table(status));
        }
        if notified > 0 {
            print!("\nNotified {notified} new alert(s).\n");
        }
    }
    Ok(())
}

async fn run_watch(cli: &Cli, args: &WatchArgs) -> Result<()> {
    let client = make_client(0)?;
    let date = resolve_date(args.common.date.as_deref())?;
    let config = load_config(cli)?;
    let watchlist_used = if args.watchlist {
        if config.watchlist.is_empty() {
            bail!("watchlist is empty; add a flight, airport or airline with `hkia watchlist add`");
        }
        Some(config.watchlist.clone())
    } else {
        None
    };
    let minimum = watchlist_used
        .as_ref()
        .map(|watchlist| watchlist.min_delay())
        .unwrap_or(args.min_delay);
    let include_cancelled = watchlist_used
        .as_ref()
        .map(|watchlist| watchlist.include_cancelled())
        .unwrap_or(true);
    let mut filter = args.common.filter()?;
    filter.limit = 0;
    let store = open_store(cli)?;
    let webhook = args.webhook.clone().or(config.notify.webhook.clone());
    let desktop = args.notify || config.notify.desktop;
    let mut baseline_taken = false;
    let interval = Duration::from_secs(args.interval.max(5));
    let weather_report = fetch_weather(&args.common.lang, args.weather).await;
    let access_client = if args.no_ael {
        None
    } else {
        match access::AccessClient::new(Duration::from_secs(30)) {
            Ok(client) => Some(client),
            Err(error) => {
                eprintln!("warning: {error}");
                None
            }
        }
    };
    let mut last_ael: Option<bool> = None;
    let mut last_runway: Option<String> = None;

    loop {
        let (flights, last_updated) = fetch_flights(
            &client,
            date,
            args.direction,
            args.common.cargo,
            &args.common.lang,
        )
        .await?;
        let current = match &watchlist_used {
            Some(watchlist) => watchlist.filter(&flights),
            None => filter.apply(&flights),
        };
        let now = api::hk_now().to_rfc3339_opts(SecondsFormat::Secs, false);
        let changes = store.record(&now, &current, minimum)?;
        let ael_status = match &access_client {
            Some(client) => client.ael_status().await.ok(),
            None => None,
        };
        let ael_delayed = ael_status.as_ref().map(|status| status.delayed);
        let ael_event = if baseline_taken {
            match (last_ael, ael_delayed) {
                (Some(previous), Some(current)) if previous != current => Some(if current {
                    "Airport Express delay reported"
                } else {
                    "Airport Express delay cleared"
                }),
                _ => None,
            }
        } else {
            None
        };
        last_ael = ael_delayed.or(last_ael);
        let runway_status = if args.no_runway {
            None
        } else {
            runway::fetch().await.ok()
        };
        let current_runway = runway_status.as_ref().and_then(runway::runway_summary);
        let runway_event = if baseline_taken {
            match (&last_runway, &current_runway) {
                (Some(previous), Some(current)) if previous != current => {
                    Some(format!("Runway in use changed: {previous} -> {current}"))
                }
                _ => None,
            }
        } else {
            None
        };
        last_runway = current_runway.or(last_runway);

        if !baseline_taken {
            let alerts = build_alerts(&current, minimum, include_cancelled);
            if cli.json {
                print_json(&json!({
                    "baseline": true,
                    "date": date.to_string(),
                    "direction": args.direction.map(|direction| direction.as_str()).unwrap_or("both"),
                    "lastUpdated": last_updated,
                    "trackedFlights": current.len(),
                    "watchlist": watchlist_used,
                    "weather": weather_report,
                    "airportExpress": ael_status,
                    "runway": runway_status,
                    "alerts": alerts,
                }))?;
            } else {
                print!(
                    "Watching HKG {} on {} — {} flights tracked, polling every {}s. Ctrl-C to stop.\n\n",
                    direction_label(args.direction),
                    date,
                    current.len(),
                    args.interval
                );
                print!(
                    "{}",
                    render::alerts_list(
                        &alerts,
                        direction_label(args.direction),
                        &date.to_string(),
                        last_updated.as_deref(),
                    )
                );
                if let Some(weather) = &weather_report {
                    print!("\n{}", render::weather_lines(weather));
                }
                if let Some(status) = &ael_status {
                    print!("\n{}", access::status_table(status));
                }
                if let Some(status) = &runway_status {
                    print!("\n{}", runway::runway_table(status));
                }
            }
            baseline_taken = true;
            if !changes.is_empty() {
                print!("{}", render::changes_list(&changes, &now));
            }
        } else if !changes.is_empty() {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "timestamp": now,
                        "watchlist": watchlist_used,
                        "airportExpress": ael_status,
                        "runway": runway_status,
                        "changes": changes,
                    }))?
                );
            } else {
                print!("{}", render::changes_list(&changes, &now));
            }

            if desktop || webhook.is_some() {
                let mut delivered = false;
                if desktop {
                    let body = changes
                        .iter()
                        .take(4)
                        .map(|change| format!("{} {}", change.flight_number, change.kind))
                        .collect::<Vec<_>>()
                        .join("; ");
                    match notify::desktop(&format!("HKG: {} change(s)", changes.len()), &body).await
                    {
                        Ok(()) => delivered = true,
                        Err(error) => eprintln!("warning: {error}"),
                    }
                }
                if let Some(url) = &webhook {
                    let payload = json!({
                        "source": "hkia",
                        "version": 1,
                        "kind": "changes",
                        "timestamp": now,
                        "date": date.to_string(),
                        "watchlist": watchlist_used,
                        "runway": runway_status,
                        "changes": changes,
                    });
                    match notify::webhook(url, &payload).await {
                        Ok(()) => delivered = true,
                        Err(error) => eprintln!("warning: {error}"),
                    }
                }
                if !delivered {
                    eprintln!("warning: no notification target accepted the change batch");
                }
            }
        }

        if let Some(event) = ael_event {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "timestamp": now,
                        "kind": "airport_express",
                        "airportExpress": ael_status,
                    }))?
                );
            } else {
                println!("{now}  {event}");
                if let Some(status) = &ael_status {
                    print!("{}", access::status_table(status));
                }
            }
            if desktop && let Err(error) = notify::desktop("HKG: Airport Express", event).await {
                eprintln!("warning: {error}");
            }
            if let Some(url) = &webhook {
                let payload = json!({
                    "source": "hkia",
                    "version": 1,
                    "kind": "airport_express",
                    "timestamp": now,
                    "airportExpress": ael_status,
                });
                if let Err(error) = notify::webhook(url, &payload).await {
                    eprintln!("warning: {error}");
                }
            }
        }

        if let Some(event) = runway_event {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "timestamp": now,
                        "kind": "runway",
                        "runway": runway_status,
                    }))?
                );
            } else {
                println!("{now}  {event}");
                if let Some(status) = &runway_status {
                    print!("{}", runway::runway_table(status));
                }
            }
            if desktop && let Err(error) = notify::desktop("HKG: Runway", &event).await {
                eprintln!("warning: {error}");
            }
            if let Some(url) = &webhook {
                let payload = json!({
                    "source": "hkia",
                    "version": 1,
                    "kind": "runway",
                    "timestamp": now,
                    "runway": runway_status,
                });
                if let Err(error) = notify::webhook(url, &payload).await {
                    eprintln!("warning: {error}");
                }
            }
        }

        tokio::time::sleep(interval).await;
    }
}

async fn run_stats(cli: &Cli, args: &StatsArgs) -> Result<()> {
    let client = make_client(cli.cache_ttl)?;
    if let Some(from) = &args.from {
        if let Some(name) = &args.group_by {
            let valid = matches!(
                name.trim().to_ascii_lowercase().as_str(),
                "airline"
                    | "airlines"
                    | "route"
                    | "routes"
                    | "hour"
                    | "hours"
                    | "weekday"
                    | "weekdays"
                    | "day"
            );
            if !valid {
                bail!("unknown group '{name}'; use airline, route, hour or weekday");
            }
        }
        let from = resolve_date(Some(from.as_str()))?;
        let to = resolve_date(args.to.as_deref())?;
        let report =
            analytics::range_report(&client, from, to, args.direction, args.cargo, &args.lang)
                .await?;
        if cli.json {
            print_json(&serde_json::to_value(&report)?)?;
        } else if cli.csv {
            print!(
                "{}",
                render::range_csv(&report, args.group_by.as_deref(), !cli.no_header)?
            );
        } else {
            print!("{}", render::range_table(&report, args.group_by.as_deref()));
        }
        return Ok(());
    }
    if args.group_by.is_some() {
        bail!("--group-by requires --from for a historical range");
    }

    let date = resolve_date(args.date.as_deref())?;
    let (flights, last_updated) =
        fetch_flights(&client, date, args.direction, args.cargo, &args.lang).await?;
    let stats = compute_stats(&flights);

    if cli.json {
        print_json(&json!({
            "date": date.to_string(),
            "direction": args.direction.map(|direction| direction.as_str()).unwrap_or("both"),
            "lastUpdated": last_updated,
            "stats": stats,
        }))?;
    } else if cli.csv {
        print!("{}", render::stats_csv(&stats, !cli.no_header)?);
    } else {
        print!(
            "{}",
            render::stats_table(
                &stats,
                direction_label(args.direction),
                &date.to_string(),
                last_updated.as_deref(),
            )
        );
    }
    Ok(())
}

fn selector(args: &WatchItemArgs) -> Result<(ItemKind, &str)> {
    let mut selected = Vec::new();
    if let Some(value) = &args.flight {
        selected.push((ItemKind::Flight, value.as_str()));
    }
    if let Some(value) = &args.airport {
        selected.push((ItemKind::Airport, value.as_str()));
    }
    if let Some(value) = &args.airline {
        selected.push((ItemKind::Airline, value.as_str()));
    }
    if selected.len() != 1 {
        bail!("provide exactly one of --flight, --airport or --airline");
    }
    Ok(selected.remove(0))
}

fn effective_config_path(cli: &Cli) -> PathBuf {
    cli.config.clone().unwrap_or_else(watchlist::config_path)
}

fn run_watchlist(cli: &Cli, args: &WatchlistArgs) -> Result<()> {
    let mut config = load_config(cli)?;
    match &args.command {
        WatchlistCommand::Show => {}
        WatchlistCommand::Add(item) => {
            let (kind, value) = selector(item)?;
            let added = config.watchlist.add(kind, value)?;
            save_config(cli, &config)?;
            if !added {
                eprintln!("already in the watchlist");
            }
        }
        WatchlistCommand::Remove(item) => {
            let (kind, value) = selector(item)?;
            if !config.watchlist.remove(kind, value) {
                bail!("not found in the watchlist");
            }
            save_config(cli, &config)?;
        }
        WatchlistCommand::Set(values) => {
            if let Some(min_delay) = values.min_delay {
                config.watchlist.min_delay = Some(min_delay);
            }
            if let Some(direction) = values.direction {
                config.watchlist.direction = Some(direction.as_str().to_string());
            }
            if let Some(include_cancelled) = values.include_cancelled {
                config.watchlist.include_cancelled = Some(include_cancelled);
            }
            if let Some(webhook) = &values.webhook {
                config.notify.webhook = Some(webhook.clone());
            }
            if let Some(desktop) = values.desktop {
                config.notify.desktop = desktop;
            }
            save_config(cli, &config)?;
        }
        WatchlistCommand::Clear => {
            config.watchlist = Watchlist::default();
            save_config(cli, &config)?;
        }
    }

    if cli.json {
        print_json(&json!({
            "path": effective_config_path(cli).display().to_string(),
            "watchlist": config.watchlist,
            "notify": config.notify,
        }))?;
    } else {
        println!("Config: {}", effective_config_path(cli).display());
        print!("{}", toml::to_string_pretty(&config)?);
        if config.watchlist.is_empty() {
            println!("\nWatchlist is empty. Add with: hkia watchlist add --flight CX251");
        }
    }
    Ok(())
}

async fn run_history(cli: &Cli, args: &HistoryArgs) -> Result<()> {
    let client = make_client(cli.cache_ttl)?;
    let date = resolve_date(args.date.as_deref())?;
    let (departures, arrivals) = tokio::join!(
        client.board(date, Direction::Departure, args.cargo, &args.lang),
        client.board(date, Direction::Arrival, args.cargo, &args.lang)
    );
    let mut flights = Vec::new();
    match departures {
        Ok(board) => flights.extend(board.flights.iter().cloned()),
        Err(error) => eprintln!("warning: departures unavailable: {error}"),
    }
    match arrivals {
        Ok(board) => flights.extend(board.flights.iter().cloned()),
        Err(error) => eprintln!("warning: arrivals unavailable: {error}"),
    }
    if flights.is_empty() {
        bail!("could not fetch HKIA boards for {date}");
    }

    let store = open_store(cli)?;
    let observed_at = api::hk_now().to_rfc3339_opts(SecondsFormat::Secs, false);
    store.record(&observed_at, &flights, 15)?;

    let filter = FlightFilter {
        flight: Some(args.number.clone()),
        limit: 0,
        ..FlightFilter::default()
    };
    let matches = filter.apply(&flights);
    let mut events = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for flight in &matches {
        for event in
            store.events_for_flight(&flight.flight_number, Some(&date.to_string()), args.limit)?
        {
            if seen.insert(event.id) {
                events.push(event);
            }
        }
    }
    events.sort_by(|left, right| {
        left.observed_at
            .cmp(&right.observed_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    if events.len() > args.limit {
        events = events.split_off(events.len() - args.limit);
    }

    if cli.json {
        print_json(&json!({
            "date": date.to_string(),
            "flightNumber": args.number,
            "matches": matches,
            "count": events.len(),
            "events": events,
        }))?;
    } else if cli.csv {
        print!("{}", render::history_csv(&events, !cli.no_header)?);
    } else {
        print!(
            "{}",
            render::history_table(&events, &args.number, &date.to_string())
        );
        if matches.is_empty() {
            println!("No current board match for '{}'.", args.number);
        }
    }
    Ok(())
}
