# Veyqa Voice

> Status (2026-10-16): the source tree is a **0.2.2 development candidate**.
> Homebrew still installs **0.2.0**, without these new fixes. Final package
> validation, Developer ID signing, and notarization are not complete.

Stability update: bounded automatic recovery for stalled recording/recognition,
functional heartbeats, and hotkey release reconciliation. Qwen setup now needs
only an API key by default; advanced endpoints remain optional. Audio is sent
to your selected cloud ASR provider. Local recording archives are off by default.
If the input target cannot be verified, results remain in Recovery for manual copy.
The current public channel is an ad-hoc signed, unnotarized beta.

For local-agent installation instructions, see the Chinese README's
“让本地 Agent 帮你安装” section. Never paste API keys into an agent conversation.

English · [简体中文](./README.md)

Push-to-talk voice dictation for macOS. Hold a hotkey, speak, release — text lands at the caret.

It does not replace your system input method. Pinyin, Wubi, or Rime keep working; voice is one keystroke away when you need it.

## Why

- Love Sogou / Rime / Wubi but occasionally want voice — don't switch IMEs just to get dictation
- Bring your own credentials for Qwen Audio 3.0 ASR or Doubao Seed ASR 2.0
- Hold to talk, release to commit. No marked-text underline, no flow disruption
- Floating preview anchored at the bottom-center, never steals focus
- Dock-resident, with a native SwiftUI settings window one click away
- Audio is sent to your selected cloud ASR provider; local archives are off by default and logs do not capture recognized content
- MIT licensed, derived from ErlichLiu's Whisper-Input

## Install

With Homebrew:

```bash
brew install --cask wjc-dev/veyqa/veyqa-voice
```

Alternatively, grab the latest Veyqa Voice installer and drag Veyqa Voice to Applications. On first launch grant Microphone and Accessibility permissions as prompted.

Requires macOS 13+ on Apple Silicon.

## Configure

The simplified interface below is part of the 0.2.2 development candidate.
Homebrew 0.2.0 still uses the original provider and credential settings.

1. Open Veyqa Voice. Qwen is the default: get an API key, paste it, then click Check Connection. Region and custom endpoints are optional advanced settings; existing settings are preserved. Doubao is still available.
2. The default hotkey is `⌃⌥Space`. macOS registers this exact combination, so it does not need Input Monitoring permission and can report a registration conflict. Settings can record other modifier-plus-key combinations.
3. Save and restart. Focus any input field, hold the hotkey, speak, release.

Left/right Option, Command, Control, or Fn can also be used alone. Modifier-only shortcuts require a read-only keyboard monitor and Input Monitoring permission, and are more likely to conflict with macOS, remote-desktop tools, Karabiner, or managed-device security software. Prefer a combination for general reliability.

## Background reliability and recovery

- Packaged builds register a small supervisor through the macOS 13+ Service Management API. It relaunches a missing Veyqa Voice process, but cools down for five minutes after three failures in 60 seconds to prevent a crash loop.
- A Quartz shortcut listener is health-checked and re-enabled when macOS disables it. Managed Macs that reject the read-only event tap fall back to an AppKit compatibility monitor.
- Stalled recording startup and recognition finalization trigger bounded recovery. The native supervisor also checks a process heartbeat; upgrading or reinstalling refreshes stale login-item registrations.
- The Dock icon is the stable primary entry point. Clicking it raises the existing settings window without starting a second voice service. The legacy menu-bar item is off by default so macOS cannot hide the app's only entry point.
- The Recovery & Diagnostics page reports login-item status, permissions, shortcut backend, and the latest session. A running legacy Voice Input process is reported during migration so two shortcut listeners are not left active silently.

Public distribution still requires a stable Developer ID signature for the App/PKG and Apple notarization. The repository's `internal` artifacts are ad-hoc-signed test builds, not a universal public release.

Idle use: closing Settings releases its process. Unchanged snapshots do not
redraw the UI or rewrite permission files. Background permission checks slow to
30 seconds (about 2 seconds with Settings open); health checks stay at 2 seconds,
with an atomic, non-fsync heartbeat about every 10 seconds. No microphone or
cloud-recognition session remains open while idle. Use `tools/measure_idle.py`
with explicit PIDs to measure one-core CPU percentage and RSS; hardware and OS
conditions affect results.

## Run from source

```bash
git clone https://github.com/wjc-dev/voxtype.git
cd voxtype
uv venv --python 3.13 .venv
uv pip install -r requirements-dev.txt
cp env.example .env          # fill in your Qwen or Doubao credentials
.venv/bin/python main.py
```

Requires macOS 13+, Apple Silicon, Xcode Command Line Tools, Python 3.13.

## Build .dmg / .pkg

```bash
zsh ./build-internal-dmg.command
```

Output in `dist-internal/`.

For source-only checks, run `.venv/bin/python -m pytest test/ --ignore=test/test_native_settings_core.py`.
The full verifier below launches temporary ad-hoc native executables and may trigger endpoint-security alerts on managed Macs. It is opt-in: use an approved test environment, never disable or bypass protection. It rebuilds, validates signatures/versions, and smoke-tests the Dock and Settings without installing the app or registering a login item.

```bash
VEYQA_ALLOW_GUI_SMOKE_TESTS=true zsh ./tools/verify-release-candidate.command
```

## Project layout

```
main.py            Entry point
settings_ui.py     Settings panel
src/
  audio/           Microphone capture
  transcription/   Qwen + Doubao engines
  keyboard/        Global hotkeys, text insertion
  ui/              Dock/app entry, floating preview
native_settings/   SwiftUI native settings helper
packaging/         PyInstaller spec, icons
test/              Unit tests
```

## Contributing

Issues and PRs welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md). PRs must pass `.venv/bin/python -m pytest test/`.

## Security

Report vulnerabilities per [SECURITY.md](./SECURITY.md).

## License

[MIT](./LICENSE). Derived from [ErlichLiu/Whisper-Input](https://github.com/ErlichLiu) and [Mor-Li/Whisper-Input-Next](https://github.com/Mor-Li); original copyright notices preserved.
