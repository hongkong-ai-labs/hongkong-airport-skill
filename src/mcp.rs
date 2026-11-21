//! MCP server exposing HKIA flight data over stdio, streamable HTTP, and legacy SSE.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::NaiveDate;
use clap::ValueEnum;
use futures::channel::mpsc::{Sender, channel};
use futures::{SinkExt, StreamExt};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    ClientJsonRpcMessage, Implementation, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ResourceUpdatedNotification, ResourceUpdatedNotificationParam,
    ServerCapabilities, ServerConfig, ServerJsonRpcMessage, ServerNotification,
    SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use crate::access;
use crate::api::{
    self, Client, Direction, Flight, FlightFilter, SortKey, build_alerts, compute_stats,
    fetch_flights,
};
use crate::runway;
use crate::store::{Store, StoredEvent};
use crate::watchlist::Config;
use crate::weather::WeatherClient;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum McpTransport {
    Stdio,
    Http,
    Sse,
}

#[derive(Debug, Clone, clap::Args)]
pub struct McpArgs {
    /// Transport to serve: stdio, http (streamable HTTP) or sse (legacy SSE).
    #[arg(long, value_enum, default_value_t = McpTransport::Stdio)]
    pub transport: McpTransport,
    /// Bind address for http/sse transports.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Bind port for http/sse transports.
    #[arg(long, default_value_t = 8787)]
    pub port: u16,
    /// Streamable HTTP endpoint path.
    #[arg(long, default_value = "/mcp")]
    pub path: String,
    /// Require this API key on HTTP/SSE routes (alias: --api-key).
    #[arg(long, alias = "api-key")]
    pub auth_token: Option<String>,
    /// Maximum HTTP/SSE requests per minute; 0 disables. Default 240.
    #[arg(long, default_value_t = 240)]
    pub rate_limit: u32,
}

#[derive(Clone)]
struct AuthConfig {
    key: Option<String>,
}

#[derive(Clone)]
struct RateConfig {
    limit: u32,
    window: Arc<Mutex<(Instant, u32)>>,
}

async fn require_auth(State(config): State<AuthConfig>, request: Request, next: Next) -> Response {
    let Some(expected) = config.key.as_deref() else {
        return next.run(request).await;
    };
    if request.method() == axum::http::Method::OPTIONS {
        return next.run(request).await;
    }
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()))
        || request
            .headers()
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()));
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized",
        )
            .into_response()
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

async fn rate_limit(State(config): State<RateConfig>, request: Request, next: Next) -> Response {
    if config.limit == 0 {
        return next.run(request).await;
    }
    let allowed = {
        let mut window = config.window.lock().expect("rate limit lock poisoned");
        if window.0.elapsed() >= Duration::from_secs(60) {
            window.0 = Instant::now();
            window.1 = 0;
        }
        if window.1 >= config.limit {
            false
        } else {
            window.1 += 1;
            true
        }
    };
    if !allowed {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            "rate limit exceeded",
        )
            .into_response();
    }
    next.run(request).await
}

pub async fn run(
    args: McpArgs,
    cache_ttl: u64,
    db_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> Result<()> {
    let client = Arc::new(Client::new(Duration::from_secs(cache_ttl))?);
    match args.transport {
        McpTransport::Stdio => {
            eprintln!("hkia MCP server listening on stdio");
            let server = HkiaServer::new(client, db_path.as_deref(), config_path.as_deref())?;
            let service = server.serve(stdio()).await?;
            service.waiting().await?;
            Ok(())
        }
        McpTransport::Http => serve_http(args, client, db_path, config_path).await,
        McpTransport::Sse => serve_sse(args, client, db_path, config_path).await,
    }
}

async fn serve_http(
    args: McpArgs,
    client: Arc<Client>,
    db_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> Result<()> {
    if !args.path.starts_with('/') {
        bail!("--path must start with '/'");
    }
    let auth = AuthConfig {
        key: args
            .auth_token
            .clone()
            .or_else(|| std::env::var("HKIA_MCP_API_KEY").ok())
            .or_else(|| std::env::var("HKIA_API_KEY").ok())
            .or_else(|| std::env::var("HKIA_AUTH_TOKEN").ok())
            .filter(|value| !value.trim().is_empty()),
    };
    if auth.key.is_some() {
        eprintln!("hkia MCP: API-key authentication enabled");
    }
    let rate = RateConfig {
        limit: args.rate_limit,
        window: Arc::new(Mutex::new((Instant::now(), 0))),
    };
    let cancellation = CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancellation.child_token())
        .with_allowed_hosts([
            args.host.clone(),
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "0.0.0.0".to_string(),
        ]);

    let service = StreamableHttpService::new(
        {
            let client = client.clone();
            let db_path = db_path.clone();
            let config_path = config_path.clone();
            move || {
                HkiaServer::new(client.clone(), db_path.as_deref(), config_path.as_deref())
                    .map_err(|error| std::io::Error::other(error.to_string()))
            }
        },
        LocalSessionManager::default().into(),
        config,
    );

    let protected = Router::new()
        .nest_service(&args.path, service)
        .layer(middleware::from_fn_with_state(rate, rate_limit))
        .layer(middleware::from_fn_with_state(auth, require_auth));
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(protected)
        .layer(CorsLayer::permissive());

    let address: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", args.host, args.port))?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!(
        "hkia MCP server (streamable HTTP) listening on http://{address}{}",
        args.path
    );

    let shutdown_token = cancellation.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown_token.cancel();
        })
        .await?;
    Ok(())
}

