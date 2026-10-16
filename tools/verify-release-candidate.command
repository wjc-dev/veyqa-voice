#!/bin/zsh
set -euo pipefail

# This workflow launches temporary, ad-hoc-signed executables and can trigger
# endpoint-security dialogs. Never silently do that on a user's work Mac.
if [[ "${VEYQA_ALLOW_GUI_SMOKE_TESTS:-false}" != "true" ]]; then
  print -u2 "Stopped: this verifier launches temporary native apps and test executables."
  print -u2 "On managed Macs, obtain an approved test environment first; do not bypass endpoint protection."
  print -u2 "Source-only checks: .venv/bin/python -m pytest test/ --ignore=test/test_native_settings_core.py"
  print -u2 "Only in an approved environment, explicitly opt in with VEYQA_ALLOW_GUI_SMOKE_TESTS=true."
  exit 2
fi

PROJECT_DIR="${0:A:h:h}"
source "$PROJECT_DIR/packaging/signing.zsh"
veyqa_configure_signing
if [[ "$VEYQA_SIGNING_PROFILE" != "internal" ]]; then
  print -u2 "Stopped: local-signed builds must be tested at the fixed installation path, not as temporary duplicate Apps."
  print -u2 "This legacy internal verifier requires VEYQA_SIGNING_IDENTITY=- in an approved test environment."
  exit 2
fi
VERSION="0.2.5"
OUTPUT_DIR="${1:-$PROJECT_DIR/dist-internal}"
ZIP_PATH="$OUTPUT_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64.zip"
PKG_PATH="$OUTPUT_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64.pkg"
DMG_PATH="$OUTPUT_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64.dmg"
VERIFY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/veyqa-verify.XXXXXX")"
EXTRACT_DIR="$VERIFY_DIR/extracted"
DATA_DIR="$VERIFY_DIR/data"
APP_PATH="$EXTRACT_DIR/Veyqa Voice.app"
APP_PID=""
ACTIVE_SETTINGS_PID=""
SETTINGS_EXECUTABLE=""
VERIFY_PASSED=false

terminate_test_pid() {
  local target_pid="${1:-}"
  if [[ -z "$target_pid" || "$target_pid" != <-> || "$target_pid" -le 1 ]]; then
    return
  fi

  kill "$target_pid" >/dev/null 2>&1 || true
  for _attempt in {1..20}; do
    if ! kill -0 "$target_pid" >/dev/null 2>&1; then
      wait "$target_pid" >/dev/null 2>&1 || true
      return
    fi
    sleep 0.1
  done
  kill -KILL "$target_pid" >/dev/null 2>&1 || true
  wait "$target_pid" >/dev/null 2>&1 || true
}

stop_test_processes() {
  # Stop the owner first. Otherwise the menu-bar health check can recreate the
  # settings helper between helper cleanup and main-process cleanup.
  terminate_test_pid "$APP_PID"
  APP_PID=""
  terminate_test_pid "$ACTIVE_SETTINGS_PID"
  ACTIVE_SETTINGS_PID=""

  local settings_pid=""
  if [[ -f "$DATA_DIR/.settings-instance" ]]; then
    settings_pid="$(<"$DATA_DIR/.settings-instance")"
    terminate_test_pid "$settings_pid"
  fi

  if [[ -n "$SETTINGS_EXECUTABLE" ]]; then
    local helper_pid=""
    while IFS= read -r helper_pid; do
      terminate_test_pid "$helper_pid"
    done < <(pgrep -f -x "$SETTINGS_EXECUTABLE" 2>/dev/null || true)
  fi
}

cleanup() {
  stop_test_processes
  if [[ "$VERIFY_PASSED" == "true" ]]; then
    rm -rf "$VERIFY_DIR"
  else
    print -u2 "Verification evidence preserved at: $VERIFY_DIR"
  fi
}
trap cleanup EXIT INT TERM

