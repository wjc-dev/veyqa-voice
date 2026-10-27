# 参与 Veyqa Voice 开发

## 开发环境

需要 Apple Silicon Mac、macOS 13+、Rust 1.85+ 和 Xcode Command Line Tools。

```bash
git clone https://github.com/wjc-dev/veyqa-voice.git
cd veyqa-voice
cargo test
./scripts/build-rust-macos.command
```

构建脚本会用固定证书签名，并要求该证书已在本机钥匙串中；可用 `VEYQA_SIGNING_IDENTITY` 指定其他证书的 SHA-1 指纹。开发包使用独立 bundle id `com.wjcdev.veyqa.rust` 和独立数据目录，可与已安装版同时运行。

## 修改与验证

```bash
cargo test
```

- 纯逻辑回归（快捷键手势、文本清洗、恢复、纠错、帧解析、重采样）在对应源文件的 `#[cfg(test)]` 中，修改后必须保持全绿。
- 快捷键与写入：修改后要在 TextEdit、浏览器输入框和聊天输入框中人工验证短按、长按、Esc 取消、切窗和连续两段。
- 音频：`cargo run -- --probe audio` 录音并输出 WAV 与 RMS。
- 语音服务：`cargo run -- --probe asr` 用 WAV 走完整的流式/转写流程。
- 设置页与 supervisor：修改 `native/` 后同样由 `build-rust-macos.command` 编译。

## 提交修改

不要直接在 `main` 上开发：

```bash
git switch -c feature/short-description
git add <明确的文件>
git commit -m "feat: describe the change"
git push -u origin feature/short-description
```

API Key、证书私钥、`private-trial.dat` 与任何本机凭证都不要提交到 Git 或附在 Issue 中。