#[derive(Clone)]
struct SseState {
    server: HkiaServer,
    sessions: Arc<Mutex<HashMap<String, Sender<ClientJsonRpcMessage>>>>,
}

struct SessionGuard {
    id: String,
    sessions: Arc<Mutex<HashMap<String, Sender<ClientJsonRpcMessage>>>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&self.id);
        }
    }
}

async fn serve_sse(
    args: McpArgs,
    client: Arc<Client>,
    db_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> Result<()> {
    let state = SseState {
        server: HkiaServer::new(client, db_path.as_deref(), config_path.as_deref())?,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let auth = AuthConfig {
        key: args
            .auth_token
            .clone()
            .or_else(|| std::env::var("HKIA_MCP_API_KEY").ok())
            .or_else(|| std::env::var("HKIA_API_KEY").ok())
            .or_else(|| std::env::var("HKIA_AUTH_TOKEN").ok())
            .filter(|value| !value.trim().is_empty()),
    };
    let rate = RateConfig {
        limit: args.rate_limit,
        window: Arc::new(Mutex::new((Instant::now(), 0))),
    };
    let protected = Router::new()
        .route("/sse", get(sse_handler))
        .route("/messages", post(sse_post))
        .layer(middleware::from_fn_with_state(rate, rate_limit))
        .layer(middleware::from_fn_with_state(auth, require_auth));
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(protected)
        .layer(CorsLayer::permissive())
        .with_state(state);

    let address: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", args.host, args.port))?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!("hkia MCP server (SSE) listening on http://{address}/sse");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn sse_handler(
    State(state): State<SseState>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let id = uuid::Uuid::new_v4().to_string();
    let (out_tx, out_rx) = channel::<ServerJsonRpcMessage>(64);
    let (in_tx, in_rx) = channel::<ClientJsonRpcMessage>(64);

    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.insert(id.clone(), in_tx);
    }

    let server = state.server.clone();
    let session_id = id.clone();
    tokio::spawn(async move {
        match server.serve((out_tx, in_rx)).await {
            Ok(running) => {
                if let Err(error) = running.waiting().await {
                    eprintln!("sse session {session_id} ended: {error}");
                }
            }
            Err(error) => eprintln!("sse session {session_id} failed: {error}"),
        }
    });

    let endpoint = Event::default()
        .event("endpoint")
        .data(format!("/messages?sessionId={id}"));
    let guard = SessionGuard {
        id,
        sessions: state.sessions.clone(),
    };

    let messages = futures::stream::unfold((out_rx, guard), |(mut receiver, guard)| async move {
        let message = receiver.next().await?;
        let event = match serde_json::to_string(&message) {
            Ok(data) => Event::default().event("message").data(data),
            Err(error) => {
                eprintln!("failed to serialise an MCP message: {error}");
                Event::default().comment("serialisation error")
            }
        };
        Some((Ok::<Event, Infallible>(event), (receiver, guard)))
    });

    Sse::new(
        futures::stream::once(async move { Ok::<Event, Infallible>(endpoint) }).chain(messages),
    )
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(25)))
}

#[derive(Debug, Deserialize)]
struct SsePostQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

