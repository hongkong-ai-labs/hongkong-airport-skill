---
name: hongkong-airport-skill
description: Live Hong Kong International Airport (HKG) flight information — departures, arrivals, delays, cancellations, terminal/gate/check-in/aisle/baggage guidance, disruption alerts, watchlists, historical on-time analytics, HKO weather context, and airport access (Airport Express, in-town check-in, A-route buses). Use for any question about HKG flights, real-time status, airport delays, terminals, on-time performance, or getting to and from the airport.
license: MIT
---

# HKIA flights

`hkia` is a Rust CLI and MCP server for the public Hong Kong International Airport
flight-information feed. It needs no API key and no account. It is an
independent, unofficial project and is not affiliated with the airport.

Use it when the user asks about:

- a flight arriving at or departing from Hong Kong (CX, UO, HX, NH, BA, QR, ...)
- delays, cancellations, estimated or actual times
- which terminal, check-in aisle, gate, stand, baggage belt, or arrival hall to use
- disruption alerts, a specific flight, an airport, or an airline
- on-time performance for a day or a historical date range
- watching a board for changes, or a persistent timeline of what was observed
- weather warnings affecting airport operations
- Airport Express trains, in-town check-in, or airport A-route buses

## Before first use

Run `scripts/ensure-binary.sh` from the skill directory:

```console
$ bash scripts/ensure-binary.sh
/Users/you/.local/bin/hkia
```

It prints the absolute path of a working `hkia` binary and exits non-zero only
if it could not download or build one. Use the printed path for every later
command, for example `BIN="$(bash scripts/ensure-binary.sh)"`, and do not assume
`hkia` is on `PATH`.

It checks, in order: `$HKIA_BIN`, `hkia` on `PATH`, `scripts/install.sh`, then
`cargo build --release --locked` in the repo root.

## Install

Skill only (teaches the agent to use the CLI):

```bash
npx skills add hongkong-ai-labs/hongkong-airport-skill
```

Rust-only alternative (community port):

```bash
cargo install skills-cli --locked
skills add hongkong-ai-labs/hongkong-airport-skill
```

Manual install: copy this directory, or just `SKILL.md` plus `scripts/`, into
the agent's skills directory (for example `.agents/skills/hongkong-airport-skill`
or `~/.claude/skills/hongkong-airport-skill`). Then run
`scripts/ensure-binary.sh` from that directory.

The binary (choose one):

```bash
sh scripts/install.sh            # from the skill directory; checksum-verified
cargo install --path .            # from a clone of this repo
cargo install --git https://github.com/hongkong-ai-labs/hongkong-airport-skill --bin hkia --locked
```

## Run modes

Use the CLI for one-shot queries and MCP when a client should call tools
directly. The transport changes how a client connects, not the data or tools.

| Mode | Start with | Use for |
| --- | --- | --- |
| CLI table | `hkia departures` | Human-readable board |
| JSON | `hkia departures --json` | Parsing and summarising |
| CSV | `hkia departures --csv` | Spreadsheet or pipeline output |
| Watch | `hkia watch --interval 60` | Change-only output |
| MCP stdio | `hkia mcp` | Client launches the process |
| MCP HTTP | `hkia mcp --transport http` | Shared long-lived server |
| MCP SSE | `hkia mcp --transport sse` | Legacy SSE clients |
| Docker | `docker run --rm hkia departures` | No local binary |

## CLI

Prefer `--json` when you need to parse or summarise output. Tables are for
humans. Times are Hong Kong time (UTC+8); JSON timestamps are RFC 3339 with
`+08:00`.

