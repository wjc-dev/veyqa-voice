#!/usr/bin/env python3
"""Measure explicit live PIDs without reading credentials, audio, or text content.

CPU is cumulative user+system time divided by elapsed wall time (one-core %).
RSS is resident memory, not macOS physical footprint or total system impact.
Run only with recording/settings closed; process replacement fails the sample.
"""
import argparse
import json
import subprocess
import time


def cpu_seconds(value):
    days, _, rest = value.rpartition("-")
    parts = [float(x) for x in (rest if days else value).split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return seconds + (int(days) * 86400 if days else 0)


def snapshot(pids):
    result = subprocess.run(
        ["/bin/ps", "-p", ",".join(map(str, pids)), "-o", "pid=,time=,rss=,lstart="],
        text=True, capture_output=True, check=True,
    )
    values = {}
    for line in result.stdout.splitlines():
        pid, cpu, rss, started = line.split(maxsplit=3)
        values[int(pid)] = {"cpu_seconds": cpu_seconds(cpu), "rss_mib": int(rss) / 1024, "started": started}
    if set(values) != set(pids):
        raise RuntimeError("A measured process exited; sample is invalid")
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pids", nargs="+", type=int)
    parser.add_argument("--seconds", type=float, default=60)
    args = parser.parse_args()
    if min(args.pids) <= 1 or args.seconds < 10:
        parser.error("Use explicit PIDs > 1 and a duration of at least 10 seconds")
    before = snapshot(args.pids)
    start = time.monotonic()
    time.sleep(args.seconds)
    after = snapshot(args.pids)
    elapsed = time.monotonic() - start
    rows = []
    for pid in args.pids:
        if before[pid]["started"] != after[pid]["started"]:
            raise RuntimeError("Process identity changed; sample is invalid")
        rows.append({"pid": pid, "cpu_percent_one_core": round(100 * (after[pid]["cpu_seconds"] - before[pid]["cpu_seconds"]) / elapsed, 3),
                     "rss_mib_start": round(before[pid]["rss_mib"], 2), "rss_mib_end": round(after[pid]["rss_mib"], 2)})
    total = round(sum(row["cpu_percent_one_core"] for row in rows), 3)
    print(json.dumps({"elapsed_seconds": round(elapsed, 2), "processes": rows,
                      "total_cpu_percent_one_core": total, "cpu_gate_0_5_percent_passed": total <= 0.5}, indent=2))


if __name__ == "__main__":
    main()
