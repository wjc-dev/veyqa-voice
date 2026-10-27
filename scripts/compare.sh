#!/bin/bash
# Side-by-side comparison of the production Python app and the Rust rewrite.
#
#   scripts/compare.sh sample [seconds]   # RSS/CPU for both main processes
#   scripts/compare.sh mark               # open the comparison window now
#   scripts/compare.sh report [--all]     # latency/outcome summary from both logs
#
# Never launches or quits either app. `mark` only remembers how far the Rust
# metrics file had grown, so earlier experiments stay out of the report.
set -euo pipefail

RUST_MATCH="${VEYQA_RUST_MATCH:-/Veyqa Voice Rust.app/Contents/MacOS/veyqa-voice-rs}"
PY_MATCH="${VEYQA_PY_MATCH:-/Veyqa Voice.app/Contents/MacOS/VeyqaVoice}"
RUST_DATA="${VOICE_INPUT_DATA_DIR:-$HOME/Library/Application Support/Veyqa-Rust-Dev}"
PY_DATA="${VEYQA_PY_DATA:-$HOME/Library/Application Support/Veyqa}"
MARK_FILE="${VEYQA_COMPARE_MARK:-/tmp/veyqa-compare-mark.json}"
SAMPLES="${VEYQA_COMPARE_SAMPLES:-/tmp/veyqa-compare-samples.csv}"

pid_of() {
  pgrep -f "$1" | head -n 1 || true
}

sample() {
  local seconds="${1:-60}"
  [[ "$seconds" =~ ^[0-9]+$ ]] || { echo "seconds must be an integer" >&2; exit 2; }
  local rust_pid py_pid
  rust_pid="$(pid_of "$RUST_MATCH")"
  py_pid="$(pid_of "$PY_MATCH")"
  [[ -n "$rust_pid" ]] || { echo "Rust app is not running ($RUST_MATCH)" >&2; exit 1; }
  [[ -n "$py_pid" ]] || { echo "Production app is not running ($PY_MATCH)" >&2; exit 1; }

  echo "timestamp,rust_pid,rust_rss_kb,rust_cpu_s,python_pid,python_rss_kb,python_cpu_s" >"$SAMPLES"
  local end=$((SECONDS + seconds)) rss cpu prss pcpu
  while (( SECONDS < end )); do
    read -r rss cpu <<<"$(ps -o rss= -o time= -p "$rust_pid" | awk 'NF')" || break
    read -r prss pcpu <<<"$(ps -o rss= -o time= -p "$py_pid" | awk 'NF')" || break
    [[ -n "${rss:-}" && -n "${prss:-}" ]] || break
    printf '%s,%s,%s,%s,%s,%s,%s\n' \
      "$(date +%s)" "$rust_pid" "$rss" "$cpu" "$py_pid" "$prss" "$pcpu" >>"$SAMPLES"
    sleep 1
  done

  python3 - "$SAMPLES" <<'PY'
import csv, math, sys

def cpu_seconds(value):
    parts = value.split(":")
    if len(parts) == 3:
        return int(parts[0]) * 3600 + int(parts[1]) * 60 + float(parts[2])
    return int(parts[0]) * 60 + float(parts[1])

def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, min(len(ordered), math.ceil(fraction * len(ordered))))
    return ordered[rank - 1]

rows = list(csv.DictReader(open(sys.argv[1])))
def summarize(prefix):
    rss, pct = [], []
    previous = None
    for row in rows:
        rss.append(int(row[f"{prefix}_rss_kb"]))
        current = (int(row["timestamp"]), cpu_seconds(row[f"{prefix}_cpu_s"]))
        if previous and current[0] > previous[0]:
            pct.append((current[1] - previous[1]) / (current[0] - previous[0]) * 100.0)
        previous = current
    return rss, pct

print("app,rss_median_kb,rss_p90_kb,rss_max_kb,cpu_median_pct,cpu_p90_pct,cpu_max_pct,samples")
for label, prefix in (("old_python", "python"), ("new_rust", "rust")):
    rss, pct = summarize(prefix)
    if not rss:
        continue
    print(f"{label},{int(percentile(rss, 0.5))},{int(percentile(rss, 0.9))},{max(rss)},"
          f"{percentile(pct, 0.5):.2f},{percentile(pct, 0.9):.2f},{max(pct):.2f},{len(rss)}")
