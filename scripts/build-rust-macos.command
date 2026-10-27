#!/bin/zsh
# Assemble the Rust app: cargo release binary + vendored Swift supervisor/helper
# + reused Sparkle framework, signed inside-out with the fixed local identity so
# TCC approvals survive rebuilds.
#
#   ./scripts/build-rust-macos.command                 # parallel dev install
#   VEYQA_DISTRIBUTION=public ./scripts/build-rust-macos.command
#
# The public build uses the production bundle id, data directory, and app name,
# so it can replace the installed Python app without losing settings or TCC.
set -euo pipefail

PROJECT_DIR="${0:A:h:h}"
cd "$PROJECT_DIR"

DISTRIBUTION="${VEYQA_DISTRIBUTION:-dev}"
OUTPUT_DIR="${1:-}"
SWIFT_DEFINES=()
SUPERVISOR_LABEL="com.wjcdev.veyqa.rust.agent"
LOCAL_SUPERVISOR_LABEL="com.wjcdev.veyqa.rust.local.agent"
SETTINGS_BUNDLE_ID="com.wjcdev.veyqa.rust.settings"
SPARKLE_KEYS=""

case "$DISTRIBUTION" in
  public)
    BUNDLE_ID="com.wjcdev.veyqa"
    APP_NAME="Veyqa Voice"
    SETTINGS_BUNDLE_ID="com.wjcdev.veyqa.settings"
    SUPERVISOR_LABEL="com.wjcdev.veyqa.agent"
    LOCAL_SUPERVISOR_LABEL="com.wjcdev.veyqa.local.agent"
    SWIFT_DEFINES=(-D VEYQA_PUBLIC)
    VERSION="${VEYQA_VERSION:-0.3.0}"
    BUILD_NUMBER="${VEYQA_BUILD_NUMBER:-300}"
    : "${OUTPUT_DIR:=$PROJECT_DIR/dist-public}"
    SPARKLE_KEYS='    <key>SUAllowsAutomaticUpdates</key>
    <false/>
    <key>SUAutomaticallyUpdate</key>
    <false/>
    <key>SUEnableAutomaticChecks</key>
    <true/>
    <key>SUEnableSystemProfiling</key>
    <false/>
    <key>SUFeedURL</key>
    <string>https://raw.githubusercontent.com/wjc-dev/veyqa-voice/main/website/appcast.xml</string>
    <key>SUPublicEDKey</key>
    <string>0LsMD9edTYW+cc1emzlg6LB3y8O6yNDE0bDnVR651Ys=</string>
    <key>SURequireSignedFeed</key>
    <true/>
    <key>SUScheduledCheckInterval</key>
    <integer>86400</integer>
    <key>SUSignedFeedFailureExpirationInterval</key>
    <integer>0</integer>
    <key>SUVerifyUpdateBeforeExtraction</key>
    <true/>'
    ;;
  dev)
    BUNDLE_ID="com.wjcdev.veyqa.rust"
    APP_NAME="Veyqa Voice Rust"
    VERSION="${VEYQA_VERSION:-$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)}"
    BUILD_NUMBER="${VEYQA_BUILD_NUMBER:-1}"
    : "${OUTPUT_DIR:=$PROJECT_DIR/dist}"
    ;;
  *)
    print -u2 "VEYQA_DISTRIBUTION must be 'dev' or 'public'."
    exit 1
    ;;
esac

if [[ "$DISTRIBUTION" == "public" ]]; then
  DISPLAY_NAME="Veyqa Voice"
  DISTRIBUTION_NAME="public-promotion"
  MICROPHONE_TEXT="Veyqa Voice 需要麦克风权限，才能把你的语音实时转成文字。"
  APPLE_EVENTS_TEXT="Veyqa Voice 需要辅助功能权限，才能在当前光标处实时写入转写文字。"
else
  DISPLAY_NAME="Veyqa Voice Rust"
  DISTRIBUTION_NAME="private-trial"
  MICROPHONE_TEXT="Veyqa Voice Rust 需要麦克风权限，才能把你的语音实时转成文字。"
  APPLE_EVENTS_TEXT="Veyqa Voice Rust 需要辅助功能权限，才能在当前光标处实时写入转写文字。"
fi

[[ "$BUILD_NUMBER" == <-> ]] || { print -u2 "Invalid build number"; exit 1; }
APP="$OUTPUT_DIR/$APP_NAME.app"
HELPER="$APP/Contents/Helpers/VeyqaSettings.app"
PRODUCTION_APP="/Applications/Veyqa Voice.app"
WORK_DIR="$PROJECT_DIR/.package-build/rust-$(date +%Y%m%d-%H%M%S)-$$"
CLANG_CACHE="$WORK_DIR/clang-module-cache"

