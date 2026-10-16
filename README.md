# Veyqa Voice

**说出来，即刻输入。** 为你的 Mac，添一个语音输入键。

[官网](https://wjc-dev.github.io/voxtype/) · [下载安装包](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.5) · [English](README.en.md)

- 按住右 Option 说话，松开写入，不替你发送。
- 不替换你的输入法，不默认添加 AI 润色。
- 支持千问和豆包，使用自己的 API 凭证。

## 安装

需要 **Apple Silicon（M 系列芯片）、macOS 13+**。

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

也可下载安装包，将 Veyqa Voice 拖入“应用程序”。想让本地 AI Agent 帮忙？到[官网复制安装配置提示词](https://wjc-dev.github.io/voxtype/#agent)。

## 开始使用

1. 打开应用，默认千问：点击“获取 API Key”，自行填写 Key，再“检查连接”。也可切换豆包。
2. 按引导确认**麦克风、辅助功能和输入监控**权限，测试麦克风后点击“完成，开始使用”。以后修改设置只需“保存”，无需重启。
3. 把光标放进文本框，**按住右 Option 说话，松开写入**。点击 Dock 图标可调整设置。

快捷键和操作方式可自定义。新安装默认右 Option；升级保留已有凭证、快捷键及配置。Key 输入框支持显示／隐藏和一键复制。

## 使用前了解

当前为 **0.2.5 Preview**，尚未经过 Apple 公证。若首次打开提示“无法验证开发者／未经公证”，确认安装包来自官网且你信任其来源后，可由你在「系统设置 → 隐私与安全性」点击「仍要打开」，再确认「打开」。这是仅针对该 App 的例外授权，不需要关闭系统防护。[苹果官方说明](https://support.apple.com/zh-cn/102445)

如果提示恶意软件、文件损坏，或被公司安全软件拦截，请停止并反馈／联系管理员，不强行放行。

语音会发送到所选云端 ASR 服务，API 费用由服务商收取；默认不保存本地录音。不要把 API Key 发给任何聊天或 Agent。

[反馈问题](https://github.com/wjc-dev/voxtype/issues) · [开发文档](docs/development.md) · [贡献](CONTRIBUTING.md) · [安全问题](SECURITY.md)

## 许可证

[MIT](LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生，保留原始版权声明。
