# Veyqa Voice

[English](./README.en.md) · 简体中文

> 版本状态（2026-10-16）：**0.2.2 Preview** 是测试版，不承诺零缺陷。公开安装包仍为 ad-hoc 签名、未经 Apple 公证；本机固定证书构建与公开包分开。实际验收范围和限制见 [验证记录](docs/release-candidate-verification.md)。

macOS 上的语音速记工具。按住快捷键说话,松开,文字落到光标处。

不替换你的系统输入法。拼音、双拼、五笔继续用;需要语音的时候按住一个键就行。

## 为什么用它

- 喜欢搜狗 / 双拼 / Rime,但偶尔想用语音 → 不用为了语音能力换走你常用的输入法
- 千问 Audio 3.0 和豆包 Seed ASR 2.0 都能用,自带 API 凭证即可
- 按住说话、松开提交,没有 marked text 下划线,不打扰写作流
- 浮窗稳定在屏幕底部居中,不抢焦点
- Dock 图标常驻，点击即可重新打开 SwiftUI 原生设置面板
- 音频发送给所选云端语音服务；默认不保存本地录音，日志不记识别内容
- MIT 协议,基于 ErlichLiu 的 Whisper-Input 衍生

## 安装

使用 Homebrew：

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

也可以下载最新版 Veyqa Voice 安装包，把 Veyqa Voice 拖到应用程序。首次启动按系统提示授权麦克风和辅助功能。

需要 macOS 13+ 和 Apple Silicon。

## 配置

以下简化配置界面属于 0.2.2 开发候选版；Homebrew 0.2.0 仍需在原设置页选择服务并填写凭证。

1. 打开 Veyqa Voice，默认千问：点击“获取 API Key”，粘贴 Key，点击“检查连接”。一般无需填写服务器地址；海外或自定义业务空间在“高级设置”中选择。已有配置会保留。也可切换豆包。
2. 默认快捷键是 `⌃⌥Space`：由 macOS 注册，不需要“输入监控”权限，也能明确提示是否被其他 App 占用。设置页也可以直接录入其他“修饰键 + 普通键”组合。
3. 保存并重启。把光标放进任何输入框,按住快捷键说话,松开提交。

右/左 Option、Command、Control 或 Fn 也可以单独使用，但这类“单修饰键”必须通过只读键盘监听实现，需要“输入监控”权限，也更容易和系统、远程桌面、Karabiner 或企业安全软件冲突。若追求通用稳定，优先使用组合键。

## 后台运行与故障恢复

- 打包版使用 macOS 13+ 的 Service Management 注册一个轻量 supervisor。Veyqa Voice 意外消失时会恢复；若 60 秒内连续失败 3 次，会冷却 5 分钟，避免反复崩溃拉起。
- 快捷键监听被 macOS 暂停时会自动恢复；不允许 Quartz 只读监听的受管控电脑会降级到 AppKit 兼容模式。
- 录音设备调用卡住超过 10 秒或识别收尾超过 45 秒，会自动恢复后台服务；原生守护还检查进程心跳。自动恢复有限次重试，避免设备持续异常时无限重启。
- 更新或重新安装应用包后会同步刷新后台登录项，避免系统显示“已启用”，实际仍指向旧安装记录。
- 快捷键恢复后会重置按键状态，并校正漏掉的松键事件。取消录音独立于启动队列；无法确认输入位置时保留文字，用户可在“恢复与诊断”复制。
- Dock 图标是默认且稳定的主入口，点击会唤起同一个设置窗口，不会重复启动语音服务。菜单栏图标默认关闭，避免因空间不足或系统刷新而让用户误以为 App 已退出。
- “恢复与诊断”页会显示后台启动、权限、快捷键后端和最近一次运行结果。升级自 Voice Input 时，如旧进程仍在运行，会明确提示先退出旧版，避免双快捷键监听。

正式对外分发仍应使用稳定的 Developer ID 对 App/PKG 签名并完成 Apple 公证。仓库生成的 `internal` 产物是 ad-hoc 签名测试包，不应直接当作面向所有用户的正式发布包。