async fn sse_post(
    State(state): State<SseState>,
    Query(query): Query<SsePostQuery>,
    body: Bytes,
) -> StatusCode {
    let Ok(message) = serde_json::from_slice::<ClientJsonRpcMessage>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let sender = state
        .sessions
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(&query.session_id).cloned());
    let Some(mut sender) = sender else {
        return StatusCode::NOT_FOUND;
    };
    match sender.send(message).await {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

#[derive(Clone)]
pub struct HkiaServer {
    client: Arc<Client>,
    fresh: Arc<Client>,
    store: Arc<Store>,
    weather: Arc<WeatherClient>,
    access: Arc<access::AccessClient>,
    config_path: Option<PathBuf>,
    subscriptions: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListFlightsArgs {
    #[schemars(description = "Board to read: 'departure' (default) or 'arrival'")]
    direction: Option<String>,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(
        description = "Filter by destination (departures) or origin (arrivals) IATA code, e.g. LHR"
    )]
    airport: Option<String>,
    #[schemars(description = "Filter by operating airline ICAO code, e.g. CPA for Cathay Pacific")]
    airline: Option<String>,
    #[schemars(description = "Filter by terminal, e.g. T1 or T2")]
    terminal: Option<String>,
    #[schemars(
        description = "Filter by state: delayed, cancelled, on_time, early, scheduled, disrupted"
    )]
    status: Option<String>,
    #[schemars(description = "Only include flights delayed by at least this many minutes")]
    delayed_over: Option<i64>,
    #[schemars(description = "Maximum number of flights to return; 0 returns all. Default 50")]
    limit: Option<usize>,
    #[schemars(description = "Sort by time (default), delay, flight or state")]
    sort: Option<String>,
    #[schemars(description = "Read the cargo board instead of the passenger board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchFlightArgs {
    #[schemars(
        description = "Flight number such as CX251 or 'CX 251'; codeshares are matched too"
    )]
    flight_number: String,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to search both boards")]
    direction: Option<String>,
    #[schemars(description = "Search the cargo board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DelaysArgs {
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to search both boards")]
    direction: Option<String>,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Minimum delay in minutes to report. Default 15")]
    min_delay_minutes: Option<i64>,
    #[schemars(description = "Maximum number of flights to return; 0 returns all. Default 100")]
    limit: Option<usize>,
    #[schemars(description = "Read the cargo board instead of the passenger board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AlertsArgs {
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to cover both boards")]
    direction: Option<String>,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Minimum delay in minutes to flag. Default 15")]
    min_delay_minutes: Option<i64>,
    #[schemars(description = "Include cancellations. Default true")]
    include_cancelled: Option<bool>,
    #[schemars(description = "Include Hong Kong Observatory weather warnings")]
    include_weather: Option<bool>,
    #[schemars(
        description = "Include the live Airport Express delay flag from the MTR Next Train API. Default true"
    )]
    include_ael: Option<bool>,
    #[schemars(description = "Maximum number of alerts to return; 0 returns all. Default 50")]
    limit: Option<usize>,
    #[schemars(description = "Read the cargo board instead of the passenger board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StatsArgs {
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to cover both boards")]
    direction: Option<String>,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Read the cargo board instead of the passenger board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WatchlistAlertsArgs {
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to cover both boards")]
    direction: Option<String>,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Override the watchlist minimum delay in minutes")]
    min_delay_minutes: Option<i64>,
    #[schemars(description = "Include cancellations")]
    include_cancelled: Option<bool>,
    #[schemars(description = "Include Hong Kong Observatory weather warnings")]
    include_weather: Option<bool>,
    #[schemars(description = "Maximum alerts to return; 0 returns all. Default 50")]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FlightHistoryArgs {
    #[schemars(description = "Flight number such as CX251 or 'CX 251'")]
    flight_number: String,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Maximum events to return. Default 100")]
    limit: Option<usize>,
    #[schemars(description = "Read the cargo board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct HistoricalStatsArgs {
    #[schemars(description = "Start date of the range (YYYY-MM-DD), up to 91 days ago")]
    from: String,
    #[schemars(description = "End date of the range (YYYY-MM-DD); defaults to today")]
    to: Option<String>,
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to cover both boards")]
    direction: Option<String>,
    #[schemars(description = "Focus group: airline, route, hour or weekday")]
    group_by: Option<String>,
    #[schemars(description = "Maximum rows per group. Default 25")]
    limit: Option<usize>,
    #[schemars(description = "Read the cargo board")]
    cargo: Option<bool>,
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitForChangesArgs {
    #[schemars(
        description = "Only return events newer than this RFC 3339 timestamp. Defaults to just before the call."
    )]
    after: Option<String>,
    #[schemars(description = "Maximum seconds to wait. Default 30, maximum 120")]
    timeout_seconds: Option<u64>,
    #[schemars(
        description = "Date as YYYY-MM-DD or today/yesterday/tomorrow. Defaults to today in Hong Kong."
    )]
    date: Option<String>,
    #[schemars(description = "Restrict to 'departure' or 'arrival'; omit to cover both boards")]
    direction: Option<String>,
    #[schemars(description = "Minimum delay in minutes to record. Default 15")]
    min_delay_minutes: Option<i64>,
    #[schemars(description = "Only watch flights matching the local watchlist config")]
    watchlist_only: Option<bool>,
    #[schemars(description = "Read the cargo board")]
    cargo: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WeatherArgs {
    #[schemars(description = "Response language: en (default), zh_HK or zh_CN")]
    lang: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AirportAccessArgs {
    #[schemars(
        description = "Access mode: airport-express (live AEL trains), airport-express-status (delay flag), in-town-checkin (dated MTR hours), airport-bus (dated A-route info plus live ETAs when route is set) or trip (latest AEL departure for a flight)"
    )]
    mode: String,
    #[schemars(description = "AEL station: HOK, KOW, TSY, AIR or AWE. Default HOK.")]
    station: Option<String>,
    #[schemars(
        description = "AEL direction: to-airport, from-airport or both. Inferred from the station when omitted."
    )]
    direction: Option<String>,
    #[schemars(
        description = "Airline IATA/ICAO code or name for in-town check-in, e.g. CX, CPA or Cathay Pacific"
    )]
    airline: Option<String>,
    #[schemars(
        description = "Airport bus route for airport-bus mode, e.g. A11; omit for all routes. Live ETAs are returned only for a single route."
    )]
    route: Option<String>,
    #[schemars(
        description = "Flight departure time in Hong Kong time, HH:MM, for trip mode (required there)"
    )]
    time: Option<String>,
    #[schemars(
        description = "Flight date as YYYY-MM-DD or today/yesterday/tomorrow for trip mode. Defaults to today."
    )]
    date: Option<String>,
    #[schemars(description = "Departure terminal for trip mode, shown for context only, e.g. T1")]
    terminal: Option<String>,
    #[schemars(
        description = "Extra minutes on top of the 90-minute check-in cutoff for trip mode. Default 0."
    )]
    buffer_minutes: Option<u32>,
    #[schemars(description = "Skip live bus ETA lookups in airport-bus mode")]
    no_live: Option<bool>,
}

