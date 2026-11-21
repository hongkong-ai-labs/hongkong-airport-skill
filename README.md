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
- live Airport Express trains and delay status, a trip planner, in-town check-in
  reference, and A-route bus data with live ETAs
- the runway in use with the airport wind and computed wind components
- an installable agent skill (`SKILL.md`)

No data API key, no scraping, no browser automation. The HTTP MCP transport can
optionally require a static API key.

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
npx skills update        # refresh the installed skill later
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
# or directly from GitHub:
cargo install --git https://github.com/hongkong-ai-labs/hongkong-airport-skill --locked
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

Apple's `container` runtime uses the same OCI image:

```bash
container build -t hkia .
container run --rm hkia departures --limit 10
container run --rm -p 8787:8787 -e HKIA_MCP_API_KEY="$SECRET" \
  hkia mcp --transport http --host 0.0.0.0 --port 8787
```

Or `docker compose up --build`. The compose file exposes port 8787 and mounts
named volumes for the SQLite store and watchlist. If you expose the HTTP MCP
transport beyond localhost, set `HKIA_MCP_API_KEY` (or use `--api-key`).

### Cloudflare Worker + Container

`wrangler.jsonc` and `cloudflare/` deploy the HTTP MCP server as a Worker front
door plus a Cloudflare Container. Wrangler builds the `linux/amd64` image from
the root `Dockerfile`; a Docker-compatible engine is required. Set the optional
key and deploy from the repository root:

```bash
npx wrangler secret put HKIA_MCP_API_KEY
npx wrangler deploy
```

Cloudflare's remote MCP transport is Streamable HTTP at `/mcp`; `/health` stays
public. A pure WASM Worker is deliberately not included: the server needs
TCP/TLS, a Tokio runtime and bundled SQLite. See
[`cloudflare/README.md`](cloudflare/README.md).

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

`hkia update` detects whether the binary came from cargo, the release
installer, a source checkout or Homebrew, and updates with that same
method. `hkia update --check` only reports.

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

hkia access ael --station HOK           # live Airport Express next trains
hkia access ael --station AIR --direction from-airport --json
hkia access status                      # live AEL delay status
hkia access checkin --airline CX        # dated in-town check-in reference
hkia access bus --route A11             # dated windows plus live ETAs
hkia access trip --airline CX --time 23:59 --date tomorrow --terminal T1
hkia access bus --route A35 --direction to-airport --json
hkia update --check
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
| `alerts` | Cancellations and delays, with optional watchlist, weather, AEL status and runway |
| `watch` | Poll and print status/gate/belt changes, plus AEL status and runway changes |
| `stats` | Single-day stats, or historical range analytics |
| `history <number>` | Locally observed status/gate timeline |
| `access` | `ael`, `status`, `checkin`, `bus`, `trip` airport-access queries |
| `runway` | Runway in use for arrivals and departures, with the airport wind |
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
| `--cache-ttl 20` | API cache seconds (HKIA and MTR); `0` disables |
| `--keep-days 90` | Days of flight and runway history kept when the store is pruned |
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

## Airport access

`hkia access` covers getting to and from the airport without changing the
flight commands. It is a separate surface with explicit data provenance.

```bash
hkia access ael --station HOK
hkia access ael --station AIR --direction from-airport --json
hkia access ael --station KOW --direction both --csv
hkia access status --json

hkia access checkin --airline CX
hkia access checkin --airline CPA --json

hkia access bus --route A11
hkia access bus --route A35 --json
hkia access bus --route A31 --direction to-airport --csv
hkia access bus --route A11 --no-live
hkia access bus --csv

hkia access trip --airline CX --time 23:59 --date tomorrow
hkia access trip --station KOW --airline CX --time 07:40 --terminal T1 --buffer 15 --json

hkia runway                            # arrival/departure runway in use
hkia runway --json
hkia runway --csv
hkia runway --history                  # locally observed switches (from watch)
```

