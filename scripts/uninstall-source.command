#!/bin/zsh
set -euo pipefail

# Removes the source-development service; packaged app installs use Homebrew/Finder.

LABEL="com.whisper-input-next"
USER_ID="$(id -u)"
PLIST_PATH="$HOME/Library/LaunchAgents/${LABEL}.plist"

launchctl bootout "gui/${USER_ID}/${LABEL}" >/dev/null 2>&1 || true
if [[ -f "$PLIST_PATH" ]]; then
  rm -f "$PLIST_PATH"
fi

print "后台常驻服务和 LaunchAgent 已移除。"
print "项目目录、API 配置和本地纠错记录均未删除；如不再需要，可手动删除整个项目文件夹。"
read -k 1 "?按任意键关闭…"
print ""