fn mcp_error(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

fn direction_arg(value: Option<&str>, default: Direction) -> Result<Direction, ErrorData> {
    match value {
        Some(value) => Direction::parse(value).ok_or_else(|| {
            mcp_error(format!(
                "unknown direction '{value}'; use departure or arrival"
            ))
        }),
        None => Ok(default),
    }
}

fn optional_direction(value: Option<&str>) -> Result<Option<Direction>, ErrorData> {
    match value {
        Some(value) => direction_arg(Some(value), Direction::Departure).map(Some),
        None => Ok(None),
    }
}

fn flights_payload(
    date: NaiveDate,
    direction: String,
    last_updated: Option<String>,
    flights: Vec<Flight>,
    extra: serde_json::Value,
) -> String {
    let mut payload = json!({
        "date": date.to_string(),
        "direction": direction,
        "lastUpdated": last_updated,
        "count": flights.len(),
        "flights": flights,
    });
    if let (Some(object), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

#[tool_router]
impl HkiaServer {
    pub fn new(
        client: Arc<Client>,
        db_path: Option<&Path>,
        config_path: Option<&Path>,
    ) -> Result<Self> {
        let store = Arc::new(match db_path {
            Some(path) => Store::open(path)?,
            None => Store::open_default()?,
        });
        let _ = store.prune(90);
        Ok(Self {
            client,
            fresh: Arc::new(Client::new(Duration::ZERO)?),
            store,
            weather: Arc::new(WeatherClient::new()?),
            access: Arc::new(access::AccessClient::new(Duration::from_secs(20))?),
            config_path: config_path.map(Path::to_path_buf),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn load_config(&self) -> Result<Config, ErrorData> {
        match &self.config_path {
            Some(path) => Config::load_from(path),
            None => Config::load(),
        }
        .map_err(mcp_error)
    }

    #[tool(
        description = "List live Hong Kong International Airport (HKG) departures or arrivals for a date, with scheduled time, estimated/actual time, delay in minutes, terminal, gate/stand, check-in aisle or baggage belt, and cancellation status."
    )]
    async fn list_flights(
        &self,
        Parameters(args): Parameters<ListFlightsArgs>,
    ) -> Result<String, ErrorData> {
        let direction = direction_arg(args.direction.as_deref(), Direction::Departure)?;
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let cargo = args.cargo.unwrap_or(false);
        let lang = args.lang.as_deref().unwrap_or("en");
        let board = self
            .client
            .board(date, direction, cargo, lang)
            .await
            .map_err(mcp_error)?;

        let sort = match args.sort.as_deref() {
            Some(value) => {
                SortKey::parse(value).ok_or_else(|| mcp_error(format!("unknown sort '{value}'")))?
            }
            None => SortKey::Scheduled,
        };
        let filter = FlightFilter {
            airport: args.airport,
            airline: args.airline,
            terminal: args.terminal,
            state: args.status,
            min_delay: args.delayed_over,
            sort,
            limit: args.limit.unwrap_or(50),
            ..FlightFilter::default()
        };
        let flights = filter.apply(&board.flights);
        let total = board.flights.len();
        Ok(flights_payload(
            date,
            direction.as_str().to_string(),
            board.last_updated.clone(),
            flights,
            json!({ "totalOnBoard": total }),
        ))
    }

    #[tool(
        description = "Look up a specific HKG flight by number (e.g. CX251) across departures and arrivals, including codeshare numbers, with live status, delay, gate and baggage information."
    )]
    async fn search_flight(
        &self,
        Parameters(args): Parameters<SearchFlightArgs>,
    ) -> Result<String, ErrorData> {
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let cargo = args.cargo.unwrap_or(false);
        let lang = args.lang.as_deref().unwrap_or("en");
        let (flights, last_updated) = fetch_flights(&self.client, date, direction, cargo, lang)
            .await
            .map_err(mcp_error)?;
        let filter = FlightFilter {
            flight: Some(args.flight_number.clone()),
            limit: 0,
            ..FlightFilter::default()
        };
        let matches = filter.apply(&flights);
        Ok(flights_payload(
            date,
            direction
                .map(|direction| direction.as_str().to_string())
                .unwrap_or_else(|| "both".to_string()),
            last_updated,
            matches,
            json!({ "flightNumber": args.flight_number }),
        ))
    }

    #[tool(
        description = "Get HKG flights that are currently delayed beyond a threshold, including computed delay minutes, scheduled and latest times, gate, terminal and baggage belt."
    )]
    async fn get_delays(
        &self,
        Parameters(args): Parameters<DelaysArgs>,
    ) -> Result<String, ErrorData> {
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let cargo = args.cargo.unwrap_or(false);
        let lang = args.lang.as_deref().unwrap_or("en");
        let minimum = args.min_delay_minutes.unwrap_or(15);
        let (flights, last_updated) = fetch_flights(&self.client, date, direction, cargo, lang)
            .await
            .map_err(mcp_error)?;
        let filter = FlightFilter {
            state: Some("delayed".to_string()),
            min_delay: Some(minimum),
            limit: args.limit.unwrap_or(100),
            ..FlightFilter::default()
        };
        let delayed = filter.apply(&flights);
        Ok(flights_payload(
            date,
            direction
                .map(|direction| direction.as_str().to_string())
                .unwrap_or_else(|| "both".to_string()),
            last_updated,
            delayed,
            json!({ "minDelayMinutes": minimum }),
        ))
    }

    #[tool(
        description = "Get real-time disruption alerts for HKG: cancellations and delays, with severity, route, latest times, gate and baggage belt. Use this for airport-wide disruption questions."
    )]
    async fn get_alerts(
        &self,
        Parameters(args): Parameters<AlertsArgs>,
    ) -> Result<String, ErrorData> {
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let cargo = args.cargo.unwrap_or(false);
        let lang = args.lang.as_deref().unwrap_or("en");
        let minimum = args.min_delay_minutes.unwrap_or(15);
        let include_cancelled = args.include_cancelled.unwrap_or(true);
        let (flights, last_updated) = fetch_flights(&self.client, date, direction, cargo, lang)
            .await
            .map_err(mcp_error)?;
        let alerts = build_alerts(&flights, minimum, include_cancelled);
        let limit = args.limit.unwrap_or(50);
        let total_alerts = alerts.len();
        let alerts = if limit == 0 {
            alerts
        } else {
            alerts.into_iter().take(limit).collect::<Vec<_>>()
        };
        let weather = if args.include_weather.unwrap_or(false) {
            self.weather.report(lang).await.ok()
        } else {
            None
        };
        let airport_express = if args.include_ael.unwrap_or(true) {
            self.access.ael_status().await.ok()
        } else {
            None
        };
        Ok(serde_json::to_string_pretty(&json!({
            "date": date.to_string(),
            "direction": direction.map(|direction| direction.as_str()).unwrap_or("both"),
            "lastUpdated": last_updated,
            "minDelayMinutes": minimum,
            "includeCancelled": include_cancelled,
            "includeWeather": args.include_weather.unwrap_or(false),
            "totalAlerts": total_alerts,
            "count": alerts.len(),
            "alerts": alerts,
            "weather": weather,
            "airportExpress": airport_express,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Compute on-time performance for HKG: totals, cancellations, delayed counts, average and maximum delay, on-time percentage, and per-hour breakdown."
    )]
    async fn get_stats(
        &self,
        Parameters(args): Parameters<StatsArgs>,
    ) -> Result<String, ErrorData> {
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let cargo = args.cargo.unwrap_or(false);
        let lang = args.lang.as_deref().unwrap_or("en");
        let (flights, last_updated) = fetch_flights(&self.client, date, direction, cargo, lang)
            .await
            .map_err(mcp_error)?;
        let stats = compute_stats(&flights);
        Ok(serde_json::to_string_pretty(&json!({
            "date": date.to_string(),
            "direction": direction.map(|direction| direction.as_str()).unwrap_or("both"),
            "lastUpdated": last_updated,
            "stats": stats,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Get disruptions filtered to the user's local watchlist (flights, airports or airlines), with optional Hong Kong Observatory weather context."
    )]
    async fn get_watchlist_alerts(
        &self,
        Parameters(args): Parameters<WatchlistAlertsArgs>,
    ) -> Result<String, ErrorData> {
        let config = self.load_config()?;
        if config.watchlist.is_empty() {
            return Err(mcp_error(
                "watchlist is empty; add flights, airports or airlines with `hkia watchlist add`",
            ));
        }
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let lang = "en";
        let (flights, last_updated) = fetch_flights(&self.client, date, direction, false, lang)
            .await
            .map_err(mcp_error)?;
        let filtered = config.watchlist.filter(&flights);
        let minimum = args
            .min_delay_minutes
            .unwrap_or_else(|| config.watchlist.min_delay());
        let include_cancelled = args
            .include_cancelled
            .unwrap_or_else(|| config.watchlist.include_cancelled());
        let mut alerts = build_alerts(&filtered, minimum, include_cancelled);
        let total = alerts.len();
        let limit = args.limit.unwrap_or(50);
        if limit > 0 {
            alerts.truncate(limit);
        }
        let weather = if args.include_weather.unwrap_or(false) {
            self.weather.report("en").await.ok()
        } else {
            None
        };
        let now = api::hk_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let _ = self.store.record(&now, &filtered, minimum);
        Ok(serde_json::to_string_pretty(&json!({
            "date": date.to_string(),
            "direction": direction.map(|direction| direction.as_str()).unwrap_or("both"),
            "lastUpdated": last_updated,
            "watchlist": config.watchlist,
            "minDelayMinutes": minimum,
            "includeCancelled": include_cancelled,
            "includeWeather": args.include_weather.unwrap_or(false),
            "totalAlerts": total,
            "count": alerts.len(),
            "alerts": alerts,
            "weather": weather,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Get the locally observed status/gate/baggage timeline for a flight. Call this after alerts, watch, or a previous history read has stored observations; the first call establishes a baseline."
    )]
    async fn get_flight_history(
        &self,
        Parameters(args): Parameters<FlightHistoryArgs>,
    ) -> Result<String, ErrorData> {
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let cargo = args.cargo.unwrap_or(false);
        let lang = args.lang.as_deref().unwrap_or("en");
        let (flights, last_updated) = fetch_flights(&self.client, date, None, cargo, lang)
            .await
            .map_err(mcp_error)?;
        let now = api::hk_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        self.store.record(&now, &flights, 15).map_err(mcp_error)?;

        let filter = FlightFilter {
            flight: Some(args.flight_number.clone()),
            limit: 0,
            ..FlightFilter::default()
        };
        let matches = filter.apply(&flights);
        let limit = args.limit.unwrap_or(100);
        let mut by_id: HashMap<i64, StoredEvent> = HashMap::new();
        for flight in &matches {
            for event in self
                .store
                .events_for_flight(&flight.flight_number, Some(&date.to_string()), limit)
                .map_err(mcp_error)?
            {
                by_id.insert(event.id, event);
            }
        }
        for event in self
            .store
            .events_for_flight(&args.flight_number, Some(&date.to_string()), limit)
            .map_err(mcp_error)?
        {
            by_id.insert(event.id, event);
        }
        let mut events: Vec<StoredEvent> = by_id.into_values().collect();
        events.sort_by(|left, right| {
            left.observed_at
                .cmp(&right.observed_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
        Ok(serde_json::to_string_pretty(&json!({
            "date": date.to_string(),
            "flightNumber": args.flight_number,
            "lastUpdated": last_updated,
            "matches": matches,
            "count": events.len(),
            "events": events,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Compute HKG on-time performance over a historical date range (up to 91 days back), with daily, airline, route, hour and weekday breakdowns."
    )]
    async fn get_historical_stats(
        &self,
        Parameters(args): Parameters<HistoricalStatsArgs>,
    ) -> Result<String, ErrorData> {
        let from = api::resolve_date(Some(args.from.as_str())).map_err(mcp_error)?;
        let to = api::resolve_date(args.to.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let report = crate::analytics::range_report(
            &self.client,
            from,
            to,
            direction,
            args.cargo.unwrap_or(false),
            args.lang.as_deref().unwrap_or("en"),
        )
        .await
        .map_err(mcp_error)?;
        let mut value = serde_json::to_value(&report).map_err(mcp_error)?;
        let limit = args.limit.unwrap_or(25).max(1);
        if let Some(object) = value.as_object_mut() {
            for key in ["by_day", "by_airline", "by_route", "by_weekday", "by_hour"] {
                if let Some(array) = object.get_mut(key).and_then(|value| value.as_array_mut()) {
                    array.truncate(limit);
                }
            }
            object.insert("group_by".to_string(), json!(args.group_by));
        }
        Ok(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Long-poll for new recorded HKG flight changes. Returns when an event newer than `after` appears or the timeout expires. Use the returned timestamp as the next `after` value."
    )]
    async fn wait_for_changes(
        &self,
        Parameters(args): Parameters<WaitForChangesArgs>,
    ) -> Result<String, ErrorData> {
        let after = match args.after.as_deref() {
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)
                .map(|value| {
                    value
                        .with_timezone(&api::hk_offset())
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
                })
                .map_err(|_| mcp_error("after must be an RFC 3339 timestamp"))?,
            None => (api::hk_now() - chrono::Duration::seconds(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        };
        let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
        let direction = optional_direction(args.direction.as_deref())?;
        let minimum = args.min_delay_minutes.unwrap_or(15);
        let timeout = args.timeout_seconds.unwrap_or(30).clamp(1, 120);
        let watchlist = if args.watchlist_only.unwrap_or(false) {
            let config = self.load_config()?;
            if config.watchlist.is_empty() {
                return Err(mcp_error("watchlist is empty"));
            }
            Some(config.watchlist)
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(timeout);

        loop {
            let (flights, last_updated) = fetch_flights(
                &self.fresh,
                date,
                direction,
                args.cargo.unwrap_or(false),
                "en",
            )
            .await
            .map_err(mcp_error)?;
            let current = match &watchlist {
                Some(watchlist) => watchlist.filter(&flights),
                None => flights.clone(),
            };
            let now = api::hk_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
            self.store
                .record(&now, &current, minimum)
                .map_err(mcp_error)?;
            let events = self
                .store
                .events_since(&after, false, 100)
                .map_err(mcp_error)?;
            if !events.is_empty() {
                return Ok(serde_json::to_string_pretty(&json!({
                    "after": after,
                    "timestamp": now,
                    "timeout": false,
                    "lastUpdated": last_updated,
                    "count": events.len(),
                    "events": events,
                }))
                .unwrap_or_else(|_| "{}".to_string()));
            }
            if Instant::now() >= deadline {
                return Ok(serde_json::to_string_pretty(&json!({
                    "after": after,
                    "timestamp": now,
                    "timeout": true,
                    "lastUpdated": last_updated,
                    "count": 0,
                    "events": [],
                }))
                .unwrap_or_else(|_| "{}".to_string()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(Duration::from_secs(remaining.as_secs().clamp(1, 5))).await;
        }
    }

    #[tool(
        description = "Get current Hong Kong Observatory weather warnings as operational context for airport disruption questions."
    )]
    async fn get_weather(
        &self,
        Parameters(args): Parameters<WeatherArgs>,
    ) -> Result<String, ErrorData> {
        let report = self
            .weather
            .report(args.lang.as_deref().unwrap_or("en"))
            .await
            .map_err(mcp_error)?;
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Get the runway in use at HKG from the official CAD ATIS, with the current airport wind and computed head/tailwind and crosswind for the arrival runway. Use for questions about which direction aircraft are landing or departing from, or aircraft noise near the airport."
    )]
    async fn get_runway_status(&self) -> Result<String, ErrorData> {
        let status = runway::fetch().await.map_err(mcp_error)?;
        Ok(serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        description = "Get airport access for HKG: live Airport Express trains and delay status, dated MTR in-town check-in hours for an airline, dated HKIA A-route bus windows and fares with live operator ETAs for a single route, or the latest Airport Express departure for a flight (mode=trip with time, station, date, airline, buffer_minutes). Check-in, fare and bus facts carry checked_on and source fields; they are reference data, not live availability."
    )]
    async fn get_airport_access(
        &self,
        Parameters(args): Parameters<AirportAccessArgs>,
    ) -> Result<String, ErrorData> {
        let mode = args.mode.trim().to_ascii_lowercase().replace('_', "-");
        match mode.as_str() {
            "airport-express" | "ael" | "express" => {
                let station = access::parse_station(args.station.as_deref().unwrap_or("HOK"))
                    .ok_or_else(|| {
                        mcp_error(format!(
                            "unknown AEL station '{}'; use HOK, KOW, TSY, AIR or AWE",
                            args.station.as_deref().unwrap_or("HOK")
                        ))
                    })?;
                let direction = match args.direction.as_deref() {
                    Some(value) => access::AelDirection::parse(value).ok_or_else(|| {
                        mcp_error(format!(
                            "unknown AEL direction '{value}'; use to-airport, from-airport or both"
                        ))
                    })?,
                    None => access::AelDirection::default_for(station),
                };
                let response = self
                    .access
                    .ael(station, direction)
                    .await
                    .map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string()))
            }
            "in-town-checkin" | "in-town-check-in" | "checkin" | "check-in" => {
                let airline = args
                    .airline
                    .as_deref()
                    .ok_or_else(|| mcp_error("airline is required for in-town check-in lookups"))?;
                let response = access::checkin(airline);
                Ok(serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string()))
            }
            "airport-express-status" | "ael-status" | "status" => {
                let status = self.access.ael_status().await.map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()))
            }
            "airport-bus" | "bus" | "buses" => {
                let direction = match args.direction.as_deref() {
                    Some(value) => access::BusDirection::parse(value).ok_or_else(|| {
                        mcp_error(format!(
                            "unknown bus direction '{value}'; use to-airport, from-airport or both"
                        ))
                    })?,
                    None => access::BusDirection::Both,
                };
                let mut response = access::bus_with_direction(args.route.as_deref(), direction)
                    .map_err(mcp_error)?;
                if args.route.is_some() && !args.no_live.unwrap_or(false) {
                    for route in response.routes.clone() {
                        response
                            .live
                            .extend(self.access.bus_live(&route, direction).await);
                    }
                }
                Ok(serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string()))
            }
            "trip" | "trip-planner" | "plan" => {
                let time = args
                    .time
                    .as_deref()
                    .ok_or_else(|| mcp_error("time (HH:MM) is required for trip mode"))?;
                let time = access::parse_time(time).map_err(mcp_error)?;
                let station = access::parse_station(args.station.as_deref().unwrap_or("HOK"))
                    .ok_or_else(|| {
                        mcp_error(format!(
                            "unknown AEL station '{}'; use HOK, KOW or TSY",
                            args.station.as_deref().unwrap_or("HOK")
                        ))
                    })?;
                let date = api::resolve_date(args.date.as_deref()).map_err(mcp_error)?;
                let trip = access::plan_ael_trip(
                    station,
                    args.airline.as_deref(),
                    date,
                    time,
                    args.terminal.clone(),
                    args.buffer_minutes.unwrap_or(0),
                    api::hk_now(),
                )
                .map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&trip).unwrap_or_else(|_| "{}".to_string()))
            }
            other => Err(mcp_error(format!(
                "unknown access mode '{other}'; use airport-express, airport-express-status, in-town-checkin, airport-bus or trip"
            ))),
        }
    }
}

impl HkiaServer {
    async fn resource_text(&self, uri: &str) -> Result<String, ErrorData> {
        let path = uri
            .strip_prefix("hkia://")
            .ok_or_else(|| mcp_error(format!("unsupported resource URI: {uri}")))?;
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        match parts.as_slice() {
            ["board", date, direction] => {
                let date = api::resolve_date(Some(date)).map_err(mcp_error)?;
                let direction = Direction::parse(direction)
                    .ok_or_else(|| mcp_error(format!("unknown board direction: {direction}")))?;
                let board = self
                    .client
                    .board(date, direction, false, "en")
                    .await
                    .map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&board).unwrap_or_else(|_| "{}".to_string()))
            }
            ["alerts", date] => {
                let date = api::resolve_date(Some(date)).map_err(mcp_error)?;
                let (flights, last_updated) = fetch_flights(&self.client, date, None, false, "en")
                    .await
                    .map_err(mcp_error)?;
                let alerts = build_alerts(&flights, 15, true);
                Ok(serde_json::to_string_pretty(&json!({
                    "date": date.to_string(),
                    "lastUpdated": last_updated,
                    "count": alerts.len(),
                    "alerts": alerts,
                }))
                .unwrap_or_else(|_| "{}".to_string()))
            }
            ["access", "ael", "status"] => {
                let status = self.access.ael_status().await.map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()))
            }
            ["access", "ael", station] => {
                let station = access::parse_station(station).ok_or_else(|| {
                    mcp_error(format!(
                        "unknown AEL station '{station}'; use HOK, KOW, TSY, AIR or AWE"
                    ))
                })?;
                let response = self
                    .access
                    .ael(station, access::AelDirection::default_for(station))
                    .await
                    .map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string()))
            }
            ["access", "checkin", airline] => {
                let response = access::checkin(airline);
                Ok(serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string()))
            }
            ["runway", "status"] => {
                let status = runway::fetch().await.map_err(mcp_error)?;
                Ok(serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()))
            }
            ["access", "bus", route] => {
                let direction = access::BusDirection::Both;
                let mut response =
                    access::bus_with_direction(Some(route), direction).map_err(mcp_error)?;
                for route in response.routes.clone() {
                    response
                        .live
                        .extend(self.access.bus_live(&route, direction).await);
                }
                Ok(serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string()))
            }
            _ => Err(mcp_error(format!("unknown resource URI: {uri}"))),
        }
    }
}

