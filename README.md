<div align="center">

# hkia

**Live Hong Kong International Airport flight data in one binary.**

[![CI](https://github.com/hongkong-ai-labs/hongkong-airport-skill/actions/workflows/ci.yml/badge.svg)](https://github.com/hongkong-ai-labs/hongkong-airport-skill/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hongkong-ai-labs/hongkong-airport-skill?style=flat-square&color=0e9488)](https://github.com/hongkong-ai-labs/hongkong-airport-skill/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-0e9488?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.89%2B-0e9488?style=flat-square&logo=rust&logoColor=white)](Cargo.toml)
[![MCP](https://img.shields.io/badge/MCP-stdio%20%7C%20HTTP%20%7C%20SSE-0e9488?style=flat-square)](https://modelcontextprotocol.io)

</div>

Live Hong Kong International Airport flight data from your terminal, your MCP
client, or your agent.

`hkia` reads the airport's public flight-information feed — the same feed behind
the HKIA app and website — and turns it into:

- a Rust CLI with tables, JSON, CSV and colour
- an MCP server over **stdio**, **streamable HTTP**, or **legacy SSE**
- a locally persisted timeline of what the airport actually published
- targeted watchlists with desktop and webhook notifications
- historical on-time analytics for the feed's D-91 to D+14 window
- an installable agent skill (`SKILL.md`)

No API key, no scraping, no browser automation.

This is an independent, unofficial project. It is not affiliated with Airport
Authority Hong Kong or HKIA, it sends no telemetry, and the underlying feed
remains subject to the airport's own terms.

## What you get

Schedules, estimated and actual times, computed delays, cancellations, terminal,
gate or stand, check-in aisle, baggage belt, arrival hall, codeshares, airline
and airport names, and ready-to-read terminal guidance. Plus disruption alerts,
hourly and multi-day statistics, a restart-safe watchlist, and Hong Kong
Observatory weather warnings as operational context. All times are normalised
to Hong Kong time (UTC+8).

### Terminal, gate and arrival guidance

Every flight carries the raw airport fields and a derived `guidance` string:

- departure: `Depart from Terminal 1, check-in aisle C, gate 30`
- arrival: `Arrive at Terminal 1, gate 30; baggage belt 12, arrival hall A`
- partial data produces partial guidance rather than an invented instruction
- cancelled flights say so instead of sending you to a gate

The airport feed does not publish airline check-in counter rows, security wait
times, or a per-flight air-traffic-control transcript. See
[Data limits and ATC](#data-limits-and-atc).

## Install

### Agent skill

The skill teaches an agent when to use the CLI or call the MCP tools. It needs
the `hkia` binary at runtime; `scripts/ensure-binary.sh` finds or installs it.

Official skills CLI (Node.js 22.20+):

```bash
npx skills add hongkong-ai-labs/hongkong-airport-skill
```

Rust-only alternative ([community `skills-cli`](https://crates.io/crates/skills-cli),
not maintained by this project):

```bash
cargo install skills-cli --locked
skills add hongkong-ai-labs/hongkong-airport-skill
```

Manual install without either CLI:

```bash
git clone --depth 1 https://github.com/hongkong-ai-labs/hongkong-airport-skill /tmp/hkia-skill
mkdir -p .agents/skills
cp -R /tmp/hkia-skill .agents/skills/hongkong-airport-skill
rm -rf .agents/skills/hongkong-airport-skill/.git

bash .agents/skills/hongkong-airport-skill/scripts/ensure-binary.sh
```

Use `~/.agents/skills` instead of `.agents/skills` for a user-wide install.
Claude Code also reads `~/.claude/skills`; other agents use their own skills
directory. `.agents/skills` is the portable layout used by both skills CLIs.

Before first use, the skill runs `scripts/ensure-binary.sh`. It checks
`$HKIA_BIN`, then `hkia` on `PATH`, then `scripts/install.sh`, then a local
`cargo build --release --locked`, and prints the absolute binary path to use.

### Prebuilt binary

macOS (Intel or Apple silicon) and Linux (x86_64 or aarch64, static musl),
checksum-verified into `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/hongkong-ai-labs/hongkong-airport-skill/main/scripts/install.sh | sh
```

Or from a checkout:

```bash
sh scripts/install.sh
# override with HKIA_BIN_DIR, HKIA_VERSION or HKIA_FORCE_INSTALL=1
```

Releases publish `hkia-vX.Y.Z-<target>.tar.gz` plus `SHA256SUMS`. The installer
verifies the checksum, replaces an older binary atomically, and falls back to a
source build if a release asset is unavailable.

### From source

```bash
git clone https://github.com/hongkong-ai-labs/hongkong-airport-skill
cd hongkong-airport-skill
cargo install --path . --locked
# or: cargo build --release --locked   # target/release/hkia
```

Requires Rust 1.89 or newer. SQLite is bundled, so there is no system
`libsqlite3` dependency.

### Docker

```bash
docker build -t hkia .
docker run --rm hkia departures --limit 10

# Streamable HTTP MCP, with persistent history and config
docker run --rm -p 8787:8787 \
  -v hkia-data:/data -v hkia-config:/config \
  -e HKIA_DATA_DIR=/data -e HKIA_CONFIG=/config/config.toml \
  hkia mcp --transport http --host 0.0.0.0 --port 8787
```

For a stdio MCP client, use the container as the server process:

```bash
docker run --rm -i -v hkia-data:/data hkia mcp
```

Or `docker compose up --build`. The compose file exposes port 8787 and mounts
named volumes for the SQLite store and watchlist. If you publish the image,
set `HKIA_AUTH_TOKEN` before exposing it beyond localhost.

## Run modes

One binary covers interactive use, scripts, live watching, MCP clients, and
containers. The skill is optional; it teaches an agent which mode to use.

| Mode | Start with | Best for |
| --- | --- | --- |
| CLI table | `hkia departures` | Reading a board in a terminal |
| JSON | `hkia departures --json` | `jq`, scripts, other programs |
| CSV | `hkia departures --csv` | Spreadsheets and data pipelines |
| Watch | `hkia watch --interval 60` | Change-only output and alerts |
| MCP stdio | `hkia mcp` | Desktop and CLI MCP clients that launch a process |
| MCP HTTP | `hkia mcp --transport http` | One long-lived server for several clients |
| MCP SSE | `hkia mcp --transport sse` | Clients that only support legacy SSE |
| Docker | `docker run --rm hkia departures` | No local Rust install |
| Agent skill | `SKILL.md` from the install section | Teaching an agent when to use the tool |

The same board in three styles:

```bash
hkia departures --limit 5
hkia departures --limit 5 --json | jq '.flights[] | {flight_number, status}'
hkia departures --limit 5 --csv
```

For MCP, the transport changes how the client connects, not the tools or data:

```bash
hkia mcp                                   # stdio
hkia mcp --transport http --port 8787      # http://127.0.0.1:8787/mcp
hkia mcp --transport sse --port 8787       # http://127.0.0.1:8787/sse
```

## CLI

```bash
hkia departures                       # today's departures, table
hkia arrivals --limit 20              # first 20 arrivals
hkia flights -D arrival               # explicit direction
hkia flight CX251                     # both boards and codeshares, plus guidance
hkia flight "CX 251" --date tomorrow

hkia delays                           # 15+ minute delays, both boards
hkia delays -D departure --min-delay 45

hkia alerts                           # severity-ranked cancellations and delays
hkia alerts --no-cancelled --min-delay 30
hkia alerts --watchlist --weather --notify

hkia stats                            # on-time performance and hourly counts
hkia stats -D arrival --date yesterday
hkia stats --from 2026-11-01 --to 2026-11-14 --group-by airline
hkia stats --from 2026-09-01 --group-by weekday --csv

hkia history CX251                    # observed status/gate timeline from SQLite
hkia watch --interval 60              # poll and print only changes
hkia watch --watchlist --notify --webhook https://example.test/hook
hkia departures --airport LHR --terminal T1 --airline CPA --status disrupted --json
hkia departures --lang zh_HK --limit 5
hkia departures --cargo --limit 10
```

Dates accept `YYYY-MM-DD`, `today`, `yesterday`, and `tomorrow`, resolved in
Hong Kong time. `--limit 0` returns every match. Run `hkia <command> --help` for
the complete flag set, including per-command filters such as `--flight`,
`--airport`, `--airline`, `--terminal`, `--status`, `--delayed-over` and
`--sort`.

### Commands

| Command | Purpose |
| --- | --- |
| `departures` / `arrivals` | One board, table/JSON/CSV |
| `flights` | Board with `-D/--direction` |
| `flight <number>` | One flight across both boards and codeshares, with guidance |
| `delays` | Flights delayed beyond a threshold |
| `alerts` | Cancellations and delays, optional watchlist and weather |
| `watch` | Poll and print status/gate/belt changes |
| `stats` | Single-day stats, or historical range analytics |
| `history <number>` | Locally observed status/gate timeline |
| `watchlist` | `show`, `add`, `remove`, `set`, `clear` |
| `mcp` | MCP server over stdio, HTTP, or SSE |

### Common flags

| Flag | Meaning |
| --- | --- |
| `--json` | Machine-readable JSON; wins over table output |
| `--csv` | CSV for board, flight, delays, alerts, stats, range, and history |
| `--no-header` | Omit the CSV header |
| `--color auto\|always\|never` | Table colour policy (default `auto`) |
| `--sort time\|delay\|flight\|state` | Board sort order (`departures`, `arrivals`, `flights`, `watch`) |
| `--cache-ttl 20` | Feed cache seconds; `0` disables |
| `--cargo` | Read the cargo board instead of passenger flights |
| `--lang en\|zh_HK\|zh_CN` | Raw feed language (default `en`); localized status text is parsed |
| `--db PATH` | Override the SQLite history database |
| `--config PATH` | Override the watchlist TOML path |

`--lang` changes the airport's raw status text, not the derived field names or
`guidance` text, which remain English. All three languages are parsed for status
type, delay basis and cancellations.

### JSON

Add `--json` to any command. A board looks like:

```json
{
  "date": "2026-11-21",
  "direction": "departure",
  "lastUpdated": "2026-11-21T20:07:01+08:00",
  "fetchedAt": "2026-11-21T20:07:03+08:00",
  "totalOnBoard": 431,
  "count": 1,
  "flights": [
    {
      "flight_number": "CX 251",
      "airline": "CPA",
      "airline_name": "Cathay Pacific",
      "codeshares": [],
      "direction": "departure",
      "date": "2026-11-21",
      "scheduled_time": "23:00",
      "scheduled_at": "2026-11-21T23:00:00+08:00",
      "status": "Dep 00:00 (22/11/2026)",
      "status_type": "departed",
      "status_published": true,
      "actual_at": "2026-11-22T00:00:00+08:00",
      "delay_minutes": 60,
      "delay_basis": "actual",
      "state": "delayed",
      "terminal": "T1",
      "gate": "30",
      "check_in_aisle": "C",
      "guidance": "Depart from Terminal 1, check-in aisle C, gate 30",
      "destination": ["LHR"],
      "destination_details": [{ "iata": "LHR", "name": "London Heathrow Airport", "city": "London", "country": "United Kingdom" }]
    }
  ]
}
```

Every flight includes `origin`, `destination`, `origin_details`,
`destination_details`, `terminal`, `gate`, `stand`, `check_in_aisle`,
`baggage_belt`, `arrival_hall`, `status_published`, `delay_basis`, `guidance`,
and `fetched_at`. Unknown operational fields are `null`, never guessed.

### Watchlist and notifications

The watchlist is TOML at `$HKIA_CONFIG`, `$XDG_CONFIG_HOME/hkia/config.toml`, or
`~/.config/hkia/config.toml`.

```bash
hkia watchlist add --flight CX251
hkia watchlist add --airport LHR
hkia watchlist add --airline CPA
hkia watchlist set --min-delay 10 --direction departure
hkia watchlist set --desktop true --webhook https://example.test/hook
hkia watchlist show
```

```toml
[watchlist]
flights = ["CX251"]
airports = ["LHR"]
airlines = ["CPA"]
direction = "departure"
min_delay = 10
include_cancelled = true

[notify]
desktop = false
webhook = "https://example.test/hook"
```

Empty selectors match every flight. Flights match operating numbers and
codeshares; airports match either endpoint of the route. Webhooks receive JSON
with `source`, `kind`, `timestamp`, `changes` or `alerts`, and the watchlist
that produced them. Notifications are deduplicated by alert fingerprint in
SQLite, so restarting `watch` does not resend the same alert; pass
`--force-notify` on `alerts` to bypass dedupe.

### History and persistence

SQLite lives at `$HKIA_DATA_DIR/hkia.db`, `$XDG_DATA_HOME/hkia/hkia.db`, or
`~/.local/share/hkia/hkia.db`. Watch ticks, `history`,
notification-enabled `alerts`, and MCP history/watch calls record observations;
a plain board read does not. Observations append an event only when status,
delay, gate, terminal, belt, or stand changes. Events older than 90 days are
pruned opportunistically. `hkia history CX251` is therefore limited to what this
machine has actually observed — it is not an airport-provided audit log.

## Historical analytics

`stats --from` uses the airport's historical endpoint, valid from D-91 through
D+14. Weekends and future days may be unavailable; failed days are reported in
`failures` instead of being silently dropped.

```bash
hkia stats --from 2026-11-01 --to 2026-11-14 --group-by route --json
```

The range report includes a flattened summary (`flights`, `cancelled`,
`delayed`, `on_time`, `early`, `scheduled`, `tracked`,
`average_delay_minutes`, `max_delay_minutes`, `on_time_percent`) plus `by_day`,
`by_airline`, `by_route`, `by_weekday`, and `by_hour`. `--group-by` is only
valid with `--from` and selects the table/CSV focus.

## MCP

### stdio

```json
{
  "mcpServers": {
    "hkia": {
      "command": "hkia",
      "args": ["mcp"]
    }
  }
}
```

If `hkia` is not on the MCP client's `PATH`, replace `"command": "hkia"` with
an absolute path. `scripts/ensure-binary.sh` prints the path to use. Restart the
client after changing its MCP configuration.

#### Client setup

Claude Code and Codex can write the stdio entry themselves:

```bash
claude mcp add hkia -- /absolute/path/to/hkia mcp
codex mcp add hkia -- /absolute/path/to/hkia mcp
```

Cursor reads `.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (user).
Claude Desktop uses `claude_desktop_config.json` in its application-support
directory. Paste the `mcpServers` block above into either file and restart the
client.

[`add-mcp`](https://add-mcp.com) is the closest equivalent to `npx skills add`
for MCP servers: it detects supported clients and writes the correct config for
each one.

```bash
npx add-mcp /absolute/path/to/hkia --args mcp --name hkia \
  -a cursor -a claude-code -y

# HTTP transport instead of stdio
npx add-mcp http://127.0.0.1:8787/mcp --name hkia \
  -a cursor -a claude-code -y
```

Use `$(command -v hkia)` in place of the path when `hkia` is already on
`PATH`. `add-mcp` is a third-party Node.js tool; the JSON above works without
it.

### Streamable HTTP

```bash
hkia mcp --transport http --port 8787
# endpoint: http://127.0.0.1:8787/mcp
# health:   http://127.0.0.1:8787/health
```

```json
{
  "mcpServers": {
    "hkia": {
      "type": "http",
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

### SSE

```bash
hkia mcp --transport sse --port 8787
# stream:   http://127.0.0.1:8787/sse
# messages: POST to the endpoint URL announced by the server
```

```json
{
  "mcpServers": {
    "hkia": {
      "type": "sse",
      "url": "http://127.0.0.1:8787/sse"
    }
  }
}
```

Use `--host 0.0.0.0` to listen beyond loopback. Both network transports expose
`GET /health` and permissive CORS for local tooling; `--path` changes the
streamable HTTP endpoint. `/health` stays public, while `/mcp`, `/sse`, and
`/messages` are protected when auth is enabled.

The server does not terminate TLS, and the bearer token is one shared static
secret. Keep the default loopback bind, or put it behind a TLS reverse proxy
before exposing it to another machine.

### Auth and rate limiting

```bash
hkia mcp --transport http --auth-token "$SECRET"
# or: HKIA_AUTH_TOKEN="$SECRET" hkia mcp --transport http
```

Protected routes require `Authorization: Bearer $SECRET`. Requests are limited
to 240 per minute by default across the process (`--rate-limit 0` disables,
`--rate-limit N` changes the cap); over-limit requests receive HTTP 429 with
`Retry-After: 60`. This is a single-process guard, not a multi-tenant quota.

```json
{
  "mcpServers": {
    "hkia": {
      "type": "http",
      "url": "http://127.0.0.1:8787/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

Keep the token in your client's secret store or an environment variable where
the client supports interpolation; do not commit it to a shared config file.

### Tools

| Tool | Purpose |
| --- | --- |
| `list_flights` | Board listing with airport, airline, terminal, state, delay, sort and limit filters |
| `search_flight` | One flight number across departures, arrivals, and codeshares |
| `get_delays` | Flights delayed beyond a threshold |
| `get_alerts` | Cancellations and delays as severity-ranked alerts, optional weather |
| `get_stats` | Single-day on-time performance and hourly breakdown |
| `get_watchlist_alerts` | Disruptions filtered to the local watchlist, optional weather |
| `get_flight_history` | Locally observed status/gate/belt timeline for a flight |
| `get_historical_stats` | Multi-day range analytics with airline/route/hour/weekday groups |
| `wait_for_changes` | Long-poll for new recorded changes; use the returned timestamp as the next `after` |
| `get_weather` | Current Hong Kong Observatory warnings |

Tools use the local watchlist and SQLite store, so the same `--config` and `--db`
global flags apply. Most tools accept `date`, `direction`, `cargo`, and `lang`;
see each tool's schema for exact names.

### Resources

| URI | Content |
| --- | --- |
| `hkia://board/today/departures` | Today's departure board JSON |
| `hkia://board/today/arrivals` | Today's arrival board JSON |
| `hkia://alerts/today` | Today's cancellations and 15+ minute delays |

Resources support read and the legacy subscribe/unsubscribe flow. Subscriptions
poll every 30 seconds and emit `notifications/resources/updated` when the
serialized resource changes. For reliable push-style integrations, prefer
`wait_for_changes`.

## Troubleshooting

- **Installer cannot find a release:** the script needs a `v*` release with a
  matching `<target>.tar.gz`. From a clone, build with
  `cargo build --release --locked`; setting `HKIA_VERSION` only selects an
  existing release tag.
- **Unsupported platform:** prebuilt binaries cover macOS x86_64/arm64 and Linux
  x86_64/aarch64. Use `cargo install --path . --locked` elsewhere.
- **MCP returns 401:** the client must send `Authorization: Bearer <token>` when
  `--auth-token` or `HKIA_AUTH_TOKEN` is set.
- **MCP returns 429:** the process-wide rate limit was reached; raise
  `--rate-limit` or wait for `Retry-After`.
- **Webhook delivery fails:** the request times out after 15 seconds, non-2xx
  responses are errors, and there is no retry queue. Delivery errors are printed
  as warnings; the watch loop continues.

## Configuration

| Variable | Meaning |
| --- | --- |
| `HKIA_API_BASE` | Override the feed base URL (default `https://www.hongkongairport.com/flightinfo-rest/rest`) |
| `HKIA_WEATHER_URL` | Override the HKO warning endpoint |
| `HKIA_AUTH_TOKEN` | Bearer token for HTTP/SSE MCP |
| `HKIA_DATA_DIR` | Directory containing `hkia.db` |
| `HKIA_CONFIG` | Watchlist TOML path |

## Data limits and ATC

- Source: the airport's public REST feed. The live board is
  `flights?date=YYYY-MM-DD&lang=en&cargo=false&arrival=false`; history uses
  `flights/past` for the same date window.
- The feed publishes one board per calendar day and may include the previous
  day's tail; only the requested date is used.
- `delay_minutes` is derived from free-text status (`Dep HH:MM`,
  `Est at HH:MM`, `At gate HH:MM`, `Cancelled`) against the scheduled time.
  A flight is `delayed` at 15 minutes or more and `early` at 15 minutes or less.
  This is not an airline cause code and does not distinguish weather, crew, or
  slot delays.
- Terminal and gate assignments can change or be blank until close to
  departure. `guidance` reflects the currently published fields only.
- Future dates may return no board until the airport publishes them. Very old
  dates outside D-91 and dates beyond D+14 return a range error.
- Responses are cached for 20 seconds (`--cache-ttl 0` disables it; `watch` is
  uncached). Delay/alert data is only as fresh as the airport feed itself; the
  project makes no uptime or freshness guarantee.
- Localized status parsing covers English, Traditional Chinese (`zh_HK`) and
  Simplified Chinese (`zh_CN`) terms such as `Dep`/`啟航`/`启航`,
  `At gate`/`到閘口`/`到闸口`, `Est`/`預計`/`预计` and
  `Cancelled`/`取消`. The raw status string still depends on the requested
  language.
- **ATC/tower text:** HKIA tower communications are voice radio, and there is
  no public per-flight ATC text or transcription feed. This tool does not
  scrape or transcribe tower audio. It can surface published delays, weather
  warnings, cancellations, and gate changes, but it cannot tell you what a
  controller said to a specific flight. Treat any such transcript as
  unverified unless it comes from an official source.

## Development

```bash
just check          # fmt-check, clippy, tests
just skill-check    # validate SKILL.md and shell scripts the way CI does
just build-release
```

Without `just`:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all --locked
```

Tests cover status parsing, midnight rollover, cancellations, codeshares,
filters, stats/alerts, the captured board fixture, watchlist matching and
dedupe, SQLite change detection and notification dedupe, weather parsing, and
TOML round-tripping. CI runs formatting, clippy, tests, a release build and a
SKILL.md/script validation job. Tagged releases verify the tag against
`Cargo.toml` and `CHANGELOG.md`, build macOS and static Linux binaries, smoke
test the native artifact, and publish `SHA256SUMS`.

Layout: `src/api.rs` (feed client, normalisation, delays, alerts, stats),
`src/analytics.rs` (historical ranges), `src/store.rs` (SQLite),
`src/watchlist.rs` (config and matching), `src/notify.rs` (desktop/webhook),
`src/weather.rs` (HKO), `src/render.rs` (tables/CSV), `src/mcp.rs` (transports,
tools, resources), `src/main.rs` (CLI), `scripts/install.sh` (release
installer), `scripts/ensure-binary.sh` (agent resolver), and `SKILL.md` (agent
skill).

## Data licences

Airport and airline reference data is embedded from OurAirports (public domain)
and OpenFlights (ODbL). See [NOTICE](NOTICE) for attribution and terms. Flight
data remains subject to the airport's own terms; this project only reads the
public endpoints.

## Licence

MIT. See [LICENSE](LICENSE).
