# 参与 Veyqa Voice 开发

## 开发环境

需要 Apple Silicon Mac、macOS 13+、Python 3.13 和 Xcode Command Line Tools。

```bash
git clone https://github.com/wjc-dev/veyqa-voice.git
cd veyqa-voice
uv venv --python 3.13 .venv
uv pip install -r requirements/dev.txt
cp config/env.example .env
zsh scripts/build-settings.command
.venv/bin/python main.py
```

测试凭证只保存在本机 `.env`，不要提交到 Git 或附在 Issue 中。安装包使用独立的用户配置目录，见[开发文档](../docs/development.md)。

## 修改与验证

```bash
.venv/bin/python -m pytest test/ -q
node --test test/test_website.mjs
```

- Python 逻辑：修改 `main.py` 或 `src/`。
- 原生设置：修改 `native_settings/` 后运行 `zsh scripts/build-settings.command`。
- 快捷键与写入：修改后必须在至少 TextEdit、浏览器输入框和聊天输入框中人工验证短按、长按、Esc 取消、切窗和连续两段。
- ASR：需要比较引擎时用 `python tools/compare_asr_once.py --duration 12`；同一份音频按顺序发送给两端，音频不写盘。

## 提交修改

不要直接在 `main` 上开发：

```bash
git switch -c feature/short-description
git add <明确的文件>
git commit -m "feat: describe the change"
git push -u origin feature/short-description
```

在 GitHub 提交 Pull Request，说明解决的问题、修改后的行为和测试结果。提交前检查 `git status --short` 和 `git diff --cached`。

## 版本与安装包

1. 同步应用、原生设置与打包脚本的版本号。
2. 运行全部自动测试。
3. 运行 `zsh scripts/build-macos.command`，生成 App、DMG、PKG、ZIP 与 SHA-256。
4. 在一台未装源码版的 Mac 上执行冒烟测试。
5. 安装包通过 GitHub Releases 发布，附 SHA-256；同步签名更新清单与 Homebrew。

签名身份与发布验证流程见[开发文档](../docs/development.md#开发签名与权限)。

## 源码包

```bash
zsh scripts/package-source.command
```

脚本会排除并审计凭证、本机纠错、恢复、日志、录音和虚拟环境。生成后仍应检查清单与哈希，再对外发送。
