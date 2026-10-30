#!/usr/bin/env python3
"""Electricity consumption from an Enedis "point de livraison" (Linky),
as JSON lines for `moli-os energy import`.

Enedis' API for individuals is Data Connect, reached here through the
MyElectricalData gateway: the account holder consents once on
https://www.myelectricaldata.fr (that also asks Enedis to switch the
hourly collection on — it can take days to start) and gets a token good
for up to three years. Like Gazpar, Enedis publishes at J-1 or J-2:
no live figure.

Two granularities, same `energy import` rows:

    --granularity hourly   30-min load curve summed per hour (7 days per
                           call, Enedis' own limit): the daily sync.
    --granularity daily    kWh per day, whole years per call: backfills.
                           Each day lands in its first hour — never mix
                           it with hourly data for the same day.

Credentials come from the environment, never the command line, and are
never stored here:

    ENEDIS_TOKEN=… ENEDIS_PDL=… \
      tools/enedis/enedis-export.py | docker exec -i moli-os /moli-os energy import

Usage: enedis-export.py [--days N] [--meter ID] [--pdl PDL] [--tz ZONE]
                        [--granularity hourly|daily]
"""
import argparse
import datetime as dt
import json
import os
import re
import sys
import urllib.error
import urllib.request
from zoneinfo import ZoneInfo

BASE = "https://www.myelectricaldata.fr"
# Enedis bounds a load-curve request to 7 consecutive days; daily reads
# go by whole years. Chunks also keep one bad range from losing the rest.
CURVE_CHUNK_DAYS = 7
DAILY_CHUNK_DAYS = 360


def get(path, token):
    req = urllib.request.Request(f"{BASE}{path}", headers={"Authorization": token})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        if e.code in (404, 409):  # no consent yet, or no data on that range
            return None
        if e.code in (401, 403, 429):  # bad token, or throttled: stop, don't burn the quota
            sys.exit(f"{path}: HTTP {e.code} {e.read()[:160]!r}")
        # Enedis answers a flaky 500 for ranges that hold nothing, and for
        # transient faults: skip the chunk, try the next one.
        print(f"{path}: HTTP {e.code}, chunk skipped", file=sys.stderr)
        return None
    except urllib.error.URLError as e:
        print(f"{path}: {e.reason}, chunk skipped", file=sys.stderr)
        return None


def local_ms(text, tz, fmt):
    try:
        naive = dt.datetime.strptime(text.replace("T", " "), fmt)
    except ValueError:
        return None
    return int(naive.replace(tzinfo=tz).timestamp() * 1000)


def interval_seconds(length):
    """'PT30M' / 'PT60M' / 'PT10M' / 'P1D' -> seconds."""
    m = re.fullmatch(r"P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?)?", str(length or ""))
    if not m:
        return None
    d, h, mi, s = (int(g) if g else 0 for g in m.groups())
    return d * 86400 + h * 3600 + mi * 60 + s


def fetch_range(endpoint, chunk_days, pdl, token, first, last):
    """Read [first, last] in Enedis-sized chunks; never one call too wide."""
    out, unit = [], None
    cur = first
    while cur <= last:
        end = min(last, cur + dt.timedelta(days=chunk_days - 1))
        body = get(f"/{endpoint}/{pdl}/start/{cur}/end/{end}", token)
        mr = (body or {}).get("meter_reading") or {}
        unit = unit or (mr.get("reading_type") or {}).get("unit")
        out.extend(mr.get("interval_reading") or [])
        cur = end + dt.timedelta(days=1)
    return out, unit


def main() -> None:
    args = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    args.add_argument("--days", type=int, default=10, help="days to fetch (re-import is idempotent)")
    args.add_argument("--until", type=int, default=0,
                      help="stop this many days ago: a daily backfill must end before the hourly window")
    args.add_argument("--meter", default="elec", help="id of the [[energy.meter]] to feed")
    args.add_argument("--pdl", default=os.environ.get("ENEDIS_PDL"), help="point de livraison (on the bill)")
    args.add_argument("--tz", default="Europe/Paris", help="must match [energy] timezone")
    args.add_argument("--granularity", choices=("hourly", "daily"), default="hourly",
                      help="hourly: 30-min curve summed per hour; daily: one kWh per day (backfill)")
    a = args.parse_args()

    token = os.environ.get("ENEDIS_TOKEN")
    if not token:
        sys.exit("ENEDIS_TOKEN must be set in the environment (consent once on myelectricaldata.fr)")
    if not a.pdl:
        sys.exit("--pdl or ENEDIS_PDL needed (the 'point de livraison', 14 digits on the bill)")

    tz = ZoneInfo(a.tz)
    today = dt.datetime.now(tz).date()
    last = today - dt.timedelta(days=a.until)
    first = last - dt.timedelta(days=a.days - 1)

    if a.granularity == "daily":
        entries, unit = fetch_range("daily_consumption", DAILY_CHUNK_DAYS, a.pdl, token, first, last)
        scale = 0.001 if unit == "Wh" else 1.0  # Wh per Enedis; kWh accepted too
        rows = 0
        for e in entries:
            hour = local_ms(str(e.get("date") or ""), tz, "%Y-%m-%d")
            if hour is None or e.get("value") is None:
                continue
            print(json.dumps({"meter": a.meter, "hour": hour, "kwh": float(e["value"]) * scale}))
            rows += 1
        print(f"{rows} days exported", file=sys.stderr)
        return

    # Hourly: each reading is the mean power (W) over the interval that
    # ENDS at 'date'. It belongs to the hour holding its start.
    entries, unit = fetch_range("consumption_load_curve", CURVE_CHUNK_DAYS, a.pdl, token, first, last)
    watts = 1000.0 if unit == "kW" else 1.0
    hours = {}
    for e in entries:
        secs = interval_seconds(e.get("interval_length"))
        end_ms = local_ms(str(e.get("date") or ""), tz, "%Y-%m-%d %H:%M:%S")
        if not secs or end_ms is None or e.get("value") is None:
            continue
        hour = (end_ms - secs * 1000) // 3_600_000 * 3_600_000
        hours[hour] = hours.get(hour, 0.0) + float(e["value"]) * watts * secs / 3_600_000 / 1000
    for hour in sorted(hours):
        print(json.dumps({"meter": a.meter, "hour": hour, "kwh": hours[hour]}))
    print(f"{len(hours)} hours exported from {len(entries)} readings", file=sys.stderr)


if __name__ == "__main__":
    main()
