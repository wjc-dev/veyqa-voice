# Veyqa Voice

**Lightweight voice input for Mac. Hold to speak, release to type.**

[Website](https://wjc-dev.github.io/veyqa-voice/) · [Download](https://github.com/wjc-dev/veyqa-voice/releases/tag/v0.3.0) · [简体中文](README.md)

[![Veyqa Voice demo](website/product-preview.svg)](https://wjc-dev.github.io/veyqa-voice/#demo)

- Works alongside your existing keyboard and input method.
- Transcribes your words into the focused input field; you decide when to send.
- Supports Qwen and Doubao. Add terms such as `user_id` and `seller_id` to improve recognition.
- Try the built-in service during the promotion without an account or API Key, or bring your own API credentials.

## Install

Requires **Apple Silicon and macOS 13+**.

1. [Download and open the DMG](https://github.com/wjc-dev/veyqa-voice/releases/download/v0.3.0/Veyqa-Voice-v0.3.0-macOS-arm64.dmg).
2. Follow the arrow: drag **Veyqa Voice into Applications**.
3. Open Veyqa Voice from Applications to begin setup.

For an unidentified-developer prompt, open System Settings → Privacy & Security → Open Anyway. [See Apple's guide](https://support.apple.com/en-us/102445).

Or install with Homebrew:

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

## Updating

Open Settings → General → App → Check for Updates, then download and relaunch. New versions are checked daily by default. Your API credentials and settings are retained; no uninstall is needed.

Version 0.2.8 and earlier need one manual upgrade using Homebrew or the DMG before in-app updates become available.

Version 0.3.0 moves the dictation engine to a native implementation while keeping the same bundle identifier, data directory, and signing identity. Your API credentials, shortcut, vocabulary, and system permissions carry over.

Alternatively, quit the app and update with Homebrew:

```bash
brew update
brew upgrade --cask wjc-dev/veyqa/veyqa-voice
```

For DMG installations, download the new installer above, drag the app into Applications and choose Replace. Follow the in-app guide if macOS asks you to confirm permissions.

## Set up once

1. Connect to the default built-in service; no account or API Key is required. You can also [use your own Key](https://wjc-dev.github.io/veyqa-voice/#api-key).
2. Grant microphone, Accessibility, and Input Monitoring permissions. Return to the app for automatic checks. Follow macOS prompts if it requests a relaunch.
3. Click the trial text field, hold **right Option** to speak, then release. Finish setup and use it in other apps.

Open settings from the menu bar or reopen the app from Applications. New installations stay out of the Dock; enable Dock visibility in General if preferred. Adjust the shortcut and hold/toggle mode, then save to apply. Updates preserve existing configuration.

## Speech services

The built-in service uses **Qwen Audio 3.1 ASR Message** (`qwen-audio-3.1-asr-flash-message`) with live transcription and text polishing off by default, paid for by Veyqa Voice during the promotion. Existing users keep their personal credentials and model when upgrading; switch to built-in service in Settings → Speech Service.

With your own API credentials, choose Qwen or Doubao models and refresh their official catalogues, or add an OpenAI-compatible provider. Qwen and Doubao stream live; custom providers transcribe when recording ends.

Audio goes directly to the selected speech service. Personal API credentials stay on your Mac and usage is billed to your provider account. Local audio archives are off by default.

## Source and build

Since 0.3.0 the dictation engine is the Rust implementation in this repository; the settings UI and supervisor are Swift. The Python sources for 0.2.18 and earlier remain on the [`python-0.2.18`](https://github.com/wjc-dev/veyqa-voice/tree/python-0.2.18) tag.

- `src/` — shortcut handling, audio capture, speech services, text cleanup, insertion and recovery.
- `native/` — Swift settings window, supervisor and update bridge.
- `config/` — bundled resources (model catalogues, icons, feature flags).
- `website/` — website and the Sparkle update feed.
- `scripts/` — build, packaging and measurement scripts.

```bash
# Development build: separate bundle id and data directory, coexists with an installation
./scripts/build-rust-macos.command

# Production build: same com.wjcdev.veyqa identity, then ZIP/DMG and update feed
VEYQA_DISTRIBUTION=public ./scripts/build-rust-macos.command
./scripts/package-public.command
```

Requires an Apple Silicon Mac, macOS 13+, Rust 1.85+ and Xcode Command Line Tools. The build expects a fixed signing certificate in your keychain; set `VEYQA_SIGNING_IDENTITY` to use a different certificate fingerprint. The update feed is signed with a Sparkle EdDSA key.

[Issues](https://github.com/wjc-dev/veyqa-voice/issues) · [Contributing](.github/CONTRIBUTING.md) · [Security](.github/SECURITY.md)

## License

[MIT](LICENSE). Derived from [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) and [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li), with original copyright notices retained.
