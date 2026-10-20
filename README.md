# Veyqa Voice

**轻量的 Mac 语音输入工具。按住说话，松开输入。**

[官网](https://wjc-dev.github.io/voxtype/) · [下载安装包](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.10) · [English](README.en.md)

[![Veyqa Voice 语音输入演示](website/product-preview.svg)](https://wjc-dev.github.io/voxtype/#demo)

- 与现有输入法一起使用：拼音、双拼、Rime 照常输入。
- 直接转写、保留原话，文字写入当前输入框，由你确认发送。
- 支持千问与豆包。添加 `user_id`、`seller_id` 等专有词汇，提升识别概率。
- 推广期间无需注册或填写 API Key，也可使用自己的语音服务账户。

## 安装

需要 **Apple Silicon（M 系列芯片）、macOS 13+**。

1. [下载 DMG](https://github.com/wjc-dev/voxtype/releases/download/v0.2.10/Veyqa-Voice-v0.2.10-macOS-arm64.dmg) 并打开。
2. 按窗口中的箭头，将 **Veyqa Voice 拖入 Applications（应用程序）**。
3. 从“应用程序”打开 Veyqa Voice，完成首次设置。

首次提示“无法验证开发者”？查看[打开步骤与图文帮助](https://wjc-dev.github.io/voxtype/#safety)。当前为尚未经过 Apple 公证的 Preview。

也可以使用 Homebrew 安装：

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

## 更新

在“设置 → 通用 → 应用”点击“检查更新”，按提示下载并重新打开。默认每天检查新版，Key 和配置保留，无需先卸载。

0.2.8 及更早版本需先用下方命令或 DMG 更新一次，之后即可在 App 内更新。

也可以先退出 App，再使用 Homebrew：

```bash
brew update
brew upgrade --cask wjc-dev/veyqa/veyqa-voice
```

DMG 用户：下载上方新版安装包，将 App 拖入“应用程序”，选择“替换”。如果 macOS 提示权限需要确认，按应用内引导开启即可。

## 首次设置

1. **连接服务**：默认“推广语音”，无需填写 Key，点击“连接并继续”。也可使用自己的 API，[配置步骤](https://wjc-dev.github.io/voxtype/#api-key)。
2. **开启权限**：按引导开启麦克风、辅助功能和输入监控，返回后自动检查。系统要求重新打开时按提示操作即可。
3. **试用输入**：点击试用文本框，按住 **右 Option** 说话，松开写入。完成后即可在其他 App 的输入框使用。

点击菜单栏或 Dock 图标打开设置。快捷键、按住／切换模式和菜单栏显示均可调整，保存后生效，升级保留配置。

## 语音服务

推广语音使用 **Qwen Audio 3.0 ASR Streaming**，由 Veyqa Voice 承担调用费用。已有用户升级保留自己的 Key 和模型，可在“设置 → 语音服务”切换推广语音。

使用自己的 API 时，可选择千问或豆包模型、刷新官方目录，也可添加 OpenAI 兼容的自定义转写服务。千问和豆包保持流式识别，自定义服务在录音结束后转写。[模型与配置说明](https://wjc-dev.github.io/voxtype/#engines)。

音频直接发送至所选语音服务，本地录音存档默认关闭。个人 API Key 保存在本机，对应费用由你的服务商账户承担。

[反馈问题](https://github.com/wjc-dev/voxtype/issues) · [开发文档](docs/development.md) · [贡献](CONTRIBUTING.md) · [安全问题](SECURITY.md)

## 许可证

[MIT](LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生，保留原始版权声明。
