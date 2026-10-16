# Veyqa Voice

**Say it. Type it.** A voice input key for your Mac.

[Website](https://wjc-dev.github.io/voxtype/) · [Download](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.4) · [简体中文](README.md)

- Hold right Option to speak. Release to insert text — never auto-send.
- Keep your input method. No AI rewriting by default.
- Choose Qwen or Doubao with your own API credentials.

## Install

Requires **Apple Silicon and macOS 13+**.

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

Or download the app and drag it to Applications. A local AI agent can help: [copy the setup prompt on our website](https://wjc-dev.github.io/voxtype/#agent).

## Start dictating

1. Open the app. Qwen is the default: get an API key, enter it yourself, and check the connection. Doubao is also available.
2. Approve **Microphone, Accessibility, and Input Monitoring** when prompted, then save and restart.
3. Focus a text field, **hold right Option, speak, then release**. Click the Dock icon to open Settings.

Shortcuts and hold/toggle mode are customizable. New installs default to right Option; updates preserve credentials and settings. Credential fields include show/hide and copy buttons.

## Before you start

**0.2.4 Preview** is not Apple-notarized. If macOS blocks the first launch because the developer cannot be verified or the app is not notarized, first confirm that the download is official and that you trust its source. You may then choose **System Settings → Privacy & Security → Open Anyway**, and confirm **Open** yourself. This is an app-specific exception, not a reason to disable system protection. [Apple's guidance](https://support.apple.com/en-us/102445)

Stop and report the issue if macOS warns of malware or damage, or if company security software blocks the app. Follow your administrator's policy; do not force it to run.

Audio is sent to your selected cloud ASR provider, which bills API usage separately. Local audio archives are off by default. Never share API keys in chat or with an agent.

[Feedback](https://github.com/wjc-dev/voxtype/issues) · [Developer guide](docs/development.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## License

[MIT](LICENSE). Derived from [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) and [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li); original copyright notices are preserved.