```bash
# Today's boards
hkia departures --json
hkia arrivals --json

# A date (YYYY-MM-DD, today, yesterday, tomorrow)
hkia flights --direction departure --date 2026-11-21 --json

# One flight, both boards and all codeshares
hkia flight CX251 --json
hkia flight "CX 251" --date tomorrow --json

# Delays and disruption alerts
hkia delays --min-delay 15 --json
hkia alerts --min-delay 30 --no-cancelled --json
hkia alerts --watchlist --weather --json

# On-time stats: single day, or a historical range
hkia stats --json
hkia stats --from 2026-11-01 --to 2026-11-14 --group-by airline --json
hkia stats --from 2026-09-01 --group-by weekday --json

# Locally observed timeline
hkia history CX251 --json

# Live changes: baseline, then only differences
hkia watch --interval 60
hkia watch --direction arrival --min-delay 20 --json   # NDJSON ticks

# Filters and formats
hkia departures --airport LHR --terminal T1 --airline CPA --limit 20 --json
hkia departures --status disrupted --csv
hkia departures --lang zh_HK --limit 10 --json
hkia departures --cargo --limit 10 --json

# Airport access
hkia access ael --station HOK --json
hkia access ael --station AIR --direction from-airport --json
hkia access status --json
hkia access checkin --airline CX --json
hkia access bus --route A11 --json
hkia access bus --route A35 --direction to-airport --json
hkia access trip --airline CX --time 23:59 --date tomorrow --json
```

Run `hkia <command> --help` for per-command filters such as `--flight`,
`--airport`, `--airline`, `--terminal`, `--status`, `--delayed-over`, and `--sort`.

The range endpoint covers D-91 through D+14. Historical range reports include
`by_day`, `by_airline`, `by_route`, `by_weekday`, and `by_hour`; failed days
appear in `failures`.

`--lang` accepts `en`, `zh_HK`, or `zh_CN` (default `en`). It changes the raw
status text, not JSON field names or `guidance`, which stay English. All three
languages are parsed for status type, delay basis, and cancellations.

## Terminal and gate guidance

Each flight JSON object has the airport's operational fields plus a derived
`guidance` string:

| Field | Meaning |
| --- | --- |
| `terminal` | Terminal, usually `T1` or `T2` |
| `gate` | Departure gate, or arrival gate when published |
| `stand` | Arrival stand when the airport publishes a remote stand |
| `check_in_aisle` | Departure check-in aisle letter |
| `baggage_belt` | Arrival reclaim belt |
| `arrival_hall` | Arrival hall |
| `guidance` | Human-readable instruction built only from the above |

Examples:

- `Depart from Terminal 1, check-in aisle C, gate 30`
- `Arrive at Terminal 1, gate 30; baggage belt 12, arrival hall A`
- `Depart from Terminal 1, check-in aisle C` when the gate is not yet assigned

When answering “which terminal?”, lead with `guidance` if present, then the raw
`terminal`, `gate`, `check_in_aisle`, `baggage_belt`, and `arrival_hall` fields.
If a field is `null`, say it has not been published yet; never fill it from a
different flight or a previous day. Gates and terminals can change, so include
`fetched_at` or `lastUpdated` when the user needs current information.

The feed does **not** provide airline counter rows, security queue times,
walking times between gates, or a per-flight ATC transcript.

## Other fields

| Field | Meaning |
| --- | --- |
| `flight_number`, `codeshares` | Operating number first; other marketing numbers after |
| `airline`, `airline_name` | ICAO code and resolved airline name |
| `direction`, `date` | `departure`/`arrival`, board date |
| `scheduled_time`, `scheduled_at` | Timetable time; RFC 3339 timestamp |
| `status`, `status_type` | Raw airport text; `departed`, `arrived`, `estimated`, `cancelled`, `scheduled`, `other` |
| `status_published` | Whether a non-scheduled status is present |
| `actual_at`, `estimated_at` | Parsed event time when published |
| `delay_minutes`, `delay_basis` | Event time minus schedule; `actual`, `estimated`, or null |
| `state` | `cancelled`, `delayed` (>=15 min), `on_time`, `early`, `scheduled` |
| `origin`, `destination` | IATA code lists; multi-leg flights list every stop |
| `origin_details`, `destination_details` | Resolved airport name, city, country |
| `fetched_at` | When this observation was fetched |

Delay minutes are computed from the airport's free-text status against the
scheduled time. English, Traditional Chinese, and Simplified Chinese status
terms are recognised. They are not airline cause codes and do not distinguish
weather, crew, slot, or maintenance reasons. `guidance` is always English.

## Airport access

Use `hkia access` for getting to or from HKG. It is separate from the flight
commands and keeps the same `--json` / `--csv` conventions.

