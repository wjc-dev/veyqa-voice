# Veyqa Voice

**思路不用等，说出来就好。** 为你的 Mac，添一个语音输入键。

[官网](https://wjc-dev.github.io/voxtype/) · [下载安装包](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.6) · [English](README.en.md)

[![按住右 Option，说话时查看预览，松开后写入；点击观看官网交互演示](website/product-preview.svg)](https://wjc-dev.github.io/voxtype/#demo)

- **一个键，就能说。** 按住右 Option，松开写入；文字不会自动发送。
- **是你的话，不是套话。** 不默认加 AI 润色，也不替换你原来的输入法。
- **服务由你选择。** 支持千问、豆包，使用自己的 API Key，费用由服务商收取。

## 安装

需要 **Apple Silicon（M 系列芯片）、macOS 13+**。

**推荐：**[下载 DMG](https://github.com/wjc-dev/voxtype/releases/download/v0.2.6/Veyqa-Voice-v0.2.6-macOS-arm64.dmg)，打开后将 Veyqa Voice 拖入“应用程序”，再从“应用程序”打开。不需要 Homebrew、Python 或开发环境。

已有 Homebrew？也可以在终端安装：

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

想让本地 AI Agent 帮忙？到[官网复制安装配置提示词](https://wjc-dev.github.io/voxtype/#agent)。

## 第一次设置，以后一个键

1. **连接服务：**默认千问，粘贴自己的百炼 API Key，点击“连接并继续”，成功后自动保存。没有 Key？展开应用里的获取步骤，或看[百炼官方教程](https://help.aliyun.com/zh/model-studio/get-api-key)。也可切换豆包。
2. **开启权限：**按引导确认麦克风、辅助功能和输入监控权限。
3. **直接试用：**点击应用内的试用文本框，**按住右 Option 说话，松开结束**。看到文字后点击“完成设置”，之后可在其他 App 的输入框使用。点击 Dock 图标可调整设置，保存即可生效。

不熟悉 API Key？看官网的[三步配置图示](https://wjc-dev.github.io/voxtype/#api-key)。快捷键和按住／切换模式均可调整，升级保留已有配置。

## 使用前了解

当前为 **0.2.6 Preview**，尚未经过 Apple 公证。语音会发送到所选云端 ASR 服务，默认不保存本地录音；不要把 API Key 发给任何聊天或 Agent。

<details>
<summary>首次打开被 macOS 阻止？</summary>

若提示“无法验证开发者／未经公证”，确认安装包来自官网且你信任其来源后，可由你在「系统设置 → 隐私与安全性」点击「仍要打开」，再确认「打开」。这是仅针对该 App 的例外授权，不需要关闭系统防护。[苹果官方说明](https://support.apple.com/zh-cn/102445)

如果提示恶意软件、文件损坏，或被公司安全软件拦截，请停止并反馈／联系管理员，不强行放行。

</details>

[反馈问题](https://github.com/wjc-dev/voxtype/issues) · [开发文档](docs/development.md) · [贡献](CONTRIBUTING.md) · [安全问题](SECURITY.md)

## 许可证

[MIT](LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生，保留原始版权声明。
