---
name: hongkong-airport-skill
description: Live Hong Kong International Airport (HKG) flight information — departures, arrivals, delays, cancellations, terminal/gate/check-in/aisle/baggage guidance, disruption alerts, watchlists, historical on-time analytics, and HKO weather context. Use for any question about HKG flights, real-time status, airport delays, terminals, or on-time performance.
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
- `get_weather` — current Hong Kong Observatory warnings

Most tools take `date`, `direction`, `cargo`, and `lang`; check the tool schema
for exact parameter names.

MCP resources:

- `hkia://board/today/departures`
- `hkia://board/today/arrivals`
- `hkia://alerts/today`

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
- The live feed may include the previous day's tail; this tool selects the
  requested date. Keep request rates reasonable; responses cache for 20 seconds
  by default (`--cache-ttl 0` disables, `watch` is uncached).
- **ATC/tower text is not available.** HKIA tower communications are voice
  radio; there is no public per-flight ATC text or transcription endpoint. Do
  not claim to quote tower instructions. You can report published delays,
  cancellations, weather warnings, and gate changes, and clearly label any
  outside audio/transcript as unverified and unofficial.
- Reference data comes from OurAirports (public domain) and OpenFlights (ODbL);
  see `NOTICE`.