```bash
hkia access ael --station HOK
hkia access ael --station KOW --direction both --json
hkia access status
hkia access checkin --airline CX
hkia access checkin --airline CPA --json
hkia access bus --route A11
hkia access bus --route A11 --direction from-airport --no-live
hkia access bus --csv
hkia access trip --station KOW --airline CX --time 07:40 --terminal T1 --buffer 15
```

- `ael` is **live** from the MTR Next Train API. Stations: `HOK`, `KOW`, `TSY`,
  `AIR`, `AWE`. Direction is `to-airport`, `from-airport`, or `both`; it
  defaults from the station. Each response has `trains`, plus `service` with
  journey times, first/last windows, official fares, `checked_on`, and sources.
- `status` is **live** from the same API's `isdelay` flag at HOK and AIR. Use it
  for "is the Airport Express disrupted" questions; it reports only whether MTR
  flagged a delay, not a cause or duration. `alerts` and `watch` include it by
  default (`--no-ael` disables), and watch notifies when the flag changes.
- `checkin` is a **dated snapshot** of MTR's published in-town check-in list.
  It matches IATA (`CX`), ICAO (`CPA`) or name. Returns station hours,
  eligibility, baggage limits, `source`, and `checked_on`. It is not live
  availability and does not confirm a specific flight is accepted.
- `bus` is a **dated snapshot** of the 30 HKIA A-routes in both directions,
  with destination, operator, adult fares and city-to-airport and from-airport
  windows. With `--route` it adds **live** Citybus/LWB/KMB/NLB ETAs for the
  published terminus stop; `--direction` selects the direction and `--no-live`
  keeps it static. ETAs are for the terminus, not the airport kerb.
- `trip` plans the latest AEL departure for a flight: 90-minute check-in cutoff,
  explicit `--buffer` (default 0), published journey time, rounded down to the
  10-minute headway, plus the airport deadline and in-town check-in hours. It
  never assumes security, immigration or walking time. The time must be in the
  future Hong Kong time; `--date tomorrow` handles late-night flights today.

The reference snapshot was checked on `2026-11-22`. Always pass through
`checked_on` and the source URL, and tell the user to confirm with MTR, the
airline, or the bus operator before travel. `scripts/refresh_access_data.py
--check` verifies the snapshots against the official sources.

## Watchlist and notifications

The watchlist is TOML at `$HKIA_CONFIG`, `$XDG_CONFIG_HOME/hkia/config.toml`, or
`~/.config/hkia/config.toml`:

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

Manage it with `hkia watchlist add --flight CX251`,
`hkia watchlist add --airport LHR`, `hkia watchlist set --min-delay 10`,
`hkia watchlist show`, and `hkia watchlist remove --flight CX251`.

Use `--watchlist` with `alerts` or `watch` to restrict output to the watchlist.
`--notify` sends desktop notifications; `--webhook URL` POSTs JSON. Alert
notifications are deduplicated by fingerprint in SQLite, so a restart does not
resend the same alert. Empty selectors match everything.

## Persistence and history

The local SQLite store is at `$HKIA_DATA_DIR/hkia.db`,
`$XDG_DATA_HOME/hkia/hkia.db`, or `~/.local/share/hkia/hkia.db`. Watch ticks,
`history`, notification-enabled alert checks, and MCP history/watch calls record
observations. Events are appended only when status, delay, gate, terminal, belt,
or stand changes. `history` shows what this machine observed, not an official
airport audit trail. Data older than 90 days is pruned opportunistically.

## MCP

```bash
hkia mcp                      # stdio (Claude Desktop, Cursor, Codex, ...)
hkia mcp --transport http     # streamable HTTP on http://127.0.0.1:8787/mcp
hkia mcp --transport sse      # legacy SSE on http://127.0.0.1:8787/sse
```

Client config for stdio:

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

If `hkia` is not on the MCP client's `PATH`, use the absolute path printed by
`scripts/ensure-binary.sh` as `command`. Restart the client after editing its
configuration.

Claude Code and Codex can register the stdio server themselves:

```bash
claude mcp add hkia -- /absolute/path/to/hkia mcp
codex mcp add hkia -- /absolute/path/to/hkia mcp
```

