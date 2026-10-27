#!/bin/bash
# Sample the resident app and report the latency logs it writes.
#
#   scripts/measure.sh [seconds]   # RSS/CPU samples as CSV (default 30 s)
#   scripts/measure.sh --report    # latest startup/session/diagnostics summary
#
# Never launches or quits the app; run it against whichever build is running.
set -euo pipefail

DATA_DIR="${VOICE_INPUT_DATA_DIR:-$HOME/Library/Application Support/Veyqa-Rust-Dev}"
MATCH="${VEYQA_PROCESS_MATCH:-/Veyqa Voice Rust.app/Contents/MacOS/veyqa-voice-rs}"

latest_line() {
  [[ -f "$1" ]] && tail -n 1 "$1" || true
}

report() {
  echo "== startup =="
  latest_line "$DATA_DIR/logs/startup.jsonl"
  echo "== last sessions =="
  [[ -f "$DATA_DIR/logs/session-metrics.jsonl" ]] && tail -n 5 "$DATA_DIR/logs/session-metrics.jsonl" || true
  echo "== diagnostics =="
  if [[ -f "$DATA_DIR/diagnostics.json" ]]; then
    /usr/bin/python3 - "$DATA_DIR/diagnostics.json" <<'PY'
import json, sys
keys = ["state", "engine", "model", "microphone", "sample_rate", "hotkey", "hotkey_backend",
        "archive_enabled", "target_locked", "last_session_outcome", "last_session_audio_ms",
        "last_session_voiced_ms", "last_session_first_preview_ms", "last_session_committed",
        "login_item_status", "recovery_status", "last_error"]
data = json.load(open(sys.argv[1]))
for key in keys:
    if key in data:
        print(f"{key}={data[key]}")
PY
  fi
}

if [[ "${1:-}" == "--report" ]]; then
  report
  exit 0
fi

SECONDS_TO_RUN="${1:-30}"
[[ "$SECONDS_TO_RUN" =~ ^[0-9]+$ ]] || { echo "seconds must be an integer" >&2; exit 2; }

PID="$(pgrep -f "$MATCH" | head -n 1 || true)"
if [[ -z "$PID" ]]; then
  echo "Veyqa Voice Rust is not running (match: $MATCH)" >&2
  exit 1
fi

echo "timestamp,pid,rss_kb,cpu_percent"
END=$((SECONDS + SECONDS_TO_RUN))
while (( SECONDS < END )); do
  read -r RSS CPU <<<"$(ps -o rss= -o %cpu= -p "$PID" | awk 'NF')"
  [[ -n "${RSS:-}" ]] || break
  printf '%s,%s,%s,%s\n' "$(date +%s)" "$PID" "$RSS" "$CPU"
  sleep 1
done | tee /tmp/veyqa-rust-samples.csv >/dev/null

echo "== rss_kb =="
awk -F, 'NR>1 {print $3}' /tmp/veyqa-rust-samples.csv | sort -n | awk '
  {v[NR]=$1; sum+=$1}
  END {if (NR==0) exit; printf "median=%d p90=%d max=%d\n", v[int((NR+1)/2)], v[int(NR*0.9)], v[NR]}'
echo "== cpu_percent =="
awk -F, 'NR>1 {print $4}' /tmp/veyqa-rust-samples.csv | sort -n | awk '
  {v[NR]=$1; sum+=$1}
  END {if (NR==0) exit; printf "median=%.2f max=%.2f\n", v[int((NR+1)/2)], v[NR]}'
echo "samples: /tmp/veyqa-rust-samples.csv"