### 快速输入与文字恢复

默认保持流式识别 → 本地轻量规则 → 写入，不额外调用大模型润色，也不自动发送。异常结束会清理录音流及音频缓冲；可控自动恢复或连接中断时，尝试保留已通过语音安全检查的识别片段，用户可在「恢复与诊断」确认后复制。只保留最近 5 条，不因此开启录音存档；尚未识别的音频、进程被直接终止或存储故障不保证可恢复。

恢复列表只在数据变化时刷新，删除按选中的记录 ID 操作，不覆盖后来新增的文字。短暂的浮窗提示不抢输入焦点，也不会关闭下一次录音。

每次独立听写都保留完整原话，不再根据上一轮的相同或重叠内容删字；同一轮识别的重复最终回调仍只提交一次。

### 为什么调试更新后会重新授权？

当前 internal 构建使用 ad-hoc 签名，身份与具体代码版本绑定，换构建后可能无法复用隐私授权；只固定 App 名称、Bundle ID 或安装路径并不能解决。见 [Apple 关于签名身份与隐私授权的说明](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)。

- 长期方案：开发构建持续使用一致的 Apple Development 身份，公开分发使用 Developer ID；保持各自兼容的 designated requirement。开发/发布签名默认不同，不要轮流覆盖同一日用 App 并假定权限能共享。
- 免费的本机开发方案：用户自行创建并批准专用自签名代码签名证书，将其 SHA-1 指纹保存到 `~/Library/Application Support/Veyqa Build/signing-identity.sha1`。打包脚本会复用该身份，证书缺失就停止，不自动退回 ad-hoc；私钥留在钥匙串，不进入仓库。首次切换身份可能仍需授权，不能保证所有系统版本永不重新询问。
- 固定身份构建输出到 `dist-local/`，文件名带 `-local`，仅供本机使用。`VEYQA_SIGNING_IDENTITY=- zsh build-internal-dmg.command` 显式生成 `dist-internal/` ad-hoc 测试包，不修改本机签名选择。不要用 Homebrew 公开包覆盖固定签名的日用 App，否则身份改变可能再次要求授权。
- 日常调试先运行不启动 App 的单元测试和 Swift 类型检查；需要端到端测试时，只在获准环境执行。脚本不创建证书、不修改信任或绕过防护；固定身份包必须在固定安装路径测试，不能交给临时 App 验收脚本。此本机签名流程不包含 Developer ID、公证或 Installer 签名。
- 「检查并授权」只请求尚未授予的必要权限。普通组合键不需要输入监控；左右 Option、Command、Control 或 Fn 单键仍需要。不会自动更改已有快捷键。
- 首次授权仍须用户确认；不会修改 TCC 数据库、重置已有权限或关闭系统/终端安全防护。公证也不等于预先授予隐私权限。

### 空闲占用

- 快捷键仍由系统事件触发，不通过高频扫描按键实现。空闲时不开启录音或云端识别会话。
- 设置窗口关闭后设置进程退出；窗口不在前台时暂停界面刷新，数据未变化时不重复重绘。
- 后台权限状态通常每 30 秒检查一次，打开设置时加快到约 2 秒；状态不变不写盘。
- 故障检查保持每 2 秒，健康心跳文件约每 10 秒更新，采用原子写入但不强制刷盘。故障变化会立即发布，不以降低占用为由取消恢复机制。
- 可用 `.venv/bin/python tools/measure_idle.py 主进程PID 守护进程PID --seconds 60` 测量空闲 CPU（单核口径）与 RSS。请关闭设置、停止录音后测量；RSS 不等于 macOS 的完整内存占用，不同机型与系统负载结果会不同。

## 让本地 Agent 帮你安装

复制下面这段话给能操作你 Mac 的本地 Agent：