`npx add-mcp /absolute/path/to/hkia --args mcp --name hkia -a claude-code -y`
is a third-party helper that writes the same entry for many clients. For HTTP,
point the client at `http://127.0.0.1:8787/mcp`; for SSE, use
`http://127.0.0.1:8787/sse` (messages are POSTed to the endpoint URL the server
sends in the `endpoint` event). `GET /health` is available on both network
transports. `--path` changes the streamable HTTP endpoint.

For non-loopback use, set a bearer token with `--auth-token` or
`HKIA_AUTH_TOKEN`; requests then need `Authorization: Bearer <token>`.
`--rate-limit` caps HTTP/SSE requests per minute (default 240, `0` disables).
The server has no TLS of its own and uses one shared static token; bind loopback
or terminate TLS at a reverse proxy before exposing it.

MCP tools:

- `list_flights` — board listing with airport, airline, terminal, state, delay,
  sort, and limit filters
- `search_flight` — one flight number across both boards
- `get_delays` — flights delayed beyond a threshold
- `get_alerts` — cancellations and delays as severity-ranked alerts, optional weather
- `get_stats` — single-day on-time performance and hourly breakdown
- `get_watchlist_alerts` — disruptions filtered to the local watchlist
- `get_flight_history` — locally observed status/gate/belt timeline
- `get_historical_stats` — historical range analytics
- `wait_for_changes` — long-poll for new recorded changes; pass the returned timestamp as `after`
- `get_airport_access` — modes `airport-express`, `airport-express-status`,
  `in-town-checkin`, `airport-bus`, `trip` (`station`, `direction`, `airline`,
  `route`, `time`, `date`, `terminal`, `buffer_minutes`, `no_live`)
- `get_weather` — current Hong Kong Observatory warnings

Most flight tools take `date`, `direction`, `cargo`, and `lang`; check the tool
schema for exact parameter names.

MCP resources:

- `hkia://board/today/departures`
- `hkia://board/today/arrivals`
- `hkia://alerts/today`
- `hkia://access/ael/status`
- `hkia://access/ael/HOK` (any station code)
- `hkia://access/checkin/CX` (any airline code)
- `hkia://access/bus/A11` (any A-route)

Resources are JSON and support read plus legacy subscribe/unsubscribe; updates
are polled every 30 seconds. Prefer `wait_for_changes` for push-style clients.

## Troubleshooting

- `ensure-binary.sh` needs either a published release, a checkout with
  `Cargo.toml`, or `$HKIA_BIN`. If it fails, ask the user for an `hkia` path
  rather than inventing one.
- MCP `401` means the bearer token is missing or wrong; `429` means the
  process-wide rate limit was hit. Read `Retry-After` before retrying.
- Webhook delivery has a 15-second timeout and no retry queue. Errors are
  warnings; the watch loop continues.

## Rules and limits

- Never invent a flight, terminal, gate, aisle, belt, delay, or cause. If the
  tool returns no match, say so and offer the nearest board or date.
- Never present computed delay minutes as an official airline metric.
- The feed is published per calendar day. Future dates may be empty or missing;
  querying tomorrow early in the day can legitimately return no board.
- Cargo flights live on a separate board: pass `--cargo` / `"cargo": true`.
- Never invent an Airport Express time, check-in hour, bus route, fare, ETAs,
  or departure window. If a lookup fails, say so. In-town check-in, fare and
  bus facts are dated snapshots, not live availability; quote `checked_on`.
  AEL trains, the delay flag and bus ETAs are live and can be unavailable.
- The AEL delay flag is a coarse signal. Do not convert it into a delay length,
  cause, or recommendation to rebook. The trip planner's output is a plan based
  on published timetables and the stated buffer, not a guarantee of on-time
  arrival.
- The live feed may include the previous day's tail; this tool selects the
  requested date. Keep request rates reasonable; responses cache for 20 seconds
  by default (`--cache-ttl 0` disables, `watch` is uncached).
- **ATC/tower text is not available.** HKIA tower communications are voice
  radio; there is no public per-flight ATC text or transcription endpoint. Do
  not claim to quote tower instructions. You can report published delays,
  cancellations, weather warnings, and gate changes, and clearly label any
  outside audio/transcript as unverified and unofficial.
- Flight reference data comes from OurAirports (public domain) and OpenFlights
  (ODbL). Airport access facts come from MTR and HKIA public pages. See
  `NOTICE`.
