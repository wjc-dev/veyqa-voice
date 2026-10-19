# Veyqa Voice

**Lightweight voice input for Mac. Hold to speak, release to type.**

[Website](https://wjc-dev.github.io/voxtype/) · [Download](https://github.com/wjc-dev/voxtype/releases/tag/v0.2.7) · [简体中文](README.md)

[![Veyqa Voice demo](website/product-preview.svg)](https://wjc-dev.github.io/voxtype/#demo)

- Works alongside your existing keyboard and input method.
- Transcribes your words into the focused input field; you decide when to send.
- Supports Qwen and Doubao. Add terms such as `user_id` and `seller_id` to improve recognition.

## Install

Requires **Apple Silicon and macOS 13+**.

1. [Download and open the DMG](https://github.com/wjc-dev/voxtype/releases/download/v0.2.7/Veyqa-Voice-v0.2.7-macOS-arm64.dmg).
2. Follow the arrow: drag **Veyqa Voice into Applications**.
3. Open Veyqa Voice from Applications to begin setup.

This Preview is not yet Apple-notarized. For an unidentified-developer prompt, see the [first-open guide](https://wjc-dev.github.io/voxtype/#safety).

<details><summary>Install with Homebrew</summary>

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

</details>

## Set up once

1. Paste your own speech-service API Key and connect. Qwen is selected by default; [Key setup guide](https://wjc-dev.github.io/voxtype/#api-key).
2. Grant microphone, Accessibility, and Input Monitoring permissions. Return to the app for automatic checks. Follow macOS prompts if it requests a relaunch.
3. Click the trial text field, hold **right Option** to speak, then release. Finish setup and use it in other apps.

Open settings from the menu bar or Dock. Adjust the shortcut, hold/toggle mode, and menu-bar visibility; save to apply. Updates preserve existing configuration.

## Speech services

The default **Qwen Audio 3.0 ASR Streaming** engine offers streaming recognition, multilingual and dialect support, and hotword enhancement. Doubao is also available.

Your API Key stays on your Mac. Audio goes directly to the selected speech service, which bills your account for API usage. Local audio archives are off by default.

[Issues](https://github.com/wjc-dev/voxtype/issues) · [Development](docs/development.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## License

[MIT](LICENSE). Derived from [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) and [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li), with original copyright notices retained.
