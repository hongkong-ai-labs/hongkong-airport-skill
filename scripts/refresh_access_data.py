#!/usr/bin/env python3
"""Refresh or verify the dated airport-access reference data.

Sources:
- HKIA public-bus page JSON (both directions, fares, remarks, route URLs)
- MTR in-town check-in page
- MTR Airport Express fares CSV
- Operator route/stop APIs for live-ETA stop IDs

Usage:
    python3 scripts/refresh_access_data.py --write
    python3 scripts/refresh_access_data.py --check

The generated files are embedded in the binary. `--check` ignores the
`checked_on` date and compares the actual rows, so it is suitable for CI.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import re
import sys
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "data"

BUS_URL = (
    "https://www.hongkongairport.com/iwov-resources/custom/json/"
    "transport_bus_en.json"
)
CHECKIN_URL = "https://www.mtr.com.hk/en/customer/services/complom_checkin.html"
AEL_FARES_URL = "https://opendata.mtr.com.hk/data/airport_express_fares.csv"
CITYBUS_STOP_URL = "https://rt.data.gov.hk/v2/transport/citybus/stop/{}"
CITYBUS_ROUTE_STOP_URL = (
    "https://rt.data.gov.hk/v2/transport/citybus/route-stop/CTB/{}/{}"
)
CITYBUS_ETA_URL = "https://rt.data.gov.hk/v2/transport/citybus/eta/CTB/{}/{}"
KMB_STOP_URL = "https://data.etabus.gov.hk/v1/transport/kmb/stop/{}"
KMB_ROUTE_STOP_URL = (
    "https://data.etabus.gov.hk/v1/transport/kmb/route-stop/{}/{}/1"
)
KMB_ETA_URL = "https://data.etabus.gov.hk/v1/transport/kmb/eta/{}/{}/1"
NLB_ROUTE_URL = "https://rt.data.gov.hk/v2/transport/nlb/route.php?action=list"
NLB_STOP_URL = (
    "https://rt.data.gov.hk/v2/transport/nlb/stop.php?action=list&routeId={}"
)
NLB_ETA_URL = (
    "https://rt.data.gov.hk/v2/transport/nlb/stop.php?"
    "action=estimatedArrivals&routeId={}&stopId={}&language=en"
)

CHECKIN_AIRLINES = {
    "Cathay Pacific": ("CX", "CPA"),
    "China Airlines": ("CI", "CAL"),
    "EVA Air": ("BR", "EVA"),
    "Hong Kong Airlines": ("HX", "CRK"),
    "Mandarin Airlines": ("AE", "MDA"),
    "Qantas Airways": ("QF", "QFA"),
    "Singapore Airlines": ("SQ", "SIA"),
}

AEL_STATIONS = {
    "44": "HOK",
    "45": "KOW",
    "46": "TSY",
    "47": "AIR",
    "56": "AWE",
}
ROUND_TRIPS = {"HOK": 215.0, "KOW": 195.0, "TSY": 130.0}


def hkt_today() -> str:
    return datetime.now(timezone(timedelta(hours=8))).date().isoformat()


def fetch(url: str, timeout: int = 30) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": "hkia-access-refresh/1"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def fetch_json(url: str):
    return json.loads(fetch(url).decode("utf-8-sig"))


def clean_html(value: str) -> str:
    value = re.sub(r"<[^>]+>", " ", value)
    return re.sub(r"\s+", " ", value).strip()


def parse_clock(value: str) -> str:
    match = re.fullmatch(r"(\d{1,2}):(\d{2})\s*(am|pm)", value.strip(), re.I)
    if not match:
        raise ValueError(f"unrecognized time: {value!r}")
    hour = int(match.group(1)) % 12
    if match.group(3).lower() == "pm":
        hour += 12
    return f"{hour:02d}:{match.group(2)}"


def parse_hours(value: str) -> str | None:
    text = clean_html(value)
    if "not available" in text.lower():
        return None
    match = re.fullmatch(
        r"(\d{1,2}:\d{2}\s*(?:am|pm))\s+to\s+(\d{1,2}:\d{2}\s*(?:am|pm))",
        text,
        re.I,
    )
    if not match:
        raise ValueError(f"unrecognized check-in hours: {text!r}")
    return f"{parse_clock(match.group(1))}-{parse_clock(match.group(2))}"


def parse_checkin() -> dict:
    html = fetch(CHECKIN_URL).decode("utf-8", "replace")
    table = re.search(r"<table[^>]*>(.*?)</table>", html, re.S)
    if not table:
        raise RuntimeError("MTR check-in table not found")
    rows = re.findall(r"<tr[^>]*>(.*?)</tr>", table.group(1), re.S)
    airlines = []
    for row in rows[1:]:
        cells = re.findall(r"<td[^>]*>(.*?)</td>", row, re.S)
        if len(cells) != 3:
            continue
        name = clean_html(cells[0])
        if name not in CHECKIN_AIRLINES:
            raise RuntimeError(f"unexpected check-in airline: {name!r}")
        iata, icao = CHECKIN_AIRLINES[name]
        airlines.append(
            {
                "iata": iata,
                "icao": icao,
                "name": name,
                "hong_kong_station": parse_hours(cells[1]),
                "kowloon_station": parse_hours(cells[2]),
            }
        )
    if len(airlines) != len(CHECKIN_AIRLINES):
        raise RuntimeError(f"expected {len(CHECKIN_AIRLINES)} airlines, got {len(airlines)}")
    return {
        "checked_on": hkt_today(),
        "source": {
            "name": "MTR in-town check-in service",
            "url": CHECKIN_URL,
        },
        "airlines": airlines,
    }


def parse_ael_fares() -> dict:
    reader = csv.DictReader(io.StringIO(fetch(AEL_FARES_URL).decode("utf-8-sig")))
    fares = []
    for row in reader:
        origin = AEL_STATIONS.get(row["ST_FROM_ID"])
        destination = AEL_STATIONS.get(row["ST_TO_ID"])
        if not origin or not destination:
            continue
        round_trip = None
        if {origin, destination} == {"HOK", "AIR"}:
            round_trip = ROUND_TRIPS["HOK"]
        elif {origin, destination} == {"KOW", "AIR"}:
            round_trip = ROUND_TRIPS["KOW"]
        elif {origin, destination} == {"TSY", "AIR"}:
            round_trip = ROUND_TRIPS["TSY"]
        fares.append(
            {
                "from": origin,
                "to": destination,
                "adult_octopus_hkd": float(row["OCT_ADT_FARE"]),
                "child_octopus_hkd": float(row["OCT_CHD_FARE"]),
                "adult_single_hkd": float(row["SINGLE_ADT_FARE"]),
                "child_single_hkd": float(row["SINGLE_CHD_FARE"]),
                "round_trip_30d_hkd": round_trip,
            }
        )
    if len(fares) != 14:
        raise RuntimeError(f"expected 14 AEL fare rows, got {len(fares)}")
    return {
        "checked_on": hkt_today(),
        "source": {
            "name": "MTR Airport Express fares",
            "url": AEL_FARES_URL,
        },
        "fares": fares,
    }


def operator_kind(name: str) -> str:
    if name == "Citybus":
        return "ctb"
    if name == "Long Win":
        return "lwb"
    if name == "New Lantao":
        return "nlb"
    raise RuntimeError(f"unsupported bus operator: {name!r}")


def ctb_stop(stop_id: str) -> dict:
    return fetch_json(CITYBUS_STOP_URL.format(stop_id))["data"]


def ctb_route_stop(route: str, direction: str) -> list[dict]:
    data = fetch_json(CITYBUS_ROUTE_STOP_URL.format(route, direction)).get("data", [])
    return data


def lwb_stop(stop_id: str) -> dict:
    return fetch_json(KMB_STOP_URL.format(stop_id))["data"]


def lwb_route_stop(route: str, direction: str) -> list[dict]:
    data = fetch_json(KMB_ROUTE_STOP_URL.format(route, direction)).get("data", [])
    return data


def nlb_route_ids(route: str) -> tuple[int | None, int | None]:
    routes = fetch_json(NLB_ROUTE_URL).get("routes", [])
    matches = [item for item in routes if item.get("routeNo") == route]
    to_airport = next(
        (
            item
            for item in matches
            if not item.get("routeName_e", "").upper().startswith("HZMB")
        ),
        None,
    )
    from_airport = next(
        (
            item
            for item in matches
            if item.get("routeName_e", "").upper().startswith("HZMB")
        ),
        None,
    )
    return (
        int(to_airport["routeId"]) if to_airport else None,
        int(from_airport["routeId"]) if from_airport else None,
    )


def nlb_stops(route_id: int) -> list[dict]:
    return fetch_json(NLB_STOP_URL.format(route_id)).get("stops", [])


def live_stop_info(route: str, operator: str, destination: str) -> dict:
    """Resolve the live-ETA stop for the airport-bound and city-bound directions."""
    kind = operator_kind(operator)
    to_airport = None
    from_airport = None
    nlb_to_id = None
    nlb_from_id = None

    if kind == "ctb":
        outbound = ctb_route_stop(route, "outbound")
        inbound = ctb_route_stop(route, "inbound")
        if outbound:
            stop = ctb_stop(outbound[0]["stop"])
            to_airport = {
                "stop_id": stop["stop"],
                "stop_name": stop["name_en"],
            }
        if inbound:
            stop = ctb_stop(inbound[0]["stop"])
            from_airport = {
                "stop_id": stop["stop"],
                "stop_name": stop["name_en"],
            }
    elif kind == "lwb":
        outbound = lwb_route_stop(route, "outbound")
        inbound = lwb_route_stop(route, "inbound")
        if outbound:
            stop = lwb_stop(outbound[0]["stop"])
            to_airport = {
                "stop_id": stop["stop"],
                "stop_name": stop["name_en"],
            }
        if inbound:
            stop = lwb_stop(inbound[0]["stop"])
            from_airport = {
                "stop_id": stop["stop"],
                "stop_name": stop["name_en"],
            }
    elif kind == "nlb":
        nlb_to_id, nlb_from_id = nlb_route_ids(route)
        if nlb_to_id:
            stops = nlb_stops(nlb_to_id)
            if stops:
                to_airport = {
                    "stop_id": str(stops[0]["stopId"]),
                    "stop_name": stops[0]["stopName_e"],
                }
        if nlb_from_id:
            stops = nlb_stops(nlb_from_id)
            airport = next(
                (stop for stop in stops if "airport" in stop.get("stopName_e", "").lower()),
                None,
            )
            if airport:
                from_airport = {
                    "stop_id": str(airport["stopId"]),
                    "stop_name": airport["stopName_e"],
                }
    else:
        raise RuntimeError(f"unsupported operator: {operator!r}")

    return {
        "to_airport": to_airport,
        "from_airport": from_airport,
        "nlb_to_airport_route_id": nlb_to_id,
        "nlb_from_airport_route_id": nlb_from_id,
    }


def remarks(entry: dict) -> list[str]:
    return [item.get("content", "") for item in entry.get("remark", []) if item.get("content")]


def parse_bus() -> dict:
    payload = fetch_json(BUS_URL)
    table = payload["public-buses"]["table"]
    to_airport = {}
    from_airport = {}
    for group in table["to-airport"].values():
        for item in group.get("route", []):
            for route, entry in item.items():
                to_airport[route] = entry
    for group in table["from-airport"].values():
        for item in group.get("route", []):
            for route, entry in item.items():
                from_airport[route] = entry

    routes = []
    for route in sorted(set(to_airport) | set(from_airport)):
        if not route.startswith("A"):
            continue
        to_entry = to_airport.get(route)
        from_entry = from_airport.get(route)
        base = to_entry or from_entry
        operator = base["service-provider"]
        destination = to_entry["origin"] if to_entry else None
        if not destination:
            raise RuntimeError(f"no city terminus for {route}")
        live = live_stop_info(route, operator, destination)
        fare_to = None
        fare_from = None
        fare_to_note = None
        fare_from_note = None
        if to_entry:
            fare_to = float(to_entry.get("fare-normal-day") or to_entry.get("fare-pub-day"))
        if from_entry:
            fare_from = float(
                from_entry.get("fare-normal-day") or from_entry.get("fare-pub-day")
            )
        if operator == "New Lantao":
            # NLB publishes weekday and Sunday/public-holiday fares separately.
            if live["nlb_to_airport_route_id"]:
                stops = nlb_stops(live["nlb_to_airport_route_id"])
                if stops:
                    stop = stops[0]
                    if float(stop["fareHoliday"]) != float(stop["fare"]):
                        fare_to_note = (
                            f"HK${float(stop['fareHoliday']):g} on Sundays and public holidays"
                        )
            if live["nlb_from_airport_route_id"]:
                stops = nlb_stops(live["nlb_from_airport_route_id"])
                airport = next(
                    (stop for stop in stops if "airport" in stop.get("stopName_e", "").lower()),
                    None,
                )
                if airport and float(airport["fareHoliday"]) != float(airport["fare"]):
                    fare_from_note = (
                        f"HK${float(airport['fareHoliday']):g} on Sundays and public holidays"
                    )
        routes.append(
            {
                "route": route,
                "operator": operator,
                "destination": destination,
                "to_airport_hours": to_entry.get("operating-hours") if to_entry else None,
                "from_airport_hours": from_entry.get("operating-hours") if from_entry else None,
                "fare_to_airport_hkd": fare_to,
                "fare_from_airport_hkd": fare_from,
                "fare_to_airport_note": fare_to_note,
                "fare_from_airport_note": fare_from_note,
                "notes_to_airport": remarks(to_entry) if to_entry else [],
                "notes_from_airport": remarks(from_entry) if from_entry else [],
                "route_url": base.get("route-url", ""),
                "live": live,
            }
        )
    if len(routes) != 30:
        raise RuntimeError(f"expected 30 A-routes, got {len(routes)}")
    return {
        "checked_on": hkt_today(),
        "source": {
            "name": "HKIA public buses",
            "url": "https://www.hongkongairport.com/en/transport/to-from-airport/public-buses.page",
        },
        "routes": routes,
    }


def write_json(path: Path, payload: dict) -> None:
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def comparable(payload: dict, keys: list[str]) -> dict:
    return {key: payload.get(key) for key in keys}


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true", help="refresh data files")
    mode.add_argument("--check", action="store_true", help="fail if official data changed")
    args = parser.parse_args()

    generated = {
        "access_checkin.json": (parse_checkin(), ["airlines"]),
        "access_bus.json": (parse_bus(), ["routes"]),
        "access_ael_fares.json": (parse_ael_fares(), ["fares"]),
    }

    if args.write:
        for name, (payload, _) in generated.items():
            write_json(DATA / name, payload)
            print(f"wrote data/{name}")
        return 0

    failed = False
    for name, (payload, keys) in generated.items():
        current = json.loads((DATA / name).read_text(encoding="utf-8"))
        if comparable(current, keys) != comparable(payload, keys):
            failed = True
            print(f"drift detected in data/{name}", file=sys.stderr)
        else:
            print(f"ok data/{name}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
