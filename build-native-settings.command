#!/bin/zsh
set -euo pipefail

PROJECT_DIR="${0:A:h}"
cd "$PROJECT_DIR"
mkdir -p build/swift-module-cache build/clang-module-cache
SETTINGS_APP="$PROJECT_DIR/build/VeyqaSettings.app"
SETTINGS_EXECUTABLE="$SETTINGS_APP/Contents/MacOS/VeyqaSettings"

if ! xcrun --find swiftc >/dev/null 2>&1; then
  print -u2 "缺少 Swift 编译器，请先运行：xcode-select --install"
  exit 1
fi

mkdir -p "$SETTINGS_APP/Contents/MacOS"
cp "$PROJECT_DIR/native_settings/VoiceInputSettings-Info.plist" \
  "$SETTINGS_APP/Contents/Info.plist"

CLANG_MODULE_CACHE_PATH="$PROJECT_DIR/build/clang-module-cache" \
SWIFT_MODULECACHE_PATH="$PROJECT_DIR/build/swift-module-cache" \
xcrun swiftc -parse-as-library -target arm64-apple-macos13.0 \
  -framework SwiftUI -framework AppKit \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker "$PROJECT_DIR/native_settings/VoiceInputSettings-Info.plist" \
  native_settings/SettingsCore.swift \
  native_settings/VoiceInputSettings.swift \
  -o "$SETTINGS_EXECUTABLE"

/usr/bin/codesign --force --sign - --timestamp=none "$SETTINGS_APP"
/usr/bin/codesign --verify --deep --strict "$SETTINGS_APP"

print "已生成：$SETTINGS_APP"
