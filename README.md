# Veyqa Voice

**轻量的 Mac 语音输入工具。按住说话，松开输入。**

[官网](https://wjc-dev.github.io/veyqa-voice/) · [下载安装包](https://github.com/wjc-dev/veyqa-voice/releases/tag/v0.3.0) · [English](README.en.md)

[![Veyqa Voice 语音输入演示](website/product-preview.svg)](https://wjc-dev.github.io/veyqa-voice/#demo)

- 与现有输入法一起使用：拼音、双拼、Rime 照常输入。
- 直接转写、保留原话，文字写入当前输入框，由你确认发送。
- 支持千问与豆包。添加 `user_id`、`seller_id` 等专有词汇，提升识别概率。
- 推广期间无需注册或填写 API Key，也可使用自己的语音服务账户。

## 安装

需要 **Apple Silicon（M 系列芯片）、macOS 13+**。

推荐在“终端”粘贴一行命令安装。它不经过浏览器，因此不会出现“无法验证开发者”提示：

```bash
curl -fsSL https://wjc-dev.github.io/veyqa-voice/install.sh | bash
```

脚本会读取官方更新源、校验应用签名，安装到“应用程序”并打开。升级时重复运行即可，配置和授权保留。源码见 [website/install.sh](website/install.sh)。

也可以使用 Homebrew 安装（同样没有该提示）：

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

或者手动安装：

1. [下载 DMG](https://github.com/wjc-dev/veyqa-voice/releases/download/v0.3.0/Veyqa-Voice-v0.3.0-macOS-arm64.dmg) 并打开。
2. 按窗口中的箭头，将 **Veyqa Voice 拖入 Applications（应用程序）**。
3. 从“应用程序”打开 Veyqa Voice，完成首次设置。

DMG 由浏览器下载，首次打开可能提示“无法验证开发者”。在“系统设置 → 隐私与安全性”点击“仍要打开”，或运行 `xattr -dr com.apple.quarantine "/Applications/Veyqa Voice.app"`。[查看说明](https://support.apple.com/zh-cn/102445)。

## 更新

在“设置 → 通用 → 应用”点击“检查更新”，按提示下载并重新打开。默认每天检查新版，Key 和配置保留，无需先卸载。

0.2.8 及更早版本需先用下方命令或 DMG 更新一次，之后即可在 App 内更新。

0.3.0 起听写引擎改为原生实现，沿用同一应用标识、数据目录和签名身份；升级保留 API Key、快捷键、词库和系统授权。

也可以先退出 App，再使用 Homebrew：

```bash
brew update
brew upgrade --cask wjc-dev/veyqa/veyqa-voice
```

DMG 用户：下载上方新版安装包，将 App 拖入“应用程序”，选择“替换”。如果 macOS 提示权限需要确认，按应用内引导开启即可。

## 首次设置

1. **连接服务**：默认“内置服务”，无需填写 Key，点击“连接并继续”。也可使用个人 API，[配置步骤](https://wjc-dev.github.io/veyqa-voice/#api-key)。
2. **开启权限**：按引导开启麦克风、辅助功能和输入监控，返回后自动检查。系统要求重新打开时按提示操作即可。
3. **试用输入**：点击试用文本框，按住 **右 Option** 说话，松开写入。完成后即可在其他 App 的输入框使用。

点击菜单栏图标打开设置，也可从“应用程序”再次打开。新安装默认不占用 Dock；如需显示，可在“通用”中开启。快捷键、按住／切换模式均可调整，保存后生效，升级保留配置。

## 语音服务

内置服务使用 **Qwen Audio 3.1 ASR Message**（`qwen-audio-3.1-asr-flash-message`），支持边说边显示，默认保留原话，由 Veyqa Voice 承担调用费用。已有用户升级保留自己的 Key 和模型，可在“设置 → 语音服务”切换内置服务。

使用个人 API 时，可选择千问或豆包模型、刷新官方目录，也可添加 OpenAI 兼容的自定义转写服务。千问和豆包保持流式识别，自定义服务在录音结束后转写。[模型与配置说明](https://wjc-dev.github.io/veyqa-voice/#engines)。

音频直接发送至所选语音服务，本地录音存档默认关闭。个人 API Key 保存在本机，对应费用由你的服务商账户承担。

## 源码与构建

0.3.0 起的听写引擎是本仓库的 Rust 实现，设置页与 supervisor 为 Swift。0.2.18 及更早的 Python 版源码保留在 [`python-0.2.18`](https://github.com/wjc-dev/veyqa-voice/tree/python-0.2.18) 标签。

- `src/`：快捷键、录音、语音服务、文本清洗、写入与恢复。
- `native/`：Swift 设置页、supervisor 与更新桥接。
- `config/`：随包资源（模型目录、图标、功能开关）。
- `website/`：官网与 Sparkle 更新源。
- `scripts/`：构建、打包与测量脚本。

```bash
# 开发构建：独立 bundle id 与数据目录，可与已安装版并存
./scripts/build-rust-macos.command

# 生产构建：沿用 com.wjcdev.veyqa 身份，再生成 ZIP/DMG 与更新源
VEYQA_DISTRIBUTION=public ./scripts/build-rust-macos.command
./scripts/package-public.command
```

需要 Apple Silicon Mac、macOS 13+、Rust 1.85+ 与 Xcode Command Line Tools。构建使用固定签名证书并要求其已在本机钥匙串中，可用 `VEYQA_SIGNING_IDENTITY` 指定其他证书指纹；更新源由 Sparkle EdDSA 密钥签名。

[反馈问题](https://github.com/wjc-dev/veyqa-voice/issues) · [贡献](.github/CONTRIBUTING.md) · [安全问题](.github/SECURITY.md)

## 许可证

[MIT](LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生，保留原始版权声明。