dump_smoke_logs() {
  local log_file
  for log_file in "$VERIFY_DIR/app.log" "$DATA_DIR/logs/app.log" \
      "$DATA_DIR/logs/settings_ui.log"; do
    if [[ -f "$log_file" ]]; then
      print -u2 "--- $log_file"
      sed -n '1,200p' "$log_file" >&2 || true
    fi
  done
}

cd "$PROJECT_DIR"
print "[1/7] Running unit tests"
env -u VOICE_INPUT_DISABLE_LOGIN_SYNC "$PROJECT_DIR/.venv/bin/python" -m pytest test/

print "[2/7] Building isolated internal artifacts"
zsh "$PROJECT_DIR/build-internal-dmg.command" "$OUTPUT_DIR"

print "[3/7] Verifying bundle structure, version, and signature"
mkdir -p "$EXTRACT_DIR" "$DATA_DIR"
ditto -x -k "$ZIP_PATH" "$EXTRACT_DIR"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$APP_PATH"
/usr/bin/plutil -lint \
  "$APP_PATH/Contents/Library/LaunchAgents/com.wjcdev.veyqa.agent.plist"
SETTINGS_APP="$APP_PATH/Contents/Helpers/VeyqaSettings.app"
SETTINGS_EXECUTABLE="$SETTINGS_APP/Contents/MacOS/VeyqaSettings"
/usr/bin/plutil -lint "$SETTINGS_APP/Contents/Info.plist"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$SETTINGS_APP"
bundle_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
  "$APP_PATH/Contents/Info.plist")"
[[ "$bundle_version" == "$VERSION" ]] || {
  print -u2 "Bundle version mismatch: expected $VERSION, got $bundle_version"
  exit 1
}
file "$APP_PATH/Contents/MacOS/VeyqaVoice" | grep -q "arm64"
file "$SETTINGS_EXECUTABLE" | grep -q "arm64"

print "[4/7] Stressing the bundled settings helper startup five times"
for cycle in {1..5}; do
  cycle_dir="$VERIFY_DIR/settings-cycle-$cycle"
  mkdir -p "$cycle_dir"
  VOICE_INPUT_ROOT="$APP_PATH/Contents/Frameworks" \
  VOICE_INPUT_DATA_ROOT="$cycle_dir" \
  VOICE_INPUT_BUNDLED=true \
  VOICE_INPUT_APP_PATH="$APP_PATH" \
    "$SETTINGS_EXECUTABLE" >"$cycle_dir/helper.log" 2>&1 &
  settings_cycle_pid=$!
  ACTIVE_SETTINGS_PID="$settings_cycle_pid"
  cycle_ready=false
  # A newly extracted ad-hoc bundle can spend several seconds in dyld/Gatekeeper
  # on its first ever path. Production notarization should be faster, but the
  # internal verifier allows 20 seconds and still requires the AppDelegate lock.
  for _attempt in {1..200}; do
    if ! kill -0 "$settings_cycle_pid" >/dev/null 2>&1; then
      print -u2 "Settings helper exited during startup cycle $cycle"
      sed -n '1,160p' "$cycle_dir/helper.log" >&2 || true
      exit 1
    fi
    if [[ -f "$cycle_dir/.settings-instance" ]] \
        && [[ "$(<"$cycle_dir/.settings-instance")" == "$settings_cycle_pid" ]]; then
      cycle_ready=true
      break
    fi
    sleep 0.1
  done
  if [[ "$cycle_ready" != "true" ]]; then
    print -u2 "Settings helper handshake timed out in cycle $cycle"
    exit 1
  fi
  kill "$settings_cycle_pid" >/dev/null 2>&1 || true
  wait "$settings_cycle_pid" >/dev/null 2>&1 || true
  ACTIVE_SETTINGS_PID=""
done

