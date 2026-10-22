#!/bin/zsh
set -euo pipefail

PROJECT_DIR="${0:A:h:h}"
source "$PROJECT_DIR/packaging/signing.zsh"
veyqa_configure_signing
cd "$PROJECT_DIR"
mkdir -p build/swift-module-cache build/clang-module-cache
SETTINGS_APP="$PROJECT_DIR/build/VeyqaSettings.app"
SETTINGS_EXECUTABLE="$SETTINGS_APP/Contents/MacOS/VeyqaSettings"

if ! xcrun --find swiftc >/dev/null 2>&1; then
  print -u2 "缺少 Swift 编译器，请先运行：xcode-select --install"
  exit 1
fi

mkdir -p "$SETTINGS_APP/Contents/MacOS"
mkdir -p "$SETTINGS_APP/Contents/Resources"
cp "$PROJECT_DIR/packaging/AppIcon.icns" "$SETTINGS_APP/Contents/Resources/AppIcon.icns"
cp "$PROJECT_DIR/config/asr-models.json" "$SETTINGS_APP/Contents/Resources/asr-models.json"
cp "$PROJECT_DIR/config/release-features.json" "$SETTINGS_APP/Contents/Resources/release-features.json"
cp "$PROJECT_DIR/native_settings/VoiceInputSettings-Info.plist" \
  "$SETTINGS_APP/Contents/Info.plist"

CLANG_MODULE_CACHE_PATH="$PROJECT_DIR/build/clang-module-cache" \
SWIFT_MODULECACHE_PATH="$PROJECT_DIR/build/swift-module-cache" \
xcrun swiftc -O -parse-as-library -target arm64-apple-macos13.0 \
  -framework SwiftUI -framework AppKit \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker "$PROJECT_DIR/native_settings/VoiceInputSettings-Info.plist" \
  native_settings/SettingsCore.swift \
  native_settings/SpeechModels.swift \
  native_settings/SettingsControls.swift \
  native_settings/VoiceInputSettings.swift \
  -o "$SETTINGS_EXECUTABLE"

veyqa_sign "$SETTINGS_APP"
/usr/bin/codesign --verify --deep --strict "$SETTINGS_APP"

print "已生成：$SETTINGS_APP"