if (( ! ${+VEYQA_SIGNING_IDENTITY} )); then
  config="$HOME/Library/Application Support/Veyqa Build/signing-identity.sha1"
  if [[ -f "$config" ]]; then
    VEYQA_SIGNING_IDENTITY="$(<"$config")"
  else
    VEYQA_SIGNING_IDENTITY="6CB7ED42AF70DAFACDA95E4BE77A095426B76B85"
  fi
fi
VEYQA_SIGNING_IDENTITY="${(U)VEYQA_SIGNING_IDENTITY}"
if [[ ! "$VEYQA_SIGNING_IDENTITY" =~ '^[[:xdigit:]]{40}$' ]]; then
  print -u2 "Signing identity must be an exact certificate SHA-1 fingerprint."
  exit 1
fi
if ! security find-identity -v -p codesigning | awk -v wanted="$VEYQA_SIGNING_IDENTITY" \
    '$2 == wanted { found=1 } END { exit !found }'; then
  print -u2 "Configured signing identity is unavailable. Refusing ad-hoc fallback."
  exit 1
fi

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT
mkdir -p "$WORK_DIR" "$CLANG_CACHE" "$OUTPUT_DIR"
export CLANG_MODULE_CACHE_PATH="$CLANG_CACHE"
export SWIFT_MODULECACHE_PATH="$CLANG_CACHE"
export VEYQA_DISTRIBUTION="$DISTRIBUTION"
export VEYQA_BUILD_VERSION="$VERSION"

cargo build --release

# `option_env!` bakes the distribution into the binary. Cargo normally tracks
# it, but a stale artifact here would silently install the parallel dev app
# under the production identity, so verify before assembling the bundle.
if [[ "$DISTRIBUTION" == "public" ]]; then
  strings target/release/veyqa-voice-rs > "$WORK_DIR/binary-strings.txt"
  if grep -q 'Veyqa-Rust-Dev' "$WORK_DIR/binary-strings.txt"; then
    print -u2 "Refusing to package a dev binary as the public build; run 'cargo clean -p veyqa-voice-rs' and retry."
    exit 1
  fi
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" \
  "$APP/Contents/Resources" \
  "$APP/Contents/Helpers" \
  "$APP/Contents/Frameworks" \
  "$APP/Contents/Library/LaunchAgents" \
  "$HELPER/Contents/MacOS" \
  "$HELPER/Contents/Resources"

cp target/release/veyqa-voice-rs "$APP/Contents/MacOS/veyqa-voice-rs"
cp config/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp config/StatusBarIcon.png "$APP/Contents/Resources/StatusBarIcon.png"
cp config/env.example "$APP/Contents/Resources/env.example"
cp config/asr-models.json "$APP/Contents/Resources/asr-models.json"
cp config/release-features.json "$APP/Contents/Resources/release-features.json"
# The sponsored credential is never committed; it is taken from the installed
# production bundle at package time, exactly like the Swift assets. A public
# build without it would silently lose the built-in service, so refuse.
if [[ -f "$PRODUCTION_APP/Contents/Resources/private-trial.dat" ]]; then
  cp "$PRODUCTION_APP/Contents/Resources/private-trial.dat" \
    "$APP/Contents/Resources/private-trial.dat"
elif [[ "$DISTRIBUTION" == "public" ]]; then
  print -u2 "Missing $PRODUCTION_APP/Contents/Resources/private-trial.dat; cannot build the public app."
  exit 1
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh_CN</string>
    <key>CFBundleLocalizations</key>
    <array>
        <string>zh_CN</string>
        <string>en</string>
    </array>
    <key>CFBundleDisplayName</key>
    <string>$DISPLAY_NAME</string>
    <key>CFBundleName</key>
    <string>$DISPLAY_NAME</string>
    <key>CFBundleExecutable</key>
    <string>veyqa-voice-rs</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon.icns</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$BUILD_NUMBER</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>$MICROPHONE_TEXT</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>$APPLE_EVENTS_TEXT</string>
    <key>NSHumanReadableCopyright</key>
    <string>Veyqa Voice contributors</string>
$SPARKLE_KEYS
    <key>VeyqaSigningProfile</key>
    <string>local</string>
    <key>VeyqaDistribution</key>
    <string>$DISTRIBUTION_NAME</string>
</dict>
</plist>
PLIST
printf 'APPL????' > "$APP/Contents/PkgInfo"

