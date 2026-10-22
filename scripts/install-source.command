#!/bin/zsh
set -euo pipefail

PROJECT_DIR="${0:A:h:h}"
cd "$PROJECT_DIR"

LABEL="com.whisper-input-next"
USER_ID="$(id -u)"
PLIST_PATH="$HOME/Library/LaunchAgents/${LABEL}.plist"

print ""
print "Veyqa Voice — macOS 安装"
print "项目目录：$PROJECT_DIR"
print ""

find_python() {
  local candidate
  for candidate in python3.12 python3.11 python3.10 python3; do
    if command -v "$candidate" >/dev/null 2>&1; then
      if "$candidate" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' 2>/dev/null; then
        command -v "$candidate"
        return 0
      fi
    fi
  done
  return 1
}

PYTHON_BIN="$(find_python || true)"
if [[ -z "$PYTHON_BIN" ]]; then
  print -u2 "没有找到 Python 3.10 或更高版本。"
  print -u2 "请先安装 Python 3.12，例如：brew install python@3.12"
  read -k 1 "?按任意键退出…"
  print ""
  exit 1
fi

print "[1/5] 创建 Python 环境…"
if [[ ! -x .venv/bin/python ]]; then
  "$PYTHON_BIN" -m venv .venv
fi
.venv/bin/python -m pip install --upgrade pip

print "[2/5] 安装依赖（首次可能需要几分钟）…"
.venv/bin/python -m pip install -r requirements/runtime.txt

print "[3/5] 准备本机配置…"
if [[ ! -f .env ]]; then
  cp config/env.example .env
fi
chmod 600 .env
mkdir -p logs build "$HOME/Library/LaunchAgents"

print "[4/5] 构建原生 macOS 设置窗口…"
if xcrun --find swiftc >/dev/null 2>&1; then
  "$PROJECT_DIR/scripts/build-settings.command"
else
  print "未检测到 Swift 编译器，将使用兼容设置窗口。"
  print "如需原生设置窗口，请安装：xcode-select --install"
fi

print "[5/5] 安装后台常驻服务…"
xml_escape() {
  sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'
}
ESCAPED_PROJECT_DIR="$(print -r -- "$PROJECT_DIR" | xml_escape)"
ESCAPED_PYTHON="$(print -r -- "$PROJECT_DIR/.venv/bin/python" | xml_escape)"

PLIST_TEMP="${PLIST_PATH}.tmp"
cat > "$PLIST_TEMP" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${ESCAPED_PYTHON}</string>
    <string>${ESCAPED_PROJECT_DIR}/main.py</string>
  </array>
  <key>WorkingDirectory</key><string>${ESCAPED_PROJECT_DIR}</string>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>EnvironmentVariables</key>
  <dict><key>PYTHONUNBUFFERED</key><string>1</string></dict>
  <key>StandardOutPath</key><string>${ESCAPED_PROJECT_DIR}/logs/launchd.stdout.log</string>
  <key>StandardErrorPath</key><string>${ESCAPED_PROJECT_DIR}/logs/launchd.stderr.log</string>
</dict>
</plist>
PLIST
plutil -lint "$PLIST_TEMP" >/dev/null
mv "$PLIST_TEMP" "$PLIST_PATH"
chmod 600 "$PLIST_PATH"

launchctl bootout "gui/${USER_ID}/${LABEL}" >/dev/null 2>&1 || true
launchctl bootstrap "gui/${USER_ID}" "$PLIST_PATH"
launchctl kickstart -k "gui/${USER_ID}/${LABEL}"

print ""
print "安装完成。接下来会打开设置窗口，请填写你自己的千问凭证。"
print "首次使用时，macOS 还会要求麦克风和辅助功能权限。"

"$PROJECT_DIR/scripts/open-settings.command" >/dev/null 2>&1 &!

open "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone" || true

print ""
print "如果系统没有自动弹出权限提示，请阅读《安装与分发说明.md》。"
read -k 1 "?按任意键关闭此窗口…"
print ""
