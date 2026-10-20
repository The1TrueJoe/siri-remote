#!/usr/bin/env python3
"""The smallest useful glue: react to button presses.

    siri-events listen DC:2B:2A:E0:18:71 | ./minimal.py
"""
import json
import sys

for line in sys.stdin:
    try:
        ev = json.loads(line)
    except json.JSONDecodeError:
        continue

    if ev.get("event") == "button" and ev.get("state") == "down":
        print("pressed:", ev["button"], flush=True)
    elif ev.get("event") == "swipe":
        print("swiped:", ev["direction"], flush=True)