`ael` is live: it reads the MTR Next Train API through data.gov.hk and returns
upcoming trains, destination, platform and minutes to departure. Stations are
`HOK`, `KOW`, `TSY`, `AIR` and `AWE`. The direction defaults to `to-airport`
from Hong Kong, Kowloon and Tsing Yi, and `from-airport` at Airport and
AsiaWorld-Expo. Each response includes the AEL journey times, first/last train
windows and the official MTR fares (Octopus, single and 30-day round trip,
adult and child), with `checked_on` and source URLs.

`status` checks the same live API for the `isdelay` flag at Hong Kong and
Airport and reports whether MTR has flagged an Airport Express delay. It is a
coarse line-status signal, not a cause or an estimate of how long a delay will
last. `alerts` and `watch` include this status by default (`--no-ael` skips it),
and watch mode prints and notifies when the flag changes.

`checkin` is a dated snapshot of MTR's published in-town check-in list, not live
availability. It matches IATA codes, ICAO codes or airline names and returns
Hong Kong and Kowloon station hours, the check-in window, eligibility and
baggage limits.

`bus` is a dated snapshot of the 30 HKIA A-routes with both directions:
destination, operator, adult fares (weekday plus published holiday notes) and
the city-to-airport and from-airport departure windows. With `--route`, it also
queries the Citybus, Long Win/KMB or New Lantao live ETA API for the published
terminus stop; `--direction` picks the direction (default `both`) and
`--no-live` keeps the output static.

`trip` plans the latest Airport Express departure for a flight. It applies MTR's
published 90-minute check-in cutoff, adds an explicit `--buffer` (default 0),
uses the station's published journey time, rounds down to the 10-minute
headway, and reports the airport arrival deadline and latest train with the
published first/last train window. It does **not** invent security, immigration
or walking times, and it shows in-town check-in hours for the station without
shortening the airport buffer. The flight time must be in the future Hong Kong
time; use `--date tomorrow` for late-night flights already passed today.

The reference data was checked on **2026-11-22**. Hours, fares and participating
airlines can change; confirm with MTR, the airline or the bus operator before
travel. Sources: the MTR Next Train API, MTR Airport Express timetable, fares
CSV and frequency pages, MTR's in-town check-in page, HKIA's public-bus page,
and the Citybus/LWB/KMB/NLB live ETA APIs; see [NOTICE](NOTICE) for the exact
URLs. Use `--json` or `--csv` for scripts; `--no-header` omits the CSV header.

`scripts/refresh_access_data.py --write` regenerates the embedded files and
`--check` compares them with the official sources without updating anything. A
scheduled GitHub Actions workflow runs the check weekly and opens an issue when
the data drifts.

## Runway in use

`hkia runway` answers which direction aircraft are landing and departing. It
reads the Civil Aviation Department's public ATIS page on `hkatc.gov.hk`, which
is the official runway-in-use broadcast, and reports the arrival and departure
runway separately along with the ATIS letter and issue time.

```bash
hkia runway
hkia runway --json
hkia runway --csv
hkia runway --history
```

The output also gives the airport wind from the ATIS and the computed
head/tailwind and crosswind components for the arrival runway, plus the approach
side implied by the runway designator: runway 07 means aircraft land heading
eastbound, approaching from the west, while runway 25 means the opposite. This
is what residents around the flight paths usually want when asking whether
aircraft are coming in over their side of the airport.

Runway 07 is preferred at HKG whenever the tailwind component is 5 knots or less
(AIP VHHH AD 2.22), but ATC can use runway 25 when required, and arrivals and
departures can differ. The tool therefore reports the ATIS statement rather than
predicting from wind. Wind components are derived from the reported wind, not
threshold measurements, and the ATIS page can be briefly unavailable while a
bulletin updates.

When the ATIS page cannot be read, `hkia runway` falls back to the Hong Kong
Observatory's 10-minute wind at Chek Lap Kok and says plainly that the runway is
not confirmed. In that mode the JSON has `atis_available: false`, a `wind`
object with the compass direction and knots, and `wind_favours`, which applies
the AIP rule (runway 07 when the tailwind is 5 knots or less). The wind favour
is an indication, not the runway in use, and the command never invents one. If
both the ATIS and the HKO feed fail, it returns an error instead.