print "[5/7] Triggering two registered hotkeys in the frozen app"
run_hotkey_cycle() {
  local name="$1"
  local spec="$2"
  local label="$3"
  local keycode="$4"
  local flags="$5"
  local cycle_data="$VERIFY_DIR/hotkey-$name"
  local cycle_log="$cycle_data/logs/app.log"
  mkdir -p "$cycle_data"
  print -r -- \
    "TRANSCRIPTION_SERVICE=qwen
QWEN_API_KEY=
VOICE_HOTKEY=$spec
VOICE_HOTKEY_LABEL=$label
FN_HOTKEY_MODE=hold
GLOBAL_HOTKEY_BACKEND=registered
LAUNCH_AT_LOGIN=false
AUDIO_ARCHIVE_ENABLED=false" > "$cycle_data/.env"
  chmod 600 "$cycle_data/.env"

  local registered=false
  for startup_attempt in {1..3}; do
    : > "$cycle_data/console.log"
    rm -f "$cycle_log"
    VOICE_INPUT_DATA_DIR="$cycle_data" \
    VOICE_INPUT_DISABLE_LOGIN_SYNC=true \
      "$APP_PATH/Contents/MacOS/VeyqaVoice" --background-login \
        >"$cycle_data/console.log" 2>&1 &
    APP_PID=$!
    for _attempt in {1..60}; do
      if ! kill -0 "$APP_PID" >/dev/null 2>&1; then
        wait "$APP_PID" >/dev/null 2>&1 || true
        APP_PID=""
        break
      fi
      if [[ -f "$cycle_log" ]] \
          && grep -q "系统注册型录音快捷键已启用" "$cycle_log"; then
        registered=true
        break
      fi
      sleep 0.5
    done
    [[ "$registered" == "true" ]] && break
    terminate_test_pid "$APP_PID"
    APP_PID=""
    print -u2 "Retrying cold start for hotkey $label ($startup_attempt/3)"
  done
  [[ "$registered" == "true" ]] || {
    print -u2 "Veyqa Voice could not register hotkey $label after three cold starts"
    sed -n '1,160p' "$cycle_data/console.log" >&2 || true
    exit 1
  }

  "$PROJECT_DIR/.venv/bin/python" "$PROJECT_DIR/tools/post-hotkey-event.py" \
    "$keycode" "$flags"
  local triggered=false
  for _attempt in {1..40}; do
    if grep -q "开始录音" "$cycle_log" \
        && grep -q "停止录音" "$cycle_log"; then
      triggered=true
      break
    fi
    sleep 0.25
  done
  [[ "$triggered" == "true" ]] || {
    print -u2 "Hotkey $label did not produce press and release callbacks"
    sed -n '1,180p' "$cycle_log" >&2 || true
    exit 1
  }

  # With intentionally blank credentials, the first hotkey opens Settings.
  # Wait for its native handshake before terminating the owner; otherwise a
  # cold helper can finish launching only after the verifier has cleaned up.
  local cycle_settings_pid=""
  local settings_ready=false
  for _attempt in {1..200}; do
    if [[ -f "$cycle_data/.settings-instance" ]]; then
      cycle_settings_pid="$(<"$cycle_data/.settings-instance")"
      if [[ "$cycle_settings_pid" == <-> ]] \
          && kill -0 "$cycle_settings_pid" >/dev/null 2>&1; then
        settings_ready=true
        break
      fi
    fi
    sleep 0.1
  done
  [[ "$settings_ready" == "true" ]] || {
    print -u2 "Hotkey cycle settings helper did not finish startup for $label"
    exit 1
  }

  terminate_test_pid "$APP_PID"
  APP_PID=""
  terminate_test_pid "$cycle_settings_pid"
}

# CGEvent flag masks: Control=1<<18, Option=1<<19, Shift=1<<17.
run_hotkey_cycle "control-option-space" \
  "keycode:49;mods:control+option" "⌃⌥Space" 49 786432
run_hotkey_cycle "control-shift-k" \
  "keycode:40;mods:control+shift" "⌃⇧K" 40 393216

