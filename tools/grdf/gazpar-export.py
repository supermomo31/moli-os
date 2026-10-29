#!/usr/bin/env python3
"""Daily gas consumption from a GRDF "Mon espace" account, as JSON lines
for `moli-os energy import`.

GRDF has no public API for individuals (ADICT is a company service): this
goes through the account the way a browser does, via the PyGazpar
library. Gazpar transmits once a day and GRDF publishes at J-1 or J-2:
there is no live figure. Each day's kWh lands in the first hour of its
date (local time) — the day view is the right one.

Credentials come from the environment, never the command line, and are
never stored here:

    pip install pygazpar
    GRDF_USERNAME=… GRDF_PASSWORD=… GRDF_PCE=… \
      tools/grdf/gazpar-export.py | docker exec -i moli-os /moli-os energy import

Usage: gazpar-export.py [--days N] [--meter ID] [--pce PCE] [--tz ZONE]
"""
import argparse
import datetime as dt
import json
import os
import sys
from zoneinfo import ZoneInfo


def day_ms(period, tz):
    """'13/10/2022' -> local midnight of that date, ms since epoch (or None)."""
    try:
        date = dt.datetime.strptime(period, "%d/%m/%Y").date()
    except ValueError:
        return None
    return int(dt.datetime.combine(date, dt.time.min, tzinfo=tz).timestamp() * 1000)


def main() -> None:
    args = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    args.add_argument("--days", type=int, default=10, help="days to fetch (re-import is idempotent)")
    args.add_argument("--meter", default="gaz", help="id of the [[energy.meter]] to feed")
    args.add_argument("--pce", default=os.environ.get("GRDF_PCE"), help="meter id on the bill")
    args.add_argument("--tz", default="Europe/Paris", help="must match [energy] timezone")
    a = args.parse_args()

    try:
        import pygazpar
    except ImportError:
        sys.exit("pip install pygazpar")
    user, password = os.environ.get("GRDF_USERNAME"), os.environ.get("GRDF_PASSWORD")
    if not user or not password:
        sys.exit("GRDF_USERNAME and GRDF_PASSWORD must be set in the environment")

    client = pygazpar.Client(pygazpar.JsonWebDataSource(username=user, password=password))
    pce = a.pce
    if not pce:
        pces = client.get_pce_identifiers()
        sys.exit(f"--pce needed; the account knows: {', '.join(pces) or 'none'}")

    data = client.load_since(
        pce_identifier=pce,
        last_n_days=a.days,
        frequencies=[pygazpar.Frequency.DAILY],
    )
    tz = ZoneInfo(a.tz)
    rows, estimated = 0, 0
    for entry in data.get("daily") or []:
        if "estim" in str(entry.get("type") or "").lower():
            estimated += 1  # provisional: skip it, the measured figure will come
            continue
        kwh, hour = entry.get("energy_kwh"), day_ms(str(entry.get("time_period") or ""), tz)
        if kwh is None or hour is None:
            continue
        print(json.dumps({"meter": a.meter, "hour": hour, "kwh": float(kwh)}))
        rows += 1
    print(f"{rows} days exported, {estimated} estimated skipped (corrected later by GRDF)", file=sys.stderr)


if __name__ == "__main__":
    main()
