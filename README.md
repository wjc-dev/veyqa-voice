# Veyqa Voice

**说出来，即刻输入。** 为你的 Mac，添一个语音输入键。

[官网](https://wjc-dev.github.io/voxtype/) · [下载安装包](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.3) · [English](README.en.md)

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
2. 按引导确认**麦克风、辅助功能和输入监控**权限，然后“保存并重启”。
3. 把光标放进文本框，**按住右 Option 说话，松开写入**。点击 Dock 图标可调整设置。

快捷键和操作方式可自定义。升级保留已有凭证、快捷键及配置；右 Option 是 0.2.3 起的新用户默认值。

## 使用前了解

当前为 **0.2.3 Preview**，尚未经过 Apple 公证，系统可能提示安全确认。办公电脑请遵循管理员策略，不关闭安全防护。

语音会发送到所选云端 ASR 服务，API 费用由服务商收取；默认不保存本地录音。不要把 API Key 发给任何聊天或 Agent。

[反馈问题](https://github.com/wjc-dev/voxtype/issues) · [开发文档](docs/development.md) · [贡献](CONTRIBUTING.md) · [安全问题](SECURITY.md)

## 许可证

[MIT](LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生，保留原始版权声明。
