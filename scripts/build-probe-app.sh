#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/dist/VeyqaProbe.app"
IDENTITY="Veyqa Local Development 2026"
BUNDLE_ID="com.wjcdev.veyqa.rust"

cd "$ROOT"
cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/veyqa-voice-rs "$APP/Contents/MacOS/veyqa-voice-rs"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh_CN</string>
    <key>CFBundleExecutable</key>
    <string>veyqa-voice-rs</string>
    <key>CFBundleIdentifier</key>
    <string>com.wjcdev.veyqa.rust</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Veyqa Rust Probe</string>
    <key>CFBundleDisplayName</key>
    <string>Veyqa Rust Probe</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Veyqa 需要使用麦克风把你的语音转成文字。</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Veyqa 需要与设置页和输入目标应用通信。</string>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

codesign --force --sign "$IDENTITY" --identifier "$BUNDLE_ID" --timestamp=none "$APP"
codesign --verify --strict "$APP"

echo "构建完成: $APP"
codesign -dv --verbose=2 "$APP" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|Signature" || true