In table mode, `hkia runway` also prints the most recent switch this machine
recorded while `watch` was running, so the current runway can be read together
with when it last changed. JSON carries the same as `last_observed`, which is
`null` until something has been observed; that line is local observation, not
part of the official ATIS.

`hkia watch` polls the runway with the rest of the board and prints, notifies
and webhooks when the arrival or departure runway changes (`--no-runway` skips
those checks, `--no-ael` skips the AEL status). `hkia alerts` includes the
current runway and AEL status too, with the same `--no-runway` / `--no-ael`
opt-outs. `watch` opens with a one-line summary of the recorded runway history,
and `alerts` prints the last observed switch beneath the live runway (JSON
`last_observed`).

Every observed switch is also written to the local SQLite store, so
`hkia runway --history` shows what this machine actually saw, oldest first:

```bash
hkia runway --history
hkia runway --history --limit 50 --json
hkia runway --history --csv
hkia runway --history --daily
```

`--daily` groups the rows by local date and shows how many switches were seen
that day with the first and last arrival/departure runway (`--limit` then caps
the number of days). History is pruned to the last `--keep-days` days (default
90) whenever a command opens the store; raise it to keep more, or set `0` to
keep none.

```
Observed runway switches by day  |  1 day(s)  |  local store
DATE        SWITCHES  FIRST        LAST
2026-11-22  1         07C/07R      07C/07R
```

There is no public historical runway feed; the history only covers periods when
`hkia watch` was running, and a switch is recorded once per change, not per
poll. The same rows are available to agents as the `hkia://runway/history`
resource (last 20 switches).

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

Register it with any MCP client:

```bash
npx add-mcp "$(command -v hkia) mcp" --name hkia -a claude-code -a codex -a cursor -y
claude mcp add hkia -- hkia mcp
codex mcp add hkia -- hkia mcp
```

`add-mcp` detects the client configuration files in the current project; add
`-g` for a global install. Substitute an absolute path if your client does not
inherit the shell `PATH`.


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
hkia mcp --transport http --api-key "$SECRET"
# --auth-token is an alias; either env var works:
# HKIA_MCP_API_KEY="$SECRET" hkia mcp --transport http
# HKIA_AUTH_TOKEN="$SECRET" hkia mcp --transport http
```

Protected routes require `Authorization: Bearer $SECRET` or
`X-API-Key: $SECRET`. Requests are limited
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
`/health` remains public so container and Worker readiness probes do not need
the key.

### Tools

| Tool | Purpose |
| --- | --- |
| `list_flights` | Board listing with airport, airline, terminal, state, delay, sort and limit filters |
| `search_flight` | One flight number across departures, arrivals, and codeshares |
| `get_delays` | Flights delayed beyond a threshold |
| `get_alerts` | Cancellations and delays as severity-ranked alerts, with weather, AEL status and runway in use |
| `get_stats` | Single-day on-time performance and hourly breakdown |
| `get_watchlist_alerts` | Disruptions filtered to the local watchlist, optional weather |
| `get_flight_history` | Locally observed status/gate/belt timeline for a flight |
| `get_historical_stats` | Multi-day range analytics with airline/route/hour/weekday groups |
| `wait_for_changes` | Long-poll for new recorded changes; use the returned timestamp as the next `after` |
| `get_weather` | Current Hong Kong Observatory warnings |
| `get_runway_status` | Runway in use for arrivals and departures, ATIS wind, wind components and last observed switch |
| `get_runway_history` | Locally observed runway switches, oldest first, with an optional `limit` (default 20, 0 for all) and `daily` grouping |
| `get_airport_access` | Live AEL trains and status, trip planning, dated in-town check-in hours, or A-route bus data with live ETAs |

Watchlist and history tools use the local SQLite store and config, so the same
`--db`, `--config` and `--keep-days` global flags apply. Most flight tools accept `date`,
`direction`, `cargo`, and `lang`; `get_airport_access` takes `mode`, `station`,
`direction`, `airline`, `route`, `time`, `date`, `terminal`, `buffer_minutes` and
`no_live`. Its modes are `airport-express`, `airport-express-status`,
`in-town-checkin`, `airport-bus` and `trip`. `get_alerts` takes `include_ael` and
`include_runway` (both default true). See each tool's schema for exact names.

### Resources

| URI | Content |
| --- | --- |
| `hkia://board/today/departures` | Today's departure board JSON |
| `hkia://board/today/arrivals` | Today's arrival board JSON |
| `hkia://alerts/today` | Today's cancellations and 15+ minute delays |
| `hkia://access/ael/status` | Live Airport Express delay flag |
| `hkia://access/ael/HOK` | Live AEL departures, fares and journey times (use any station code) |
| `hkia://access/checkin/CX` | Dated in-town check-in hours (use any airline code) |
| `hkia://access/bus/A11` | Dated A-route windows and fares plus live ETAs (use any route) |
| `hkia://runway/status` | Official ATIS runway in use and airport wind |
| `hkia://runway/history` | Locally observed runway switches, last 20 |

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
- **MCP returns 401:** the client must send `Authorization: Bearer <key>` (or
  `X-API-Key`) when `--api-key`, `HKIA_MCP_API_KEY` or an alias is set. `/health`
  stays public.
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
| `HKIA_MCP_API_KEY` | API key for HTTP/SSE MCP (`HKIA_API_KEY` and `HKIA_AUTH_TOKEN` are aliases) |
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
- Airport Express train data, the delay flag and bus ETAs are live from MTR,
  Citybus, LWB/KMB and NLB APIs. In-town check-in, AEL fare and A-route bus
  facts are dated reference snapshots checked `2026-11-22`, not live
  availability. Bus windows are published terminus departure windows, not
  frequency guarantees, and live ETAs cover the terminus stop only. The trip
  planner uses the 90-minute cutoff, an explicit buffer and the published
  journey time; it does not model security, immigration or walking time.