PY
  echo "samples: $SAMPLES"
}

mark() {
  local rust_lines=0
  [[ -f "$RUST_DATA/logs/session-metrics.jsonl" ]] \
    && rust_lines="$(wc -l <"$RUST_DATA/logs/session-metrics.jsonl" | tr -d ' ')"
  python3 - "$MARK_FILE" "$rust_lines" <<'PY'
import datetime, json, sys
json.dump({
    "marked_at": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
    "rust_metrics_lines": int(sys.argv[2]),
}, open(sys.argv[1], "w"), indent=1)
PY
  echo "comparison window opened: $(cat "$MARK_FILE")"
}

report() {
  python3 - "$RUST_DATA" "$PY_DATA" "$MARK_FILE" "$@" <<'PY'
import datetime, json, math, os, re, sys

rust_data, py_data, mark_file = sys.argv[1:4]
include_all = "--all" in sys.argv[4:]

mark = None
if os.path.exists(mark_file):
    mark = json.load(open(mark_file))

def timestamp(value):
    return value.astimezone().replace(tzinfo=None) if value.tzinfo else value

def parse_at(value):
    try:
        return timestamp(datetime.datetime.fromisoformat(value))
    except (TypeError, ValueError):
        return None

explicit_since = None
args = sys.argv[4:]
if "--since" in args:
    try:
        explicit_since = timestamp(datetime.datetime.fromisoformat(args[args.index("--since") + 1]))
    except (IndexError, ValueError):
        print("--since needs an ISO timestamp, e.g. 2026-10-26T17:18:00+08:00", file=sys.stderr)
        raise SystemExit(2)

def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, min(len(ordered), math.ceil(fraction * len(ordered))))
    return ordered[rank - 1]

def number(value):
    return value if isinstance(value, (int, float)) else None

def fmt(value):
    return "-" if value is None else f"{value:.0f}" if isinstance(value, float) else str(value)

# ---- Rust: logs/session-metrics.jsonl --------------------------------------
rust_lines = []
path = os.path.join(rust_data, "logs", "session-metrics.jsonl")
if os.path.exists(path):
    for line in open(path):
        line = line.strip()
        if line:
            try:
                rust_lines.append(json.loads(line))
            except json.JSONDecodeError:
                pass

rust_offset = mark["rust_metrics_lines"] if mark else 0
rust_since = parse_at(mark["marked_at"]) if mark else None
cutoff = explicit_since
if cutoff and rust_since and rust_since > cutoff:
    cutoff = rust_since
rust_sessions = []
for index, row in enumerate(rust_lines):
    at = parse_at(row.get("at"))
    if cutoff is not None:
        # Lines written before the marker have no timestamp and are skipped.
        if at is None or at < cutoff:
            continue
    elif index < rust_offset:
        continue
    rust_sessions.append(row)

# ---- Production: logs/app.log ----------------------------------------------
line_re = re.compile(
    r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2},\d{3}) - \w+ - (.*)$")
app_log = os.path.join(py_data, "logs", "app.log")
starts = []
sessions = []
open_session = None
for raw in open(app_log):
    match = line_re.match(raw)
    if not match:
        continue
    when = datetime.datetime.strptime(match.group(1), "%Y-%m-%d %H:%M:%S,%f")
    message = match.group(2)

    if message.startswith("语音输入已启动"):
        starts.append(when)
        continue
    if "开始录音" in message:
        if open_session:
            sessions.append(open_session)
        open_session = {"start": when}
        continue
    if open_session is None:
        continue
    ready = re.search(r"麦克风启动就绪：(\d+)ms", message)
    if ready:
        open_session["ready_ms"] = int(ready.group(1))
        continue
    if "停止录音" in message:
        open_session["stop"] = when
        continue
    typed = re.search(r"文本输入完成（(\d+) 个字符）", message)
    if typed:
        open_session["pasted"] = when
        open_session["chars"] = int(typed.group(1))
        continue
    final = re.search(r"本轮最终结果处理完成（(\d+) 个字符，写入=(\w+)）", message)
    if final:
        open_session["final"] = when
        open_session["written"] = final.group(2) == "True"
        sessions.append(open_session)
        open_session = None
if open_session:
    sessions.append(open_session)

