# Low-latency dictation startup implementation plan

**Goal:** Reduce visible and actual startup latency without persistent microphone access or removing chord cancellation.

**Architecture:** Keep disposable, timeout-bounded audio workers. Make their imports lean, share device selection without loading the recorder, and use raw float32 capture. Show session-scoped preparation feedback before microphone initialization. Right Option hold uses an 80 ms guard; other modifiers retain 180 ms and toggle gestures still commit on release.

**Tech stack:** Python, PortAudio/sounddevice, AppKit, PyInstaller, Sparkle, GitHub Pages, Homebrew.

**Spec:** User-approved startup optimization in this conversation. Execute inline; no additional approval ceremony.

## Constraints

- Preserve Key, model, shortcut mode, configuration and signing identity.
- Do not keep microphones open or add background polling.
- Cancel late chords and stale UI callbacks; never commit cancelled text.
- Retain device selection, failure isolation, signed updates and public built-in service.

## Tasks

- [ ] Add regression tests in `test/test_modifier_gesture.py`, `test/test_floating_preview.py`, `test/test_voice_session.py`, and `test/test_audio_worker_startup.py`. Run before implementation.
- [ ] Extract `select_input_device(sd, configured, policy)` and `capture_sample_rate(sd, device_idx, info)` to `src/audio/devices.py`. Make `src/audio/__init__.py` lazy. Worker uses `RawInputStream` and bytes rather than importing recorder/numpy/soundfile/logger. Preserve existing selection/fallback semantics; mock the raw callback and subprocess import boundary.
- [ ] Add an optional guard duration to `ModifierGesture`; pass 0.08 only for right Option. Existing chord, cancellation and release tests remain mandatory.
- [ ] Extend preview `show` with preparation and relevance arguments; main requests preparation before audio start and listening afterwards. No repeated fade on handoff; initial fade 60 ms. Record preparation and capture-ready timing without text/credentials.
- [ ] Run isolated Python tests excluding native executable harness, website tests, signing/build tests. Compare old/new packaged worker startup, measure real input and verify no microphone when idle.
- [ ] Build 0.2.18/232 using existing fixed identity and existing built-in payload. Verify signatures and preservation of configuration. Publish release assets, signed appcast, website download links and Homebrew cask. Check remote checksums, deployment, and `brew fetch`.

## Baseline

Installed 0.2.17/231 worker, quit command without accessing microphone, seven launches: 257.1, 147.2, 125.4, 145.0, 149.4, 150.4, 139.8 ms. Median 147.2 ms. These are process/import measurements, not complete dictation latency.