- Runway in use is live from the CAD ATIS. It is the official broadcast, but it
  changes with wind and operations, arrivals and departures can use different
  runways, and the ATIS can be briefly unavailable while updating. The approach
  side is derived from the runway designator, not from observed flight tracks.
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

`rust-toolchain.toml` pins the stable toolchain with `rustfmt` and
`clippy`, so `rustup` sets contributors up automatically. `just doc`
builds the API documentation. See `CONTRIBUTING.md` for the full check
list and the signed-commit requirement.


Without `just`:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all --locked
```

Tests cover status parsing, midnight rollover, cancellations, codeshares,
filters, stats/alerts, the captured board fixture, watchlist matching and
dedupe, SQLite change detection and notification dedupe, weather parsing, AEL
parsing and trip planning, bus data, ATIS parsing and wind components, and TOML
round-tripping. Integration tests exercise the access CLI JSON/CSV shapes,
station/direction errors, MCP tool mode routing and MCP access/runway
resources. CI runs formatting, clippy, tests, the access data drift check, a
release build and a SKILL.md/script validation job. Tagged releases verify the tag against
`Cargo.toml` and `CHANGELOG.md`, build macOS and static Linux binaries, smoke
test the native artifact, and publish `SHA256SUMS`.

Layout: `src/api.rs` (feed client, normalisation, delays, alerts, stats),
`src/analytics.rs` (historical ranges), `src/store.rs` (SQLite),
`src/watchlist.rs` (config and matching), `src/notify.rs` (desktop/webhook),
`src/weather.rs` (HKO), `src/access.rs` (AEL and bus live clients, trip planner
and dated reference data), `src/runway.rs` (CAD ATIS runway and wind),
`src/render.rs` (tables/CSV), `src/mcp.rs`
(transports, tools, resources), `src/main.rs` (CLI), `scripts/install.sh`
(release installer), `scripts/ensure-binary.sh` (agent resolver),
`scripts/refresh_access_data.py` (access data generator/checker), and `SKILL.md`
(agent skill).

## Data licences

Airport and airline reference data is embedded from OurAirports (public domain)
and OpenFlights (ODbL). Airport Express, in-town check-in and A-route bus
reference facts come from MTR and HKIA pages listed in [NOTICE](NOTICE), and the
runway/ATIS data comes from the Civil Aviation Department. Flight data, the live
train feed and the ATIS page remain subject to their providers' terms; this
project only reads the public endpoints.

## Licence

MIT. See [LICENSE](LICENSE).
