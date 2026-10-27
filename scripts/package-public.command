#!/bin/zsh
# Package a built public app into the release artifacts:
#   - Finder-safe ZIP (Sparkle update payload)
#   - drag-to-Applications DMG (website / Homebrew download)
#   - signed Sparkle appcast
#   - updated Homebrew cask copy
# It never uploads or deploys anything.
#
#   ./scripts/build-rust-macos.command            # VEYQA_DISTRIBUTION=public first
#   ./scripts/package-public.command             # -> dist-public + release/
set -euo pipefail

PROJECT_DIR="${0:A:h:h}"
cd "$PROJECT_DIR"

DIST_DIR="${1:-$PROJECT_DIR/dist-public}"
APP="$DIST_DIR/Veyqa Voice.app"
PACKAGING_ROOT="${VEYQA_PACKAGING_ROOT:-$HOME/Documents/voxtype}"
VENV_PY="$PACKAGING_ROOT/.venv/bin/python"
RELEASE_DIR="$PROJECT_DIR/release"
NOTES="$RELEASE_DIR/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null || true).md"

[[ -d "$APP" ]] || { print -u2 "Missing app: $APP (run VEYQA_DISTRIBUTION=public ./scripts/build-rust-macos.command)"; exit 1; }
[[ -x "$VENV_PY" ]] || { print -u2 "Missing packaging environment: $VENV_PY"; exit 1; }

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
BUILD_NUMBER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")"
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")"
[[ "$BUNDLE_ID" == "com.wjcdev.veyqa" ]] || { print -u2 "Not a production bundle id: $BUNDLE_ID"; exit 1; }
[[ -f "$NOTES" ]] || { print -u2 "Missing release notes: $NOTES"; exit 1; }

ZIP="$DIST_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64.zip"
DMG="$DIST_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64.dmg"
APPCAST="$PROJECT_DIR/website/appcast.xml"
CASK="$RELEASE_DIR/Casks/veyqa-voice.rb"
mkdir -p "$RELEASE_DIR/Casks" "$PROJECT_DIR/website"

# Refuse to package anything that does not verify exactly as it will install.
codesign --verify --deep --strict --verbose=2 "$APP"
[[ "$(codesign -d -r- "$APP" 2>&1 | awk '/^designated/ {print}')" == \
   'designated => identifier "com.wjcdev.veyqa" and certificate leaf = H"6cb7ed42af70dafacda95e4be77a095426b76b85"' ]] || {
  print -u2 "Unexpected designated requirement; TCC would be lost."
  exit 1
}

if [[ ! -f "$ZIP" ]]; then
  ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
fi
(cd "$DIST_DIR" && shasum -a 256 "${ZIP:t}") > "$ZIP.sha256"

xcrun swift "$PACKAGING_ROOT/packaging/generate_dmg_background.swift" "$DIST_DIR/.dmg-background.png"
"$VENV_PY" "$PACKAGING_ROOT/packaging/build_dmg.py" \
  "$APP" "$DIST_DIR/.dmg-background.png" "$PACKAGING_ROOT/packaging/install-guide.html" "$DMG"
rm -f "$DIST_DIR/.dmg-background.png"
(cd "$DIST_DIR" && shasum -a 256 "${DMG:t}") > "$DMG.sha256"

"$VENV_PY" "$PROJECT_DIR/scripts/generate-appcast.py" "$ZIP" "$APPCAST" --notes "$NOTES"

DMG_SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
cat > "$CASK" <<CASK
cask "veyqa-voice" do
  version "$VERSION"
  sha256 "$DMG_SHA"

  url "https://github.com/wjc-dev/veyqa-voice/releases/download/v#{version}/Veyqa-Voice-v#{version}-macOS-arm64.dmg"
  name "Veyqa Voice"
  desc "Push-to-talk voice input"
  homepage "https://wjc-dev.github.io/veyqa-voice/"

  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :ventura

  app "Veyqa Voice.app"

  uninstall quit: ["com.wjcdev.veyqa", "com.wjcdev.veyqa.settings"]
end
CASK

print "完成：$DMG"
print "完成：$ZIP"
print "完成：$APPCAST"
print "完成：$CASK"
print "版本：$VERSION ($BUILD_NUMBER)"
print "ZIP  ：$(cat "$ZIP.sha256")"
print "DMG  ：$(cat "$DMG.sha256")"
