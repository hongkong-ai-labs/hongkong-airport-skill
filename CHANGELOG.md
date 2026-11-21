# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `hkia access ael`, `status`, `checkin`, `bus` and `trip`: live Airport
  Express next trains and delay status from the MTR Next Train API, dated
  in-town check-in hours for participating airlines, all 30 HKIA A-routes in
  both directions with fares and city/airport windows, live Citybus, LWB/KMB
  and NLB ETAs for a chosen route, and an AEL trip planner with an explicit
  buffer. All support JSON/CSV except `trip`/`status`; static facts carry
  `checked_on` and source URLs.
- Official MTR Airport Express fares, including child and 30-day round-trip
  fares, replacing hand-maintained fare tables.
- AEL delay status in `hkia alerts` and `hkia watch` (including change
  notifications) with `--no-ael` to opt out.
- MCP `get_airport_access` modes `airport-express`, `airport-express-status`,
  `in-town-checkin`, `airport-bus` and `trip`, plus AEL status in `get_alerts`.
- MCP resources `hkia://access/ael/{station}`, `hkia://access/ael/status`,
  `hkia://access/checkin/{airline}` and `hkia://access/bus/{route}`.
- `scripts/refresh_access_data.py --write/--check` and a weekly scheduled
  workflow that regenerates or verifies the access reference data against the
  official HKIA, MTR and operator sources.
- CLI and MCP integration tests for access JSON shapes, station/direction
  routing, trip assumptions, and access resource reads.
- Manual no-Node skill installation, a community Rust `skills-cli` path, and a
  run-mode overview covering CLI, JSON, CSV, watch, Docker and MCP transports.
- MCP client setup examples for Claude Code, Codex, Cursor, authenticated HTTP
  and the third-party `add-mcp` config helper.

## [0.1.0] - 2026-11-21

### Added

- `hkia departures`, `arrivals`, `flights`, `flight`, `delays`, `alerts` and
  `stats`: live HKIA schedules, estimated/actual times, computed delays,
  cancellations, codeshares, terminal, gate, stand, check-in aisle, baggage
  belt and arrival hall, as tables, JSON or CSV.
- English, Traditional Chinese and Simplified Chinese status parsing for
  departures, arrivals, estimates and cancellations, including the `--lang`
  option and MCP `lang` parameter.
- Derived `guidance` strings for departures and arrivals, including gate,
  check-in aisle, stand, baggage belt and arrival hall instructions; cancelled
  flights report `Flight cancelled` instead of directing the user to a gate.
- `hkia stats --from ... --to ...` historical range analytics for the feed's
  D-91 to D+14 window, grouped by day, airline, route, weekday or hour, with
  per-day failure reporting.
- `hkia watch` with a persistent SQLite baseline, change-only output, NDJSON
  ticks, desktop notifications and webhook delivery.
- `hkia watchlist` management and targeted alerts for flights, airports and
  airlines, with direction, minimum-delay and cancellation settings.
- `hkia history <flight>`: locally observed status/gate/belt timeline from the
  SQLite store, with notification deduplication and 90-day pruning.
- MCP server over stdio, streamable HTTP and legacy SSE, with bearer auth,
  per-minute rate limiting, and `/health`.
- MCP tools `list_flights`, `search_flight`, `get_delays`, `get_alerts`,
  `get_stats`, `get_watchlist_alerts`, `get_flight_history`,
  `get_historical_stats`, `wait_for_changes` and `get_weather`.
- MCP resources `hkia://board/today/departures`,
  `hkia://board/today/arrivals` and `hkia://alerts/today`, with legacy
  subscribe/unsubscribe support.
- Hong Kong Observatory weather warnings as disruption context for CLI alerts,
  watch mode and MCP.
- Embedded OurAirports and OpenFlights reference data for airport and airline
  names, with attribution in `NOTICE`.
- `SKILL.md` at the repository root, installable with
  `npx skills add hongkong-ai-labs/hongkong-airport-skill`.
- `scripts/ensure-binary.sh` and a checksum-verifying `scripts/install.sh` for
  prebuilt macOS and static Linux binaries, with a source-build fallback.
- Dockerfile and Compose service for running the HTTP MCP server with
  persistent history and configuration.
- Documentation for language support, per-command filters, MCP absolute paths
  and TLS/auth limits, troubleshooting, and the project's unofficial status.
- Unit and fixture tests for status parsing, midnight rollover, cancellations,
  codeshares, filtering, stats/alerts, watchlist matching, SQLite change
  detection, notification dedupe, weather parsing and TOML round-tripping.