xcrun swiftc -O -target arm64-apple-macos13.0 \
  "${SWIFT_DEFINES[@]}" \
  -framework AppKit \
  native/SupervisorHealth.swift \
  native/VeyqaSupervisor.swift \
  -o "$WORK_DIR/VeyqaSupervisor"
cp "$WORK_DIR/VeyqaSupervisor" "$APP/Contents/Resources/VeyqaSupervisor"

cp native/settings/VoiceInputSettings-Info.plist "$HELPER/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $SETTINGS_BUNDLE_ID" "$HELPER/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$HELPER/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$HELPER/Contents/Info.plist"
cp config/AppIcon.icns "$HELPER/Contents/Resources/AppIcon.icns"
cp config/asr-models.json "$HELPER/Contents/Resources/asr-models.json"
cp config/release-features.json "$HELPER/Contents/Resources/release-features.json"
xcrun swiftc -O -parse-as-library -target arm64-apple-macos13.0 \
  "${SWIFT_DEFINES[@]}" \
  -framework SwiftUI -framework AppKit \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker "$HELPER/Contents/Info.plist" \
  native/settings/SettingsCore.swift \
  native/settings/SpeechModels.swift \
  native/settings/SettingsControls.swift \
  native/settings/VoiceInputSettings.swift \
  -o "$HELPER/Contents/MacOS/VeyqaSettings"

write_agent() {
  local label="$1" target="$2"
  cat > "$target" <<AGENT
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$label</string>
    <key>BundleProgram</key>
    <string>Contents/Resources/VeyqaSupervisor</string>
    <key>AssociatedBundleIdentifiers</key>
    <array>
        <string>$BUNDLE_ID</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
AGENT
}
write_agent "$SUPERVISOR_LABEL" \
  "$APP/Contents/Library/LaunchAgents/$SUPERVISOR_LABEL.plist"
write_agent "$LOCAL_SUPERVISOR_LABEL" \
  "$APP/Contents/Library/LaunchAgents/$LOCAL_SUPERVISOR_LABEL.plist"

# Sparkle and the update shim are reused from the installed production build;
# they are generic binaries, and missing them only disables update checks.
if [[ -d "$PRODUCTION_APP/Contents/Frameworks/Sparkle.framework" &&
      -f "$PRODUCTION_APP/Contents/Frameworks/VeyqaUpdates.dylib" ]]; then
  ditto "$PRODUCTION_APP/Contents/Frameworks/Sparkle.framework" \
    "$APP/Contents/Frameworks/Sparkle.framework"
  cp "$PRODUCTION_APP/Contents/Frameworks/VeyqaUpdates.dylib" \
    "$APP/Contents/Frameworks/VeyqaUpdates.dylib"
  if [[ -f "$PRODUCTION_APP/Contents/Resources/Sparkle-LICENSE" ]]; then
    cp "$PRODUCTION_APP/Contents/Resources/Sparkle-LICENSE" \
      "$APP/Contents/Resources/Sparkle-LICENSE"
  fi
else
  print -u2 "Sparkle framework not found in $PRODUCTION_APP; update checks stay disabled."
fi

sign() {
  codesign --force --sign "$VEYQA_SIGNING_IDENTITY" --timestamp=none "$@"
}

# Sign actual Mach-O code only, then seal nested bundles from inside out.
# No --deep signing, no trust overrides and no weakening of system policies.
while IFS= read -r -d '' item; do
  if [[ "$(/usr/bin/file -b "$item")" == *Mach-O* ]]; then
    sign --preserve-metadata=entitlements "$item"
  fi
done < <(find "$APP/Contents" -type f -print0)
while IFS= read -r -d '' item; do
  sign --preserve-metadata=entitlements "$item"
done < <(find "$APP/Contents" -depth -type d \( -name '*.framework' -o -name '*.app' -o -name '*.xpc' \) -print0)
sign --entitlements native/voice_input.entitlements "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

if [[ "$DISTRIBUTION" == "public" ]]; then
  ZIP_NAME="Veyqa-Voice-v${VERSION}-macOS-arm64.zip"
else
  ZIP_NAME="Veyqa-Voice-Rust-v${VERSION}-macOS-arm64.zip"
fi
ZIP_PATH="$OUTPUT_DIR/$ZIP_NAME"
rm -f "$ZIP_PATH"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP_PATH"
(cd "$OUTPUT_DIR" && shasum -a 256 "${ZIP_PATH:t}") > "$ZIP_PATH.sha256"

print "完成：$APP"
print "完成：$ZIP_PATH"
