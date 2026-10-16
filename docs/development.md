# 开发与维护

本文保留原 README 中的实现、调试与构建信息。用户安装和使用请看 [README](../README.md)，历史测试证据见 [验证记录](release-candidate-verification.md)。

## 本地开发

需要 Apple Silicon、macOS 13+、Xcode Command Line Tools 和 Python 3.13。

```bash
git clone https://github.com/wjc-dev/voxtype.git
cd voxtype
uv venv --python 3.13 .venv
uv pip install -r requirements-dev.txt
cp env.example .env
.venv/bin/python main.py
```

API Key 在本地自行填写，不进入聊天、Git、命令参数或日志。打包版数据位于 `~/Library/Application Support/Veyqa`；测试使用隔离的 `VOICE_INPUT_DATA_DIR`。

## 默认值与升级

0.2.3 新安装使用 `right_option` / `hold` / `passive`。首次需要输入监控；组合键自动使用 registered 后端，不需要输入监控。既有 `.env` 不被模板覆盖；缺少快捷键字段的旧配置保留旧组合键默认语义。Swift 设置和 Python 主程序的默认值、标签与权限需求必须同步测试。

## 后台运行与故障恢复

- macOS Service Management 注册轻量 supervisor。意外退出会恢复；60 秒内三次失败后冷却五分钟，避免重启循环。主动退出则暂停恢复，手动打开恢复服务。
- 快捷键回调只入队，不执行可能阻塞的录音调用；监听恢复时重置锁存并校正松键。取消不被阻塞队列困住。
- 分别检查录音启动、识别收尾、监听器和事件循环；原生守护检查功能心跳。静默不按键不是故障。
- 更新或重装刷新登录项注册。固定本机签名使用独立 local 服务标识，正常注销旧服务，不全局重置系统后台项目。
- Dock 是默认入口，设置 helper 单实例；菜单栏图标默认关闭。后台启动不应抢焦点或反复弹设置。

## 快速输入与文字恢复

保持流式 ASR → 本地规则 → 最终写入，不增加默认润色调用、不自动发送。独立听写即使内容相同也分别写入；只在同一会话拦截重复最终回调。

输入目标无法确认时不盲目粘贴；异常结束或可控重启前，尝试保存通过安全检查的已识别片段。恢复最多保留五条，跨进程文件锁保护读改写，删除按 ID；尚未识别的音频、强杀前未保存内容或存储故障不保证可恢复。取消内容不保存，可选录音存档失败不阻止文字交付。

## 开发签名与权限

ad-hoc 身份随构建变化，仅固定名称、Bundle ID 或路径不能保证复用授权。参考 [Apple 对签名要求的说明](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)。

- 本机可用用户批准的专用自签名代码签名证书。其 SHA-1 指纹保存在 `~/Library/Application Support/Veyqa Build/signing-identity.sha1`；私钥留在钥匙串，不提交或上传。构建身份缺失时失败，不自动降级。
- 默认复用本机身份，产物在 `dist-local/`，带 `-local` 后缀；未配置身份则输出 `dist-internal/` ad-hoc 包。
- 显式公开测试构建：`VEYQA_SIGNING_IDENTITY=- zsh build-internal-dmg.command`。它不修改保存的身份，但不可直接覆盖固定签名的日用 App。
- `zsh build-internal-dmg.command` 生成 DMG、ZIP、PKG。当前流程不是 Developer ID / Installer 签名与公证流程。
- 首次切换身份可能需授权。只请求缺失的必要权限，不修改 TCC 数据库、不移除隔离属性、不关闭系统或终端防护。固定身份也不保证所有系统永不重新询问。

## 空闲占用

快捷键走系统事件，不高频扫描。空闲不录音、不保持云端识别会话；关闭设置即释放设置进程，未变化的快照不重绘。后台权限通常每 30 秒检查，设置打开时约两秒；健康检查每两秒、心跳约十秒，无变化不写盘。

```bash
.venv/bin/python tools/measure_idle.py 主进程PID 守护进程PID --seconds 60
```

关闭设置和录音后测量。CPU 是单核口径，RSS 不等于完整物理内存影响；机型和系统负载会影响结果。

## 测试与发布

源码测试应设置一个全新的临时 `VOICE_INPUT_DATA_DIR`：

```bash
.venv/bin/python -m pytest test/ --ignore=test/test_native_settings_core.py -q
node --test test/test_website.mjs
```

Swift 类型检查/编译不等于运行测试。完整验收会启动临时原生程序，可能触发办公电脑安全拦截；仅在获准环境显式运行 `VEYQA_SIGNING_IDENTITY=- VEYQA_ALLOW_GUI_SMOKE_TESTS=true zsh tools/verify-release-candidate.command`。不要反复放行或绕过防护；本机固定签名包只在固定安装路径验收。

发布使用新版本号，不覆盖旧 Release。核对远端附件 SHA-256 后同步 Homebrew cask，并用 `brew fetch --cask --force wjc-dev/veyqa/veyqa-voice` 验证下载。不用公开包覆盖开发者固定签名安装。下载或进程存在不等于实际听写成功，发布说明应区分自动、真实输入及多机测试范围。

## 官网

`website/` 是无构建依赖的静态站，GitHub Actions 只上传该目录至 Pages。修改前后验证复制成功/拒绝分支、键盘导航、桌面与手机布局、链接和版本。无需服务器、用户账户、追踪脚本或 Key 表单。

预览：`.venv/bin/python -m http.server 8765 --bind 127.0.0.1 --directory website`。

## 代码入口

`main.py` 会话与生命周期；`src/audio/` 录音；`src/transcription/` ASR；`src/keyboard/` 快捷键与安全写入；`src/ui/` Dock 与浮窗；`native_settings/` SwiftUI 设置和原生守护；`packaging/` 构建资源；`test/` 回归测试。