print "[6/7] Running Dock/settings smoke test without login-item mutation"
VOICE_INPUT_DATA_DIR="$DATA_DIR" \
VOICE_INPUT_DISABLE_LOGIN_SYNC=true \
DOCK_ENTRY_SELF_TEST=true \
  "$APP_PATH/Contents/MacOS/VeyqaVoice" >"$VERIFY_DIR/app.log" 2>&1 &
APP_PID=$!

status_log="$DATA_DIR/logs/settings_ui.log"
runtime_log="$DATA_DIR/logs/app.log"
ready=false
for _attempt in {1..60}; do
  if ! kill -0 "$APP_PID" >/dev/null 2>&1; then
    print -u2 "Veyqa Voice exited during smoke test"
    dump_smoke_logs
    exit 1
  fi
  if [[ -f "$status_log" ]] \
      && grep -q "dock entry ready; menu bar disabled" "$status_log" \
      && grep -q "settings process started" "$status_log" \
      && grep -Eq "settings process warming up|reused settings process" "$status_log" \
      && [[ -f "$runtime_log" ]] \
      && grep -q "系统注册型录音快捷键已启用" "$runtime_log"; then
    ready=true
    break
  fi
  sleep 0.5
done
[[ "$ready" == "true" ]] || {
  print -u2 "Timed out waiting for Dock/settings smoke evidence"
  dump_smoke_logs
  exit 1
}
if grep -Eq \
    "status=-30|Traceback|CRITICAL|系统注册型快捷键不可用|全局快捷键不可用" \
    "$status_log" "$runtime_log" "$VERIFY_DIR/app.log"; then
  print -u2 "Fatal signature found in smoke logs"
  exit 1
fi

stop_test_processes
if [[ -n "$SETTINGS_EXECUTABLE" ]] \
    && pgrep -f -x "$SETTINGS_EXECUTABLE" >/dev/null 2>&1; then
  print -u2 "Temporary settings helper remained after cleanup"
  exit 1
fi

print "[7/8] Verifying automatic recovery from a stalled recording start"
recovery_data="$VERIFY_DIR/automatic-recovery"
mkdir -p "$recovery_data"
print -r -- 'QWEN_API_KEY=
GLOBAL_HOTKEY_BACKEND=off
LAUNCH_AT_LOGIN=false' > "$recovery_data/.env"
VOICE_INPUT_DATA_DIR="$recovery_data" \
VOICE_INPUT_DISABLE_LOGIN_SYNC=true \
VEYQA_HEALTH_SELF_TEST=true \
  "$APP_PATH/Contents/MacOS/VeyqaVoice" --background-login >"$recovery_data/console.log" 2>&1 &
APP_PID=$!
recovered=false
for _attempt in {1..90}; do
  if [[ -f "$recovery_data/logs/app.log" ]] \
      && [[ "$(grep -c '语音输入已启动' "$recovery_data/logs/app.log")" -ge 2 ]]; then
    recovered=true
    break
  fi
  sleep 0.5
done
[[ "$recovered" == "true" ]] || {
  print -u2 "Automatic recovery did not restart the stalled runtime"
  exit 1
}
kill -0 "$APP_PID"
"$PROJECT_DIR/.venv/bin/python" -c 'import json,sys; from pathlib import Path; p=Path(sys.argv[1]); assert len(json.loads((p/"automatic-restarts.json").read_text())) == 1; assert "QWEN_API_KEY=\n" in (p/".env").read_text(); print("automatic_recovery=passed; configuration_preserved=yes")' "$recovery_data"
stop_test_processes

print "[8/8] Artifact SHA-256"
for artifact in "$PKG_PATH" "$ZIP_PATH" "$DMG_PATH"; do
  if [[ -f "$artifact" ]]; then
    shasum -a 256 "$artifact"
  fi
done
print "Release-candidate verification passed without installing or registering a login item."
VERIFY_PASSED=true
