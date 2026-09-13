#!/usr/bin/env python3
"""Live MIDI monitor for a controller (default: the APC40 mkII).

Prints every message the controller sends, as it arrives, with the value shown three
ways — raw MIDI (0-127), the DMX-equivalent (0-255) and the percentage Prisma writes to
a linearly mapped dimmer — plus a bar and the time since the previous message.

It listens through the ALSA sequencer (`aseqdump`, from alsa-utils), so it runs
alongside Prisma without taking the port away from it.

Usage:
    python3 tools/midi-monitor.py            # auto-detect a port matching "apc"/"akai"
    python3 tools/midi-monitor.py 28:0       # explicit ALSA client:port
    python3 tools/midi-monitor.py --cc 7     # only controller 7 (all channels)

How to read a dead zone: move the fader SLOWLY and steadily from the very bottom. If
the first few millimetres print nothing (no new line), the fader itself is not sending
there — a mechanical dead zone. If lines keep printing with changing values while
Prisma shows no change, the problem is in Prisma.
"""

import argparse
import re
import shutil
import subprocess
import sys
import time

BAR_WIDTH = 40


def find_port(keywords=("apc", "akai")):
    out = subprocess.run(["aconnect", "-i"], capture_output=True, text=True).stdout
    client = None
    for line in out.splitlines():
        m = re.match(r"client (\d+): '([^']*)'", line)
        if m:
            client = m.group(1), m.group(2)
            continue
        m = re.match(r"\s+(\d+) '([^']*)'", line)
        if m and client and any(k in client[1].lower() for k in keywords):
            return f"{client[0]}:{m.group(1)}", client[1]
    return None, None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("port", nargs="?", help="ALSA client:port (default: auto-detect APC40)")
    ap.add_argument("--cc", type=int, help="show only this controller number")
    args = ap.parse_args()

    if not shutil.which("aseqdump"):
        sys.exit("aseqdump not found — install alsa-utils")

    port, name = (args.port, args.port) if args.port else find_port()
    if not port:
        sys.exit("No APC40/Akai MIDI port found. Is it plugged in? (see: aconnect -i)")

    print(f"Listening on {name} ({port}). Ctrl+C to stop.\n")
    print(f"{'Δt ms':>7}  {'msg':<9} {'ch':>2} {'num':>3}  {'midi':>4} {'dmx':>3} {'%':>6}  bar")
    print("-" * (40 + BAR_WIDTH), flush=True)

    proc = subprocess.Popen(["aseqdump", "-p", port], stdout=subprocess.PIPE, text=True, bufsize=1)
    cc_re = re.compile(r"Control change\s+(\d+), controller (\d+), value (\d+)")
    note_re = re.compile(r"Note (on|off)\s+(\d+), note (\d+)(?:, velocity (\d+))?")
    last = None
    try:
        for line in proc.stdout:
            now = time.monotonic()
            m = cc_re.search(line)
            if m:
                ch, num, val = (int(g) for g in m.groups())
                if args.cc is not None and num != args.cc:
                    continue
                kind = "CC"
            else:
                m = note_re.search(line)
                if not m or args.cc is not None:
                    continue
                kind = f"note {m.group(1)}"
                ch, num = int(m.group(2)), int(m.group(3))
                val = int(m.group(4) or 0)

            dt = "" if last is None else f"{(now - last) * 1000:7.0f}"
            last = now
            pct = val / 127 * 100
            dmx = round(val / 127 * 255)
            filled = round(val / 127 * BAR_WIDTH)
            bar = "█" * filled + "·" * (BAR_WIDTH - filled)
            # MIDI channels are shown 1-16, as on the hardware (aseqdump prints 0-15).
            print(f"{dt:>7}  {kind:<9} {ch + 1:>2} {num:>3}  {val:>4} {dmx:>3} {pct:5.1f}%  {bar}", flush=True)
    except KeyboardInterrupt:
        pass
    finally:
        proc.terminate()


if __name__ == "__main__":
    main()
