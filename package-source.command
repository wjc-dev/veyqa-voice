#!/bin/zsh
set -euo pipefail

PROJECT_DIR="${0:A:h}"
PROJECT_PARENT="${PROJECT_DIR:h}"
VERSION_DATE="$(date +%Y-%m-%d)"
PACKAGE_NAME="Veyqa-Voice-v0.2.12"
OUTPUT_DIR="${VOICE_INPUT_PACKAGE_OUTPUT_DIR:-$PROJECT_PARENT}"
OUTPUT_PATH="${OUTPUT_DIR}/${PACKAGE_NAME}-macOS-source-${VERSION_DATE}.tar.gz"
STAGING_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$STAGING_DIR"
}
trap cleanup EXIT

mkdir -p "$OUTPUT_DIR" "$STAGING_DIR/$PACKAGE_NAME"

# Copy a clean, distributable source tree. Credentials, learned personal data,
# generated binaries, logs and historical upstream material stay on this Mac.
rsync -a \
  --exclude='.git/' \
  --exclude='.env' \
  --exclude='.env.*' \
  --exclude='.env_*' \
  --exclude='.venv/' \
  --exclude='build/' \
  --exclude='.package-build/' \
  --exclude='dist-internal/' \
  --exclude='dist-local/' \
  --exclude='outputs/' \
  --exclude='trash/' \
  --exclude='report*/' \
  --exclude='.codegraph/' \
  --exclude='docs/release-candidate-verification.md' \
  --exclude='docs/superpowers/specs/' \
  --exclude='.settings-open' \
  --exclude='.settings-instance' \
  --exclude='.shortcut-capture' \
  --exclude='packaging/AppIcon.icns' \
  --exclude='logs/' \
  --exclude='audio_archive/' \
  --exclude='personal_context.txt' \
  --exclude='custom_vocabulary.txt' \
  --exclude='corrections.json' \
  --exclude='recovery.json' \
  --exclude='diagnostics.json' \
  --exclude='permissions.json' \
  --exclude='.permission-request.json' \
  --exclude='.DS_Store' \
  --exclude='__pycache__/' \
  --exclude='*.pyc' \
  --exclude='assets/audio/' \
  --exclude='assets/images/' \
  --exclude='control_ui.py' \
  --exclude='start.sh' \
  --exclude='src/llm/' \
  --exclude='src/transcription/whisper.py' \
  --exclude='src/transcription/local_whisper.py' \
  --exclude='src/transcription/senseVoiceSmall.py' \
  "$PROJECT_DIR/" "$STAGING_DIR/$PACKAGE_NAME/"

LC_ALL=C COPYFILE_DISABLE=1 tar -czf "$OUTPUT_PATH" -C "$STAGING_DIR" "$PACKAGE_NAME"

# Fail closed: a distributable archive must never contain local credentials or
# personal runtime data.  Keep this audit in the packaging script so future
# contributors cannot accidentally weaken an rsync exclusion unnoticed.
FORBIDDEN_PATH_PATTERN='/(\.env|personal_context\.txt|custom_vocabulary\.txt|corrections\.json|recovery\.json|diagnostics\.json|permissions\.json|\.permission-request\.json|\.venv|logs|audio_archive|\.git|build|outputs|\.settings-open|\.settings-instance|\.shortcut-capture)(/|$)'
if LC_ALL=C tar -tzf "$OUTPUT_PATH" | grep -E "$FORBIDDEN_PATH_PATTERN" >/dev/null; then
  print -u2 "安全审计失败：压缩包包含本机配置或运行数据。"
  exit 1
fi

if grep -R -I -E \
  '^[[:space:]]*(QWEN_API_KEY|DOUBAO_APP_KEY|DOUBAO_ACCESS_KEY)=[^[:space:]#][^[:space:]]*|sk-ws-[A-Za-z0-9]{20,}|sk-sp-[A-Za-z0-9]{20,}|sk-[A-Za-z0-9]{24,}' \
  "$STAGING_DIR/$PACKAGE_NAME" >/dev/null; then
  print -u2 "安全审计失败：源码包中发现疑似真实 API Key。"
  exit 1
fi

print "已生成：$OUTPUT_PATH"
print "隐私审计通过：未包含凭证、个人上下文、纠错词库、录音、日志或虚拟环境。"
LC_ALL=C shasum -a 256 "$OUTPUT_PATH" > "$OUTPUT_PATH.sha256"
cat "$OUTPUT_PATH.sha256"