if not include_all and starts:
    sessions = [s for s in sessions if s["start"] >= starts[-1]]
if explicit_since:
    sessions = [s for s in sessions if s["start"] >= explicit_since]

def ms_between(session, end_key, start_key):
    if end_key in session and start_key in session:
        delta = session[end_key] - session[start_key]
        return delta.total_seconds() * 1000.0
    return None

old_ready = [s["ready_ms"] for s in sessions if "ready_ms" in s]
old_stop_final = [v for s in sessions if (v := ms_between(s, "final", "stop")) is not None]
old_stop_paste = [v for s in sessions if (v := ms_between(s, "pasted", "stop")) is not None]
old_start_final = [v for s in sessions if (v := ms_between(s, "final", "start")) is not None]
old_finished = [s for s in sessions if "final" in s]
old_committed = [s for s in old_finished if s.get("written")]
old_chars = [s["chars"] for s in old_finished if "chars" in s]

new_ready = [v for row in rust_sessions if (v := number(row.get("key_down_to_audio_ready_ms"))) is not None]
new_partial = [v for row in rust_sessions if (v := number(row.get("key_down_to_first_partial_ms"))) is not None]
new_final = [v for row in rust_sessions if (v := number(row.get("key_up_to_final_ms"))) is not None]
new_paste = [v for row in rust_sessions if (v := number(row.get("key_up_to_paste_ms"))) is not None]
new_start_final = [v for row in rust_sessions if (v := number(row.get("key_down_to_final_ms"))) is not None]
new_committed = [row for row in rust_sessions if row.get("key_down_to_paste_ms") is not None]

outcomes = {}
for row in rust_sessions:
    outcomes[row.get("outcome", "?")] = outcomes.get(row.get("outcome", "?"), 0) + 1

window = [mark["marked_at"]] if mark else ["all rust lines"]
if explicit_since:
    window.append(f"since {explicit_since:%Y-%m-%d %H:%M:%S}")
if not include_all and starts:
    window.append(f"production since {starts[-1]:%Y-%m-%d %H:%M:%S}")

def row(phase, old, new, old_n, new_n):
    print(f"{phase},{fmt(percentile(old, 0.5))},{fmt(percentile(old, 0.9))},{old_n},"
          f"{fmt(percentile(new, 0.5))},{fmt(percentile(new, 0.9))},{new_n}")

print("== window ==")
print("rust_window," + "; ".join(window))
print()
print("== latency_ms ==")
print("phase,old_p50,old_p90,old_n,new_p50,new_p90,new_n")
row("key_to_audio_ready", old_ready, new_ready, len(old_ready), len(new_ready))
row("key_to_first_partial", [], new_partial, 0, len(new_partial))
row("key_up_to_final", old_stop_final, new_final, len(old_stop_final), len(new_final))
row("key_up_to_text_inserted", old_stop_paste, new_paste, len(old_stop_paste), len(new_paste))
row("key_down_to_final", old_start_final, new_start_final, len(old_start_final), len(new_start_final))
print()
print("== outcomes ==")
print("app,started,finished,committed,abandoned,chars_total")
old_abandoned = len(sessions) - len(old_finished)
print(f"old_python,{len(sessions)},{len(old_finished)},{len(old_committed)},{old_abandoned},{sum(old_chars)}")
new_abandoned = len(rust_sessions) - len(new_committed)
print(f"new_rust,{len(rust_sessions)},{len(new_final)},{len(new_committed)},{new_abandoned},-")
print()
print("== rust_outcome_detail ==")
print("outcome,count")
for name, count in sorted(outcomes.items()):
    print(f"{name},{count}")

startup = os.path.join(rust_data, "logs", "startup.jsonl")
if os.path.exists(startup):
    last = [line for line in open(startup) if line.strip()]
    if last:
        try:
            print()
            print("== startup ==")
            print(f"new_rust_startup_ms,{json.loads(last[-1])['startup_ms']}")
        except (json.JSONDecodeError, KeyError):
            pass
PY
}

case "${1:-report}" in
  sample) shift; sample "${1:-60}" ;;
  mark) mark ;;
  report) shift || true; report "$@" ;;
  *) echo "usage: $0 [sample [seconds] | mark | report [--all]]" >&2; exit 2 ;;
esac