#[tool_handler(
    name = "hkia",
    version = "0.1.0",
    instructions = "Live Hong Kong International Airport (HKG) flight data. All times are Hong Kong time (UTC+8). Delay minutes are computed from the airport's scheduled time versus the published estimated or actual time; a flight is delayed at 15 minutes or more. The feed is public and rate-friendly, but responses are cached for a short period."
)]
impl ServerHandler for HkiaServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new("hkia", "0.1.0"))
        .with_instructions(
            "Live Hong Kong International Airport (HKG) flight data. All times are Hong Kong time (UTC+8). Resources: hkia://board/today/departures, hkia://board/today/arrivals, hkia://alerts/today, hkia://access/ael/status, hkia://access/ael/HOK, hkia://access/checkin/CX, hkia://access/bus/A11, hkia://runway/status.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = vec![
            Resource::new("hkia://board/today/departures", "HKG departures today")
                .with_description("Live departure board, refreshed when read")
                .with_mime_type("application/json"),
            Resource::new("hkia://board/today/arrivals", "HKG arrivals today")
                .with_description("Live arrival board, refreshed when read")
                .with_mime_type("application/json"),
            Resource::new("hkia://alerts/today", "HKG disruption alerts today")
                .with_description("Cancellations and 15+ minute delays across both boards")
                .with_mime_type("application/json"),
            Resource::new("hkia://access/ael/status", "Airport Express line status")
                .with_description("MTR Next Train delay flag for HOK and AIR")
                .with_mime_type("application/json"),
            Resource::new("hkia://access/ael/HOK", "Airport Express at HOK")
                .with_description(
                    "Replace HOK with KOW, TSY, AIR or AWE for live departures, fares and journey times",
                )
                .with_mime_type("application/json"),
            Resource::new("hkia://access/checkin/CX", "In-town check-in for CX")
                .with_description(
                    "Replace CX with an airline IATA/ICAO code for dated MTR check-in hours",
                )
                .with_mime_type("application/json"),
            Resource::new("hkia://runway/status", "Runway in use at HKG")
                .with_description(
                    "Official CAD ATIS arrival and departure runway with airport wind",
                )
                .with_mime_type("application/json"),
            Resource::new("hkia://access/bus/A11", "Airport bus A11")
                .with_description(
                    "Replace A11 with an A-route for dated windows, fares and live ETAs",
                )
                .with_mime_type("application/json"),
        ];
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let text = self.resource_text(&request.uri).await?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri).with_mime_type("application/json"),
        ])
        .into())
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        self.resource_text(&request.uri).await?;
        let uri = request.uri.clone();
        let peer = context.peer.clone();
        let server = self.clone();
        let handle = tokio::spawn(async move {
            let mut previous = String::new();
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Ok(text) = server.resource_text(&uri).await else {
                    continue;
                };
                if !previous.is_empty() && previous != text {
                    let notification = ServerNotification::ResourceUpdatedNotification(
                        ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(
                            uri.clone(),
                        )),
                    );
                    if peer.send_notification(notification).await.is_err() {
                        break;
                    }
                }
                previous = text;
            }
        });
        if let Ok(mut subscriptions) = self.subscriptions.lock()
            && let Some(old) = subscriptions.insert(request.uri, handle)
        {
            old.abort();
        }
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Ok(mut subscriptions) = self.subscriptions.lock()
            && let Some(handle) = subscriptions.remove(&request.uri)
        {
            handle.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn compares_api_keys_without_early_exit() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
        assert!(!constant_time_eq(b"", b"secret"));
    }
}
