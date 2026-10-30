#!/bin/bash
# Install or upgrade Veyqa Voice without the Gatekeeper "cannot verify" prompt.
#
#   curl -fsSL https://wjc-dev.github.io/veyqa-voice/install.sh | bash
#
# The package is fetched with curl instead of a browser, so macOS never attaches
# the com.apple.quarantine attribute that triggers the warning on first launch.
#
#   VEYQA_INSTALL_DIR=~/Applications  install somewhere other than /Applications
#   VEYQA_NO_OPEN=1                   do not launch the app when finished
set -euo pipefail

APP_NAME="Veyqa Voice"
BUNDLE_ID="com.wjcdev.veyqa"
CERT_SHA1="6cb7ed42af70dafacda95e4be77a095426b76b85"
FEED="https://wjc-dev.github.io/veyqa-voice/appcast.xml"

say() { printf '%s\n' "$*"; }
die() { printf 'Veyqa Voice: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "this installer only runs on macOS"
[ "$(uname -m)" = "arm64" ] || die "Veyqa Voice requires Apple Silicon (arm64)"
[ "$(sw_vers -productVersion | cut -d. -f1)" -ge 13 ] || die "Veyqa Voice requires macOS 13 or later"

say "读取最新版本…"
appcast="$(curl -fsSL --proto '=https' --tlsv1.2 --retry 3 "$FEED")" \
  || die "无法访问更新源：$FEED"
url="$(printf '%s\n' "$appcast" | sed -n 's/.*enclosure url="\([^"]*\.zip\)".*/\1/p' | head -n 1)"
[ -n "$url" ] || die "更新源里没有找到安装包"
case "$url" in
  https://github.com/wjc-dev/veyqa-voice/releases/download/*) ;;
  *) die "更新源指向了非官方地址：$url" ;;
esac

tmp="$(mktemp -d "${TMPDIR:-/tmp}/veyqa-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

say "下载安装包…"
curl -fL --proto '=https' --tlsv1.2 --retry 3 --progress-bar -o "$tmp/app.zip" "$url" \
  || die "下载失败"

say "校验签名…"
ditto -x -k "$tmp/app.zip" "$tmp/unpacked" || die "安装包解压失败"
source_app="$tmp/unpacked/$APP_NAME.app"
[ -d "$source_app" ] || die "安装包里没有 $APP_NAME.app"
codesign --verify --deep --strict "$source_app" 2>/dev/null || die "代码签名校验失败"
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$source_app/Contents/Info.plist" 2>/dev/null)" = "$BUNDLE_ID" ] \
  || die "应用标识不符"
codesign -d -r- "$source_app" 2>&1 | grep -q "certificate leaf = H\"$CERT_SHA1\"" \
  || die "安装包不是 Veyqa 官方发布证书签名的"

if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
  say "退出正在运行的 Veyqa Voice…"
  osascript -e "tell application \"$APP_NAME\" to quit" >/dev/null 2>&1 || true
  n=0
  while pgrep -x "$APP_NAME" >/dev/null 2>&1 && [ "$n" -lt 20 ]; do
    sleep 0.25
    n=$((n + 1))
  done
  if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
    pkill -x "$APP_NAME" >/dev/null 2>&1 || true
    sleep 2
  fi
  if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
    die "Veyqa Voice 仍在运行，请先退出后重新运行安装"
  fi
fi

if [ -n "${VEYQA_INSTALL_DIR:-}" ]; then
  dest="$VEYQA_INSTALL_DIR"
  mkdir -p "$dest"
elif [ -w /Applications ]; then
  dest="/Applications"
else
  dest="$HOME/Applications"
  mkdir -p "$dest"
  say "/Applications 不可写，改为安装到 $dest"
fi
[ -w "$dest" ] || die "$dest 不可写"

target="$dest/$APP_NAME.app"
staged="$dest/.$APP_NAME.app.new"
old="$dest/.$APP_NAME.app.old"
rm -rf "$staged" "$old"
ditto "$source_app" "$staged"
xattr -dr com.apple.quarantine "$staged" 2>/dev/null || true
if [ -e "$target" ]; then
  mv "$target" "$old"
fi
mv "$staged" "$target"
rm -rf "$old"

say "已安装：$target"
if [ "${VEYQA_NO_OPEN:-0}" = "1" ]; then
  exit 0
fi
say "正在打开 Veyqa Voice…"
open "$target"
say "首次打开时按提示开启麦克风、辅助功能和输入监控，然后按住右 Option 试一句。"
