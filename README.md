# Veyqa Voice

**轻量的 Mac 语音输入工具。按住说话，松开输入。**

[官网](https://wjc-dev.github.io/voxtype/) · [下载安装包](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.7) · [English](README.en.md)

[![Veyqa Voice 语音输入演示](website/product-preview.svg)](https://wjc-dev.github.io/voxtype/#demo)

- 与现有输入法一起使用：拼音、双拼、Rime 照常输入。
- 直接转写、保留原话，文字写入当前输入框，由你确认发送。
- 支持千问与豆包。添加 `user_id`、`seller_id` 等专有词汇，提升识别概率。

## 安装

需要 **Apple Silicon（M 系列芯片）、macOS 13+**。

1. [下载 DMG](https://github.com/wjc-dev/voxtype/releases/download/v0.2.7/Veyqa-Voice-v0.2.7-macOS-arm64.dmg) 并打开。
2. 按窗口中的箭头，将 **Veyqa Voice 拖入 Applications（应用程序）**。
3. 从“应用程序”打开 Veyqa Voice，完成首次设置。

首次提示“无法验证开发者”？查看[打开步骤与图文帮助](https://wjc-dev.github.io/voxtype/#safety)。当前为尚未经过 Apple 公证的 Preview。

也可以使用 Homebrew 安装：

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

## 首次设置

1. **连接服务**：默认千问，粘贴自己的百炼 API Key，点击“连接并继续”。[获取 Key 的步骤](https://wjc-dev.github.io/voxtype/#api-key)。
2. **开启权限**：按引导开启麦克风、辅助功能和输入监控，返回后自动检查。系统要求重新打开时按提示操作即可。
3. **试用输入**：点击试用文本框，按住 **右 Option** 说话，松开写入。完成后即可在其他 App 的输入框使用。

点击菜单栏或 Dock 图标打开设置。快捷键、按住／切换模式和菜单栏显示均可调整，保存后生效，升级保留配置。

## 语音服务

默认使用 **Qwen Audio 3.0 ASR Streaming**，支持流式识别、多语种及方言、热词增强；也可使用豆包语音服务。[模型与配置说明](https://wjc-dev.github.io/voxtype/#engines)。

API Key 保存在本机，音频直接发送至所选语音服务，API 费用由对应服务商收取。本地录音存档默认关闭。

[反馈问题](https://github.com/wjc-dev/voxtype/issues) · [开发文档](docs/development.md) · [贡献](CONTRIBUTING.md) · [安全问题](SECURITY.md)

## 许可证

[MIT](LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生，保留原始版权声明。
