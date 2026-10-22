# Veyqa Voice 安全与隐私

## 数据流

Veyqa Voice 是本地 macOS 客户端，不是离线识别模型。一次正常语音输入的数据路径是：

1. macOS 默认麦克风采集 16 kHz 音频；
2. 音频在内存中发送给用户选择的千问或豆包 ASR；
3. 流式文字只显示在本地悬浮胶囊；
4. 松开快捷键后，稳定的最终文本通过 macOS 辅助功能写到原光标；
5. 最终文字只短暂写入剪贴板并立即恢复原内容；不模拟 Return，不自动发送消息。

## 本地保存

- 默认不保存录音：`AUDIO_ARCHIVE_ENABLED=false`。
- API 凭证与设置位于当前用户的 `~/Library/Application Support/Veyqa`；首次运行会迁移旧版设置；源码模式位于项目的 `.env`。
- 本机纠错可能保存“识别错误 → 人工改为”及次数，因此可能含有私人词汇。
- 写入失败时的恢复记录可能包含最近的转写文字。
- 这些文件只应由当前用户读取，不应上传到 Git、Issue、聊天或安装包。

## 云端边界

选择千问时音频会发送给阿里云百炼；选择豆包时音频会发送给火山引擎豆包语音。服务端传输、日志、留存、内容处理与计费规则由用户账号对应的云服务协议和控制台配置决定。本项目不声称云端零留存，也不能替用户承诺第三方云服务的合规性。

## 凭证

- 推广语音使用维护者提供的凭证，由维护者承担调用费用；用户也可以选择自己的 API 凭证。
- 千问需要百炼 API Key 与匹配的 API Host；不要填写阿里云 AccessKey ID/Secret。
- 豆包需要语音应用的 App ID 与 Access Token。
- 当前版本把凭证保存在权限受限的本地配置文件中，尚未使用 macOS Keychain。不要在共享账号或不受信任电脑上保存生产凭证。

## 仓库防泄漏

`.gitignore` 和 `scripts/package-source.command` 会排除 `.env`、个人上下文、纠错、恢复、诊断、录音、日志、虚拟环境、构建与输出目录。提交前仍必须人工运行凭证扫描；忽略规则不是秘密管理方案。

建议检查：

```bash
git status --short
git ls-files | rg '(^|/)(\.env|personal_context\.txt|corrections\.json|recovery\.json|diagnostics\.json|audio_archive|logs)(/|$)'
rg -n 'sk-ws-|DOUBAO_ACCESS_KEY=.+|QWEN_API_KEY=.+' --glob '!outputs/**' --glob '!build/**' --glob '!.venv/**'
```

发现凭证已经提交时，不要只删除文件：立即在对应云控制台重置/吊销凭证，再清理 Git 历史。

## 内测签名限制

当前小范围分发包没有 Apple Developer ID，使用 ad-hoc 签名且不能公证。每次重建都会改变代码签名身份，macOS 可能要求重新授权辅助功能和输入监控。正式扩大分发前应申请 Developer ID、启用 hardened runtime、完成公证，并迁移到 Keychain。
