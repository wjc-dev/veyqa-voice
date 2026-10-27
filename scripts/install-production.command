#!/bin/zsh
# Replace the installed production app with the packaged Rust build, keeping
# the bundle id, data directory, signing identity, and therefore all TCC
# approvals. Run explicitly; it stops the running app and replaces
# /Applications/Veyqa Voice.app (a backup is kept).
#
#   ./scripts/install-production.command [path/to/new.app]
set -euo pipefail

PROJECT_DIR="${0:A:h:h}"
SOURCE="${1:-$PROJECT_DIR/dist-public/Veyqa Voice.app}"
TARGET="/Applications/Veyqa Voice.app"
SUPPORT_DIR="$HOME/Library/Application Support/Veyqa"
PAUSE_FILE="$SUPPORT_DIR/.supervisor-paused"
BACKUP_ROOT="$HOME/Library/Application Support/Veyqa Rebuild"
EXPECTED_REQUIREMENT='designated => identifier "com.wjcdev.veyqa" and certificate leaf = H"6cb7ed42af70dafacda95e4be77a095426b76b85"'

[[ -d "$SOURCE" ]] || { print -u2 "Missing source app: $SOURCE"; exit 1; }
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$SOURCE/Contents/Info.plist")" == "com.wjcdev.veyqa" ]] || {
  print -u2 "Source app is not the production bundle."
  exit 1
}
codesign --verify --deep --strict --verbose=2 "$SOURCE"
[[ "$(codesign -d -r- "$SOURCE" 2>&1 | awk '/^designated/ {print}')" == "$EXPECTED_REQUIREMENT" ]] || {
  print -u2 "Source app does not carry the fixed signing identity; TCC would be lost."
  exit 1
}
NEW_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$SOURCE/Contents/Info.plist")"
NEW_BUILD="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$SOURCE/Contents/Info.plist")"

# 1. Back up the installed app before touching it.
STAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP_ROOT"
if [[ -d "$TARGET" ]]; then
  OLD_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$TARGET/Contents/Info.plist" 2>/dev/null || print unknown)"
  OLD_BUILD="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$TARGET/Contents/Info.plist" 2>/dev/null || print unknown)"
  BACKUP="$BACKUP_ROOT/Veyqa Voice-$OLD_VERSION-$OLD_BUILD-$STAMP.app"
  ditto "$TARGET" "$BACKUP"
  print "已备份：$BACKUP"
fi

# 2. Pause the supervisor so it cannot relaunch the app during the swap,
#    then stop it and let the app quit gracefully.
mkdir -p "$SUPPORT_DIR"
: > "$PAUSE_FILE"
for label in com.wjcdev.veyqa.agent com.wjcdev.veyqa.local.agent; do
  launchctl bootout "gui/$UID/$label" 2>/dev/null || true
done
osascript -e 'tell application id "com.wjcdev.veyqa" to quit' 2>/dev/null || true
for _ in {1..50}; do
  pgrep -f -- "$TARGET/Contents/MacOS/" >/dev/null || break
  sleep 0.2
done
pkill -TERM -f -- "$TARGET/Contents/MacOS/" 2>/dev/null || true
sleep 1
pkill -KILL -f -- "$TARGET/Contents/MacOS/" 2>/dev/null || true

# 3. Copy beside the target, verify, then swap so a failed copy never leaves
#    a half-written bundle.
STAGING="/Applications/.Veyqa Voice.replacing.$$.app"
rm -rf "$STAGING"
ditto "$SOURCE" "$STAGING"
codesign --verify --deep --strict "$STAGING"
[[ "$(codesign -d -r- "$STAGING" 2>&1 | awk '/^designated/ {print}')" == "$EXPECTED_REQUIREMENT" ]] || {
  rm -rf "$STAGING"
  print -u2 "Staged copy lost the signing identity; nothing was replaced."
  exit 1
}
rm -rf "$TARGET"
mv "$STAGING" "$TARGET"
xattr -dr com.apple.quarantine "$TARGET" 2>/dev/null || true

# 4. Start the new build and confirm it stays up.
rm -f "$PAUSE_FILE"
open "$TARGET"
for _ in {1..50}; do
  if pgrep -f -- "$TARGET/Contents/MacOS/veyqa-voice-rs" >/dev/null; then
    print "已替换：$TARGET"
    print "版本：$NEW_VERSION ($NEW_BUILD)"
    print "旧版备份位于：$BACKUP_ROOT"
    exit 0
  fi
  sleep 0.2
done
print -u2 "新版已安装但未能确认启动；请从“应用程序”手动打开，或从备份恢复。"
exit 1
