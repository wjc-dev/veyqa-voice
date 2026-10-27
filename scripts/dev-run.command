#!/bin/bash
# Launch the dev build for the parallel install.
#
#   scripts/dev-run.command [--headless]
#
# Reads the production `.env` read-only, keeps its own data directory, and
# forces a different push-to-talk gesture (right Command, hold) so this build
# never fires together with the installed Veyqa Voice.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/dist/Veyqa Voice Rust.app"
PROD_ENV="${VOICE_INPUT_DATA_DIR_PRODUCTION:-$HOME/Library/Application Support/Veyqa}/.env"
DATA_DIR="${VOICE_INPUT_DATA_DIR:-$HOME/Library/Application Support/Veyqa-Rust-Dev}"

if [[ ! -d "$APP" ]]; then
  echo "missing $APP - run scripts/build-rust-macos.command first" >&2
  exit 1
fi
if [[ ! -f "$PROD_ENV" ]]; then
  echo "missing production config $PROD_ENV" >&2
  exit 1
fi
if pgrep -f "$APP/Contents/MacOS/veyqa-voice-rs" >/dev/null; then
  echo "already running; quit it from the menu bar first (open would reuse the old build)" >&2
  exit 1
fi
if pgrep -f "/Applications/Veyqa Voice.app/Contents/MacOS/VeyqaVoice" >/dev/null; then
  echo "production Veyqa Voice is running; both builds would share one hotkey" >&2
  echo "quit production first, or put right_command in $DATA_DIR/.dev-hotkey" >&2
  exit 1
fi
mkdir -p "$DATA_DIR/logs"
HOTKEY="$(tr -d '[:space:]' < "$DATA_DIR/.dev-hotkey" 2>/dev/null || true)"

ARGS=(--background-login)
if [[ "${1:-}" == "--headless" ]]; then
  ARGS+=("--headless")
fi
ARGS+=(--log "$DATA_DIR/logs/dev-stdout.log")

open -g "$APP" \
  --env "VEYQA_CONFIG_FILE=$PROD_ENV" \
  --env "VOICE_INPUT_DATA_DIR=$DATA_DIR" \
  --env "RUST_BACKTRACE=1" \
  --args "${ARGS[@]}"

echo "launched: $APP"
echo "config:   $PROD_ENV (read-only)"
echo "data:     $DATA_DIR"
echo "hotkey:   ${HOTKEY:-from .env} (file $DATA_DIR/.dev-hotkey)"
