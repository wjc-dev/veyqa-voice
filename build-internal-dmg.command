#!/bin/zsh
set -euo pipefail

PROJECT_DIR="${0:A:h}"
source "$PROJECT_DIR/packaging/signing.zsh"
veyqa_configure_signing
VERSION="0.2.12"
OUTPUT_DIR="${1:-$PROJECT_DIR/dist-$VEYQA_SIGNING_PROFILE}"
BUILD_NUMBER="${VEYQA_BUILD_NUMBER:-226}"
[[ "$BUILD_NUMBER" == <-> ]] || { print -u2 "Invalid build number"; exit 1; }
ARTIFACT_SUFFIX=""
[[ "$VEYQA_SIGNING_PROFILE" == "local" && "${VEYQA_PUBLIC_RELEASE:-false}" != "true" ]] && ARTIFACT_SUFFIX="-local"
if [[ -n "${VEYQA_PRIVATE_TRIAL_PAYLOAD:-}" ]]; then
  if [[ "${VEYQA_PUBLIC_RELEASE:-false}" == "true" && "${VEYQA_PUBLIC_PROMOTION:-false}" != "true" ]]; then
    print -u2 "Public credential distribution requires VEYQA_PUBLIC_PROMOTION=true."
    exit 1
  fi
  [[ -f "$VEYQA_PRIVATE_TRIAL_PAYLOAD" ]] || { print -u2 "Private trial payload missing"; exit 1; }
  [[ "$OUTPUT_DIR" == "$PROJECT_DIR"/outputs/* ]] || {
    print -u2 "Private trial artifacts must stay in the ignored outputs directory."
    exit 1
  }
  [[ "${VEYQA_PUBLIC_RELEASE:-false}" != "true" ]] && ARTIFACT_SUFFIX="-private-trial"
  # The artifact directory is private, but files inside the installable app
  # must be readable after a root-owned PKG install.
  umask 022
  mkdir -p "$OUTPUT_DIR"
  chmod 700 "$OUTPUT_DIR"
fi
if [[ "${VEYQA_PUBLIC_PROMOTION:-false}" == "true" && \
      ( "${VEYQA_PUBLIC_RELEASE:-false}" != "true" || -z "${VEYQA_PRIVATE_TRIAL_PAYLOAD:-}" ) ]]; then
  print -u2 "Public promotion requires both an explicit public release and a credential payload."
  exit 1
fi
if [[ "${VEYQA_PUBLIC_RELEASE:-false}" == "true" && "$VEYQA_SIGNING_IDENTITY" == "-" ]]; then
  print -u2 "Public releases require the fixed signing identity; refusing ad-hoc fallback."
  exit 1
fi
RUN_ID="$(date +%Y%m%d-%H%M%S)-$$"
WORK_DIR="$PROJECT_DIR/.package-build/$RUN_ID"
PYINSTALLER_WORK="$WORK_DIR/pyinstaller"
PYINSTALLER_DIST="$WORK_DIR/dist"
ICONSET="$WORK_DIR/AppIcon.iconset"
CLANG_CACHE="$WORK_DIR/clang-module-cache"
APP_PATH="$PYINSTALLER_DIST/Veyqa Voice.app"
DMG_PATH="$OUTPUT_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64${ARTIFACT_SUFFIX}.dmg"
ZIP_PATH="$OUTPUT_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64${ARTIFACT_SUFFIX}.zip"
PKG_PATH="$OUTPUT_DIR/Veyqa-Voice-v${VERSION}-macOS-arm64${ARTIFACT_SUFFIX}.pkg"

cleanup() {
  # Generated .app bundles must not remain under the source tree. LaunchServices
  # indexes nested app bundles and can later grant Accessibility to an obsolete
  # build that has the same display name as the installed app.
  if [[ "${VOICE_INPUT_KEEP_PACKAGE_BUILD:-false}" != "true" ]]; then
    rm -rf "$WORK_DIR"
  fi
}
trap cleanup EXIT

mkdir -p "$WORK_DIR" "$PYINSTALLER_WORK" "$PYINSTALLER_DIST" "$ICONSET" "$OUTPUT_DIR" "$CLANG_CACHE" "$PROJECT_DIR/build"
[[ -n "${VEYQA_PRIVATE_TRIAL_PAYLOAD:-}" ]] && chmod 700 "$WORK_DIR"
export CLANG_MODULE_CACHE_PATH="$CLANG_CACHE"
export SWIFT_MODULECACHE_PATH="$CLANG_CACHE"

if ! "$PROJECT_DIR/.venv/bin/python" -c 'import PyInstaller' >/dev/null 2>&1; then
  if command -v uv >/dev/null 2>&1; then
    uv pip install --python "$PROJECT_DIR/.venv/bin/python" 'pyinstaller>=6.10,<7'
  else
    "$PROJECT_DIR/.venv/bin/python" -m ensurepip --upgrade
    "$PROJECT_DIR/.venv/bin/python" -m pip install 'pyinstaller>=6.10,<7'
  fi
fi

xcrun swiftc -O -target arm64-apple-macos13.0 \
  -framework AppKit \
  "$PROJECT_DIR/native_settings/SupervisorHealth.swift" \
  "$PROJECT_DIR/native_settings/VeyqaSupervisor.swift" \
  -o "$PROJECT_DIR/build/VeyqaSupervisor"

if [[ ! -f "$PROJECT_DIR/packaging/AppIcon.icns" || \
      "$PROJECT_DIR/packaging/generate_app_icon.swift" -nt "$PROJECT_DIR/packaging/AppIcon.icns" ]]; then
  xcrun swift "$PROJECT_DIR/packaging/generate_app_icon.swift" "$WORK_DIR/AppIcon-1024.png"
  for SIZE in 16 32 128 256 512; do
    sips -z "$SIZE" "$SIZE" "$WORK_DIR/AppIcon-1024.png" \
      --out "$ICONSET/icon_${SIZE}x${SIZE}.png" >/dev/null
    DOUBLE=$((SIZE * 2))
    sips -z "$DOUBLE" "$DOUBLE" "$WORK_DIR/AppIcon-1024.png" \
      --out "$ICONSET/icon_${SIZE}x${SIZE}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$PROJECT_DIR/packaging/AppIcon.icns"
fi

zsh "$PROJECT_DIR/build-native-settings.command"

SPARKLE_DIR="$("$PROJECT_DIR/.venv/bin/python" "$PROJECT_DIR/packaging/prepare_sparkle.py")"
xcrun clang -dynamiclib -fobjc-arc -O2 -arch arm64 -mmacosx-version-min=13.0 \
  -framework AppKit -framework Sparkle -F "$SPARKLE_DIR" \
  -Wl,-rpath,@loader_path -install_name @rpath/VeyqaUpdates.dylib \
  "$PROJECT_DIR/native_settings/VeyqaUpdates.m" -o "$PROJECT_DIR/build/VeyqaUpdates.dylib"

"$PROJECT_DIR/.venv/bin/pyinstaller" \
  --noconfirm \
  --distpath "$PYINSTALLER_DIST" \
  --workpath "$PYINSTALLER_WORK" \
  "$PROJECT_DIR/packaging/VoiceInput.spec"

mkdir -p "$APP_PATH/Contents/Helpers" \
  "$APP_PATH/Contents/Frameworks" \
  "$APP_PATH/Contents/Library/LaunchAgents" \
  "$APP_PATH/Contents/Resources"
ditto "$PROJECT_DIR/build/VeyqaSettings.app" \
  "$APP_PATH/Contents/Helpers/VeyqaSettings.app"
cp "$PROJECT_DIR/packaging/com.wjcdev.veyqa.agent.plist" \
  "$APP_PATH/Contents/Library/LaunchAgents/com.wjcdev.veyqa.agent.plist"
if [[ "$VEYQA_SIGNING_PROFILE" == "local" ]]; then
  cp "$PROJECT_DIR/packaging/com.wjcdev.veyqa.agent.plist" \
    "$APP_PATH/Contents/Library/LaunchAgents/com.wjcdev.veyqa.local.agent.plist"
  /usr/libexec/PlistBuddy -c 'Set :Label com.wjcdev.veyqa.local.agent' \
    "$APP_PATH/Contents/Library/LaunchAgents/com.wjcdev.veyqa.local.agent.plist"
fi
cp "$PROJECT_DIR/build/VeyqaSupervisor" \
  "$APP_PATH/Contents/Resources/VeyqaSupervisor"
ditto "$SPARKLE_DIR/Sparkle.framework" "$APP_PATH/Contents/Frameworks/Sparkle.framework"
cp "$PROJECT_DIR/build/VeyqaUpdates.dylib" "$APP_PATH/Contents/Frameworks/VeyqaUpdates.dylib"
cp "$SPARKLE_DIR/LICENSE" "$APP_PATH/Contents/Resources/Sparkle-LICENSE"
"$PROJECT_DIR/.venv/bin/python" "$PROJECT_DIR/packaging/configure_updates.py" "$APP_PATH"

/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$APP_PATH/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :VeyqaSigningProfile string $VEYQA_SIGNING_PROFILE" "$APP_PATH/Contents/Info.plist"
if [[ -n "${VEYQA_PRIVATE_TRIAL_PAYLOAD:-}" ]]; then
  PROMOTION_ARGS=()
  [[ "${VEYQA_PUBLIC_PROMOTION:-false}" == "true" ]] && PROMOTION_ARGS=(--public-promotion)
  "$PROJECT_DIR/.venv/bin/python" "$PROJECT_DIR/packaging/configure_private_trial.py" \
    inject "$APP_PATH" "$VEYQA_PRIVATE_TRIAL_PAYLOAD" "${PROMOTION_ARGS[@]}"
fi
veyqa_sign_app "$APP_PATH" "$PROJECT_DIR/packaging/voice_input.entitlements"

/usr/bin/pkgbuild \
  --component "$APP_PATH" \
  --install-location "/Applications" \
  --identifier "com.wjcdev.veyqa" \
  --version "$VERSION" \
  "$PKG_PATH"
(cd "$OUTPUT_DIR" && shasum -a 256 "${PKG_PATH:t}") > "$PKG_PATH.sha256"
print "完成：$PKG_PATH"

# Always provide the .app as a Finder-safe ZIP. This is the simplest artifact
# to hand to a small trusted test group and preserves bundle metadata.
ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$ZIP_PATH"
(cd "$OUTPUT_DIR" && shasum -a 256 "${ZIP_PATH:t}") > "$ZIP_PATH.sha256"
print "完成：$ZIP_PATH"

if ! "$PROJECT_DIR/.venv/bin/python" -c 'import dmgbuild' >/dev/null 2>&1; then
  print -u2 "Install packaging dependencies: uv pip install --python .venv/bin/python -r requirements-dev.txt"
  exit 1
fi
xcrun swift "$PROJECT_DIR/packaging/generate_dmg_background.swift" "$WORK_DIR/dmg-background.png"
if "$PROJECT_DIR/.venv/bin/python" "$PROJECT_DIR/packaging/build_dmg.py" \
  "$APP_PATH" "$WORK_DIR/dmg-background.png" "$PROJECT_DIR/packaging/install-guide.html" "$DMG_PATH"; then
  (cd "$OUTPUT_DIR" && shasum -a 256 "${DMG_PATH:t}") > "$DMG_PATH.sha256"
  print "完成：$DMG_PATH"
  print "校验：$(cat "$DMG_PATH.sha256")"
else
  print -u2 "当前环境无法创建磁盘镜像；仍可使用已生成的 App ZIP 或 PKG。"
  exit 1
fi