```text
请帮我在这台 Mac 安装并配置 Veyqa Voice。
官方仓库：https://github.com/wjc-dev/voxtype
官方安装：brew install --cask wjc-dev/veyqa/veyqa-voice

先确认是 Apple Silicon、macOS 13 或更新版本，检查是否已有 Veyqa Voice、VoxType、Voice Input。
若 brew 不在 PATH，先检查 /opt/homebrew/bin/brew；未安装时引导我按 https://brew.sh 的官方说明安装，管理员密码由我亲自输入。
升级前为 ~/Library/Application Support/Veyqa 做权限受限的本地备份，保留凭证、快捷键、词汇和配置；不要卸载清理用户数据。
如已由 Homebrew 安装，先 brew update，再 brew upgrade --cask wjc-dev/veyqa/veyqa-voice；未安装则使用上面的安装命令。
不要重复启动多个版本，也不要修改其他 App。替换正在录音的旧版本前等我结束录音。
核对安装版本和官方包校验值，打开 /Applications/Veyqa Voice.app。
已有 API Key 直接保留；缺少时引导我在 App 的凭证设置中自行填写。若当前版本提供“获取 API Key”和“检查连接”，可使用这些按钮；最后“保存并重启”。不要因旧版没有新按钮就重复安装或替换为未验收开发包。
不要让我在聊天里提供 Key，不要打印或上传 .env，不要把 Key 放入命令参数、截图、日志或 Git。
系统麦克风、辅助功能等权限由我亲自确认；不要禁用 Gatekeeper、移除隔离属性或代改系统权限数据库。
当前发布渠道是未公证测试版；若系统拦截，请说明限制并等我按 macOS 官方流程处理。
最后核对只有一个主程序，Dock 能打开设置，版本匹配；让我在临时文本框试一次语音输入，确认成功后再说安装完成。
关闭设置后确认设置进程退出，没有持续录音或异常 CPU 占用；不要把“进程还在”当作快捷键、识别、文字写入均正常的证据。
```

Agent 不需要接触你的新 Key。API 凭证由所选云服务计费；应用的“检查连接”只验证连接及语音服务权限，不会开启麦克风。

## 从源码运行

```bash
git clone https://github.com/wjc-dev/voxtype.git
cd voxtype
uv venv --python 3.13 .venv
uv pip install -r requirements-dev.txt
cp env.example .env          # 填入你的千问或豆包 API 凭证
.venv/bin/python main.py
```

需要 macOS 13+、Apple Silicon、Xcode Command Line Tools、Python 3.13。

## 构建 .dmg / .pkg

```bash
zsh ./build-internal-dmg.command
```

产物在 `dist-internal/`。

先运行不启动临时原生程序的源码检查：

```bash
.venv/bin/python -m pytest test/ --ignore=test/test_native_settings_core.py
```

完整隔离验收会执行完整测试、重新构建、检查签名与版本，并多次启动临时原生程序，验证 Dock 入口和设置窗口。它不会安装 App 或注册登录项，但可能触发办公电脑的终端安全拦截。不要反复放行、关闭或绕过防护；请先使用获准的测试环境。该流程默认拒绝启动，只有确认测试环境允许后才显式启用：

```bash
VEYQA_ALLOW_GUI_SMOKE_TESTS=true zsh ./tools/verify-release-candidate.command
```

## 项目结构

```
main.py            主程序
settings_ui.py     设置面板
src/
  audio/           麦克风采集
  transcription/   千问 + 豆包引擎
  keyboard/        全局快捷键、文本插入
  ui/              Dock/应用入口、浮窗
native_settings/   SwiftUI 原生设置 helper
packaging/         PyInstaller spec、图标
test/              单元测试
```

## 贡献

欢迎 Issue 和 PR。详见 [CONTRIBUTING.md](./CONTRIBUTING.md)。所有 PR 需通过 `.venv/bin/python -m pytest test/`。

## 安全

报告安全漏洞请看 [SECURITY.md](./SECURITY.md)。

## 许可证

[MIT](./LICENSE)。基于 [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) 和 [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li) 衍生,保留原始版权声明。
