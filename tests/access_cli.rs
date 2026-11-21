//! End-to-end checks for the airport access commands and MCP routing.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn hkia() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hkia"))
}

fn temp_path(name: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("hkia-test-{}-{unique}-{name}", std::process::id()))
}

fn run_with(
    db: &std::path::Path,
    config: &std::path::Path,
    args: &[&str],
) -> (bool, String, String) {
    let output = hkia()
        .args(args)
        .arg("--db")
        .arg(db)
        .arg("--config")
        .arg(config)
        .output()
        .expect("run hkia");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn run(args: &[&str]) -> (bool, String, String) {
    run_with(&temp_path("cli.db"), &temp_path("cli.toml"), args)
}

/// Today and yesterday in Hong Kong time, so seeded rows stay inside the
/// store's 90-day retention window whenever the suite runs.
fn hk_today_and_yesterday() -> (chrono::NaiveDate, chrono::NaiveDate) {
    let offset = chrono::FixedOffset::east_opt(8 * 3600).expect("UTC+8 offset");
    let today = chrono::Utc::now().with_timezone(&offset).date_naive();
    let yesterday = today.pred_opt().expect("previous day");
    (today, yesterday)
}

#[test]
fn runway_history_daily_reports_seeded_days() {
    let db = temp_path("runway-daily.db");
    let config = temp_path("runway-daily.toml");
    let (ok, _, stderr) = run_with(&db, &config, &["runway", "--history"]);
    assert!(ok, "creating the store failed: {stderr}");

    // Seed through SQLite so the test stays offline and deterministic.
    let (today, yesterday) = hk_today_and_yesterday();
    let connection = rusqlite::Connection::open(&db).expect("open seeded store");
    connection
        .execute_batch(&format!(
            "INSERT INTO runway_changes (observed_at, arrival, departure) VALUES
             ('{yesterday}T09:15:00+08:00', '07C', '07R'),
             ('{yesterday}T11:40:00+08:00', '25R', '25C'),
             ('{today}T05:05:00+08:00', '07L', '07C');"
        ))
        .expect("seed runway changes");
    drop(connection);

    let (ok, stdout, stderr) = run_with(&db, &config, &["runway", "--history", "--daily"]);
    assert!(ok, "daily history failed: {stderr}");
    assert!(stdout.contains("2 day(s)"), "{stdout}");
    let older = stdout
        .find(&format!("{yesterday}  2"))
        .unwrap_or_else(|| panic!("missing yesterday row: {stdout}"));
    let newest = stdout
        .find(&format!("{today}  1"))
        .unwrap_or_else(|| panic!("missing today row: {stdout}"));
    assert!(older < newest, "expected oldest day first: {stdout}");
    assert!(stdout.contains("07C/07R"), "{stdout}");
    assert!(stdout.contains("25R/25C"), "{stdout}");

    let (ok, stdout, stderr) =
        run_with(&db, &config, &["--json", "runway", "--history", "--daily"]);
    assert!(ok, "daily JSON failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("daily JSON");
    assert_eq!(value["count"], 2);
    assert_eq!(value["days"][0]["date"], yesterday.to_string());
    assert_eq!(value["days"][0]["switches"], 2);
    assert_eq!(value["days"][0]["first_departure"], "07R");
    assert_eq!(value["days"][0]["last_arrival"], "25R");
    assert_eq!(value["days"][1]["date"], today.to_string());
    assert_eq!(value["days"][1]["switches"], 1);

    let (ok, stdout, stderr) = run_with(&db, &config, &["--csv", "runway", "--history", "--daily"]);
    assert!(ok, "daily CSV failed: {stderr}");
    assert!(
        stdout
            .starts_with("date,switches,first_arrival,first_departure,last_arrival,last_departure"),
        "{stdout}"
    );

    let (ok, stdout, stderr) = run_with(
        &db,
        &config,
        &["runway", "--history", "--daily", "--limit", "1"],
    );
    assert!(ok, "limited daily history failed: {stderr}");
    assert!(stdout.contains("1 day(s)"), "{stdout}");
    assert!(!stdout.contains(&yesterday.to_string()), "{stdout}");
}

#[test]
fn checkin_json_has_static_source_metadata() {
    let (ok, stdout, stderr) = run(&["--json", "access", "checkin", "--airline", "CX"]);
    assert!(ok, "checkin failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("checkin JSON");
    assert_eq!(value["mode"], "in_town_checkin");
    assert_eq!(value["query"], "CX");
    assert_eq!(value["airline"]["iata"], "CX");
    assert_eq!(value["participating"], true);
    assert!(
        value["checked_on"]
            .as_str()
            .is_some_and(|date| !date.is_empty())
    );
    assert!(
        value["source"]["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://"))
    );
    let stations = value["stations"].as_array().expect("stations array");
    assert_eq!(stations.len(), 2);
    assert!(stations.iter().all(|station| station["hours"].is_string()));
}

#[test]
fn bus_json_supports_direction_routing() {
    let (ok, stdout, stderr) = run(&[
        "--json",
        "access",
        "bus",
        "--route",
        "a11",
        "--direction",
        "from-airport",
        "--no-live",
    ]);
    assert!(ok, "bus lookup failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("bus JSON");
    assert_eq!(value["mode"], "airport_bus");
    assert_eq!(value["direction"], "from-airport");
    assert_eq!(value["route_count"], 1);
    let route = &value["routes"][0];
    assert_eq!(route["route"], "A11");
    assert_eq!(route["operator"], "Citybus");
    assert!(route["from_airport_hours"].is_string());
    assert!(route["fare_from_airport_hkd"].is_number());
    assert_eq!(value["live"].as_array().map(Vec::len), Some(0));
    assert!(value["checked_on"].is_string());
}

#[test]
fn bus_direction_only_route_is_reported() {
    let (ok, stdout, stderr) = run(&[
        "--json",
        "access",
        "bus",
        "--route",
        "A28X",
        "--direction",
        "from-airport",
        "--no-live",
    ]);
    assert!(ok, "bus lookup failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("bus JSON");
    let route = &value["routes"][0];
    assert_eq!(route["route"], "A28X");
    assert!(route["to_airport_hours"].is_string());
    assert!(route["from_airport_hours"].is_null());
    assert!(route["fare_from_airport_hkd"].is_null());
}

#[test]
fn unknown_ael_station_and_direction_fail_cleanly() {
    let (ok, _, stderr) = run(&["access", "ael", "--station", "ZZZ"]);
    assert!(!ok, "unknown station should fail");
    assert!(stderr.contains("unknown station"), "stderr: {stderr}");

    let (ok, _, stderr) = run(&["access", "ael", "--direction", "sideways"]);
    assert!(!ok, "unknown direction should fail");
    assert!(
        stderr.contains("possible values") || stderr.contains("invalid value"),
        "stderr: {stderr}"
    );

    let (ok, _, stderr) = run(&["access", "bus", "--route", "Z99", "--no-live"]);
    assert!(!ok, "unknown bus route should fail");
    assert!(
        stderr.contains("unknown airport bus route"),
        "stderr: {stderr}"
    );
}

#[test]
fn trip_planner_keeps_the_buffer_explicit() {
    let (ok, stdout, stderr) = run(&[
        "--json",
        "access",
        "trip",
        "--station",
        "KOW",
        "--airline",
        "CX",
        "--time",
        "23:59",
        "--date",
        "tomorrow",
        "--buffer",
        "30",
        "--terminal",
        "T1",
    ]);
    assert!(ok, "trip planning failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("trip JSON");
    assert_eq!(value["mode"], "airport_express_trip");
    assert_eq!(value["station"]["code"], "KOW");
    assert_eq!(value["assumptions"]["check_in_cutoff_minutes"], 90);
    assert_eq!(value["assumptions"]["buffer_minutes"], 30);
    assert_eq!(value["assumptions"]["headway_minutes"], 10);
    assert!(
        value["assumptions"]["journey_minutes"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert_eq!(value["flight"]["terminal"], "T1");
    assert_eq!(value["airline"]["iata"], "CX");
    assert!(value["in_town_checkin_hours"].is_string());
    assert_eq!(value["available"], true);
    let latest = value["latest_departure"]
        .as_str()
        .expect("latest departure");
    assert!(latest.ends_with("+08:00"), "latest departure: {latest}");
    let deadline = value["airport_arrival_deadline"]
        .as_str()
        .expect("airport deadline");
    assert!(deadline.ends_with("+08:00"), "deadline: {deadline}");
    assert!(latest < deadline);
    let notes = value["notes"].as_array().expect("notes");
    assert!(notes.iter().any(|note| {
        note.as_str()
            .is_some_and(|note| note.contains("No security"))
    }));
}

#[test]
fn trip_planner_outputs_csv() {
    let (ok, stdout, stderr) = run(&[
        "--csv",
        "access",
        "trip",
        "--station",
        "HOK",
        "--airline",
        "CX",
        "--time",
        "23:59",
        "--date",
        "tomorrow",
    ]);
    assert!(ok, "trip CSV failed: {stderr}");
    let header = stdout.lines().next().unwrap_or_default();
    assert!(
        header.starts_with("station,airline,flight_date"),
        "header: {header}"
    );
    let row = stdout.lines().nth(1).unwrap_or_default();
    assert!(row.starts_with("HOK,CX,"), "row: {row}");
}

#[test]
fn trip_planner_rejects_airport_side_stations() {
    let (ok, _, stderr) = run(&[
        "access",
        "trip",
        "--station",
        "AIR",
        "--time",
        "23:59",
        "--date",
        "tomorrow",
    ]);
    assert!(!ok, "airport-side station should fail");
    assert!(stderr.contains("HOK, KOW or TSY"), "stderr: {stderr}");
}

#[test]
fn mcp_runway_history_tool_groups_seeded_switches() {
    let db = temp_path("mcp-runway.db");
    let config = temp_path("mcp-runway.toml");
    let (ok, _, stderr) = run_with(&db, &config, &["runway", "--history"]);
    assert!(ok, "creating the store failed: {stderr}");

    let (today, yesterday) = hk_today_and_yesterday();
    let connection = rusqlite::Connection::open(&db).expect("open seeded store");
    connection
        .execute_batch(&format!(
            "INSERT INTO runway_changes (observed_at, arrival, departure) VALUES
             ('{yesterday}T09:15:00+08:00', '07C', '07R'),
             ('{yesterday}T11:40:00+08:00', '25R', '25C'),
             ('{today}T05:05:00+08:00', '07L', '07C');"
        ))
        .expect("seed runway changes");
    drop(connection);

    let mut mcp = Mcp::start_with(&db, &config);
    mcp.initialize();

    let recent = tool_payload(&mcp.call(
        2,
        "tools/call",
        serde_json::json!({ "name": "get_runway_history", "arguments": { "limit": 2 } }),
    ));
    assert_eq!(recent["count"], 2);
    assert_eq!(recent["changes"][0]["arrival"], "25R", "{recent}");
    assert_eq!(recent["changes"][1]["arrival"], "07L", "{recent}");

    let all = tool_payload(&mcp.call(
        3,
        "tools/call",
        serde_json::json!({ "name": "get_runway_history" }),
    ));
    assert_eq!(all["count"], 3);

    let daily = tool_payload(&mcp.call(
        4,
        "tools/call",
        serde_json::json!({ "name": "get_runway_history", "arguments": { "daily": true } }),
    ));
    assert_eq!(daily["count"], 2);
    assert_eq!(daily["days"][0]["date"], yesterday.to_string());
    assert_eq!(daily["days"][0]["switches"], 2);
    assert_eq!(daily["days"][1]["switches"], 1);
    assert!(
        daily.get("changes").is_none(),
        "daily payload should not list switches: {daily}"
    );

    let daily_one = tool_payload(&mcp.call(
        5,
        "tools/call",
        serde_json::json!({ "name": "get_runway_history", "arguments": { "daily": true, "limit": 1 } }),
    ));
    assert_eq!(daily_one["count"], 1);
    assert_eq!(daily_one["days"][0]["date"], today.to_string());
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<String>,
}

impl Mcp {
    fn start() -> Self {
        Self::start_with(&temp_path("mcp.db"), &temp_path("mcp.toml"))
    }

    fn start_with(db: &std::path::Path, config: &std::path::Path) -> Self {
        let mut child = hkia()
            .arg("mcp")
            .arg("--db")
            .arg(db)
            .arg("--config")
            .arg(config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start hkia mcp");
        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = child.stdout.take().expect("mcp stdout");
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            child,
            stdin,
            responses,
        }
    }

    fn initialize(&mut self) {
        let id = self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "hkia-tests", "version": "0" }
            }
        }));
        let init = self.response(id);
        assert!(init["result"]["serverInfo"]["name"].is_string());
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }));
    }

    fn send(&mut self, message: serde_json::Value) -> u64 {
        writeln!(self.stdin, "{message}").expect("write mcp request");
        self.stdin.flush().expect("flush mcp request");
        message["id"].as_u64().unwrap_or_default()
    }

    fn response(&self, id: u64) -> serde_json::Value {
        loop {
            let line = self
                .responses
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("timed out waiting for MCP response {id}"));
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value["id"].as_u64() == Some(id) {
                return value;
            }
        }
    }

    fn call(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.response(id)
    }

    fn tool(&mut self, id: u64, arguments: serde_json::Value) -> serde_json::Value {
        self.call(
            id,
            "tools/call",
            serde_json::json!({ "name": "get_airport_access", "arguments": arguments }),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool_payload(request: &serde_json::Value) -> serde_json::Value {
    if let Some(error) = request.get("error") {
        return serde_json::json!({ "error": error["message"] });
    }
    let text = request["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("unexpected tool result: {request}"));
    serde_json::from_str(text).expect("tool payload JSON")
}

#[test]
fn mcp_routes_access_modes_and_resources() {
    let mut mcp = Mcp::start();
    mcp.initialize();

    let tools = mcp.call(2, "tools/list", serde_json::json!({}));
    let names = tools["result"]["tools"]
        .as_array()
        .expect("tools list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"get_airport_access"), "tools: {names:?}");
    assert!(names.contains(&"get_alerts"), "tools: {names:?}");
    assert!(names.contains(&"get_runway_status"), "tools: {names:?}");
    assert!(names.contains(&"get_runway_history"), "tools: {names:?}");
    let history_tool = tools["result"]["tools"]
        .as_array()
        .expect("tools list")
        .iter()
        .find(|tool| tool["name"] == "get_runway_history")
        .expect("get_runway_history tool");
    assert!(
        history_tool["inputSchema"]["properties"]["limit"].is_object(),
        "get_runway_history schema: {history_tool}"
    );
    let alerts_tool = tools["result"]["tools"]
        .as_array()
        .expect("tools list")
        .iter()
        .find(|tool| tool["name"] == "get_alerts")
        .expect("get_alerts tool");
    assert!(
        alerts_tool["inputSchema"]["properties"]["include_runway"].is_object(),
        "get_alerts schema: {alerts_tool}"
    );

    let resources = mcp.call(11, "resources/list", serde_json::json!({}));
    let uris = resources["result"]["resources"]
        .as_array()
        .expect("resources list")
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect::<Vec<_>>();
    assert!(
        uris.contains(&"hkia://runway/status"),
        "resources: {uris:?}"
    );
    assert!(
        uris.contains(&"hkia://runway/history"),
        "resources: {uris:?}"
    );
    assert!(
        uris.contains(&"hkia://access/ael/status"),
        "resources: {uris:?}"
    );

    let checkin =
        tool_payload(&mcp.tool(3, serde_json::json!({ "mode": "checkin", "airline": "CX" })));
    assert_eq!(checkin["mode"], "in_town_checkin");
    assert_eq!(checkin["airline"]["iata"], "CX");

    let bus = tool_payload(&mcp.tool(
        4,
        serde_json::json!({
            "mode": "bus",
            "route": "A11",
            "direction": "to-airport",
            "no_live": true
        }),
    ));
    assert_eq!(bus["direction"], "to-airport");
    assert_eq!(bus["routes"][0]["route"], "A11");
    assert!(bus["routes"][0]["to_airport_hours"].is_string());

    let trip_response = mcp.tool(
        5,
        serde_json::json!({
            "mode": "trip",
            "station": "HOK",
            "airline": "CX",
            "time": "23:59",
            "date": "tomorrow"
        }),
    );
    let trip = tool_payload(&trip_response);
    assert_eq!(trip["mode"], "airport_express_trip");
    assert_eq!(trip["station"]["code"], "HOK");
    assert_eq!(trip["assumptions"]["buffer_minutes"], 0);
    assert!(trip["latest_departure"].is_string());

    let missing_time = mcp.tool(6, serde_json::json!({ "mode": "trip", "station": "HOK" }));
    let message = missing_time["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("time"), "error: {missing_time}");

    let unknown = mcp.tool(7, serde_json::json!({ "mode": "hovercraft" }));
    let message = tool_payload(&unknown);
    assert!(
        message["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown access mode")
    );

    let status = tool_payload(&mcp.tool(8, serde_json::json!({ "mode": "status" })));
    if let Some(error) = status["error"].as_str() {
        assert!(
            error.contains("MTR line status unavailable"),
            "unexpected status error: {error}"
        );
    } else {
        assert_eq!(status["mode"], "airport_express_status");
        assert!(status["stations"].is_array());
    }

    let resource = mcp.call(
        9,
        "resources/read",
        serde_json::json!({ "uri": "hkia://access/checkin/CX" }),
    );
    let text = resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    let resource_value: serde_json::Value = serde_json::from_str(text).expect("resource JSON");
    assert_eq!(resource_value["airline"]["iata"], "CX");

    let history = mcp.call(
        12,
        "resources/read",
        serde_json::json!({ "uri": "hkia://runway/history" }),
    );
    let history_text = history["result"]["contents"][0]["text"]
        .as_str()
        .expect("runway history text");
    let history_value: serde_json::Value =
        serde_json::from_str(history_text).expect("runway history JSON");
    assert!(history_value["changes"].is_array());
    assert_eq!(history_value["count"], 0);

    let missing = mcp.call(
        10,
        "resources/read",
        serde_json::json!({ "uri": "hkia://access/checkin/ZZ" }),
    );
    let missing_value: serde_json::Value = serde_json::from_str(
        missing["result"]["contents"][0]["text"]
            .as_str()
            .unwrap_or("{}"),
    )
    .expect("resource JSON");
    assert_eq!(missing_value["participating"], false);
}
