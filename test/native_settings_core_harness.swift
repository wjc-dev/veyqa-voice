import Darwin
import Foundation

private func require(_ condition: @autoclosure () -> Bool, _ message: String) {
    guard condition() else {
        FileHandle.standardError.write(Data("FAILED: \(message)\n".utf8))
        exit(1)
    }
}

@main
struct SettingsCoreHarness {
    static func main() throws {
        require(SetupStep.current(connectionApplied: false, permissionsGranted: true) == .connection, "permission alone does not complete setup")
        require(SetupStep.current(connectionApplied: true, permissionsGranted: false) == .permissions, "connection advances to permission")
        require(SetupStep.current(connectionApplied: true, permissionsGranted: true) == .trial, "ready for real dictation trial")
        require(SetupStep.instruction(hotkey: "右 Option", mode: "hold").contains("按住"), "hold instructions")
        require(SetupStep.instruction(hotkey: "自定义按键", mode: "toggle").contains("再按一下"), "existing toggle mode retained")
        var snapshotGate = SnapshotChangeGate()
        require(!snapshotGate.changed(nil), "missing snapshot stays quiet")
        require(snapshotGate.changed(Data("first".utf8)), "first snapshot updates")
        require(!snapshotGate.changed(Data("first".utf8)), "unchanged snapshot stays quiet")
        require(snapshotGate.changed(Data("next".utf8)), "changed snapshot updates")
        require(snapshotGate.changed(nil), "deleted snapshot resets")
        require(QwenConnection.host(custom: "", region: "beijing") == "dashscope.aliyuncs.com", "key-only defaults")
        let messageModel = "qwen-audio-3.1-asr-flash-message"
        let request = QwenConnection.startRequest(taskID: "probe", model: messageModel)
        let requestPayload = request["payload"] as! [String: Any]
        require(requestPayload["model"] as? String == messageModel, "connection check uses selected model")
        let parameters = requestPayload["parameters"] as! [String: Any]
        require(parameters["disfluency_removal_enabled"] as? Bool == false, "message polishing remains off")
        require(parameters["intermediate_result_enabled"] as? Bool == true, "message preview enabled")
        require(SpeechModels.compatible("qwen-audio-9.2-asr-flash-streaming", provider: "qwen"), "future same-protocol version")
        require(!SpeechModels.compatible("qwen-audio-3.1-asr-flash-filetrans", provider: "qwen"), "file ASR excluded")
        let discovered = SpeechModels.fromOfficialText("qwen-audio-3.1-asr-flash-streaming qwen-audio-3.1-asr-flash-streaming qwen-audio-3.1-asr-flash-message", provider: "qwen")
        require(discovered.count == 2, "official discovery deduplicates compatible models")
        let api = SpeechModels.fromAPI(["output": ["total": 2, "models": [["model": messageModel], ["model": "qwen-image"]]]])
        require(api?.models.count == 1 && api?.total == 2, "API discovery filters protocol and preserves pagination total")
        require(QwenConnection.host(custom: "", region: "singapore") == "dashscope-intl.aliyuncs.com", "region honored")
        require(QwenConnection.host(custom: "https://example.com", region: "beijing") == nil, "untrusted host rejected")
        require(QwenConnection.host(custom: "https://dashscope.aliyuncs.com@evil.example", region: "beijing") == nil, "userinfo rejected")
        require(QwenConnection.host(custom: "https://workspace.cn-beijing.maas.aliyuncs.com/compatible-mode/v1", region: "beijing") == "workspace.cn-beijing.maas.aliyuncs.com", "existing custom endpoint preserved")
        var health = SupervisorHealth()
        require(!health.needsRecovery(pid: 5, heartbeatPID: nil, timestamp: nil, now: 100), "startup grace")
        for tick in stride(from: 105.0, through: 190.0, by: 5.0) {
            require(!health.needsRecovery(pid: 5, heartbeatPID: 5, timestamp: tick, now: tick), "healthy idle")
        }
        require(health.needsRecovery(pid: 5, heartbeatPID: 5, timestamp: 100, now: 195), "stale heartbeat detected")
        require(!health.needsRecovery(pid: 5, heartbeatPID: 5, timestamp: 100, now: 4000), "wake grace")
        require(!health.needsRecovery(pid: 6, heartbeatPID: 5, timestamp: 100, now: 4005), "new pid grace")
        let first = HotkeyConfig.registeredSpec(
            keyCode: 49, modifiers: ["control", "option"]
        )
        let second = HotkeyConfig.registeredSpec(
            keyCode: 40, modifiers: ["command", "shift"]
        )
        require(first == "keycode:49;mods:control+option", "first combo encoding")
        require(second == "keycode:40;mods:command+shift", "second combo encoding")
        require(HotkeyConfig.backend(for: first!) == "registered", "combo backend")
        require(HotkeyConfig.backend(for: "right_option") == "passive", "preset backend")
        require(HotkeyConfig.isSupported("right_option"), "modifier-only preset")
        require(HotkeyConfig.defaultSpec == "right_option", "new user right Option default")
        require(HotkeyConfig.missingSpec(hasExistingConfig: false) == "right_option", "new config fallback")
        require(HotkeyConfig.missingSpec(hasExistingConfig: true) == first, "legacy missing key preserved")
        require(
            HotkeyConfig.registeredSpec(keyCode: 49, modifiers: []) == nil,
            "missing modifier rejected"
        )
        require(
            HotkeyConfig.registeredSpec(keyCode: 49, modifiers: ["hyper"]) == nil,
            "unknown modifier rejected"
        )
        require(
            HotkeyConfig.parseRegisteredSpec(
                "keycode:49;keycode:40;mods:control"
            ) == nil,
            "duplicate fields rejected without crashing"
        )

        var modifierCapture = ModifierCaptureAccumulator()
        require(
            modifierCapture.modifierChanged(keyCode: 59, aggregateFlagIsSet: true)
                == .waiting,
            "first modifier waits for a possible chord"
        )
        require(
            modifierCapture.modifierChanged(keyCode: 58, aggregateFlagIsSet: true)
                == .waiting,
            "second modifier still waits for the ordinary key"
        )
        modifierCapture.reset()
        require(
            modifierCapture.modifierChanged(keyCode: 54, aggregateFlagIsSet: true)
                == .waiting,
            "single modifier waits until release"
        )
        require(
            modifierCapture.modifierChanged(keyCode: 54, aggregateFlagIsSet: false)
                == .singleModifier(54),
            "single modifier is captured on release"
        )
        require(
            modifierCapture.modifierChanged(keyCode: 59, aggregateFlagIsSet: true)
                == .waiting,
            "incomplete chord first press"
        )
        require(
            modifierCapture.modifierChanged(keyCode: 58, aggregateFlagIsSet: true)
                == .waiting,
            "incomplete chord second press"
        )
        require(
            modifierCapture.modifierChanged(keyCode: 59, aggregateFlagIsSet: false)
                == .waiting,
            "incomplete chord waits for all modifiers to release"
        )
        require(
            modifierCapture.modifierChanged(keyCode: 58, aggregateFlagIsSet: false)
                == .incompleteChord,
            "modifier-only chord is rejected without stealing the first key"
        )

        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "voxtype-settings-core-\(UUID().uuidString)", isDirectory: true
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        let envURL = directory.appendingPathComponent(".env")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let recoveryURL = directory.appendingPathComponent("recovery.json")
        let entries = [
            RecoveryEntry(id: "new", text: "刚刚恢复", reason: "失败", timestamp: "now"),
            RecoveryEntry(id: "old", text: "旧文字", reason: "失败", timestamp: "before"),
        ]
        try SettingsEnvironment.privateWrite(JSONEncoder().encode(entries), to: recoveryURL)
        try RecoveryFile.delete(ids: ["old"], at: recoveryURL)
        let kept = try JSONDecoder().decode([RecoveryEntry].self, from: Data(contentsOf: recoveryURL))
        require(kept.map(\.id) == ["new"], "stale selection preserves unseen recovery")
        let recoveryLock = directory.appendingPathComponent(".recovery.json.lock")
        require(FileManager.default.fileExists(atPath: recoveryLock.path), "shared Python/Swift lock path")
        try Data("# preserve this comment\nVOICE_HOTKEY=right_option\n".utf8).write(to: envURL)

        try SettingsEnvironment.update(envURL, updates: [
            "VOICE_HOTKEY": second!,
            "VOICE_HOTKEY_LABEL": "⌘⇧K",
            "GLOBAL_HOTKEY_BACKEND": HotkeyConfig.backend(for: second!),
        ])
        let saved = SettingsEnvironment.read(envURL)
        require(saved["VOICE_HOTKEY"] == second, "custom combo persisted")
        require(saved["VOICE_HOTKEY_LABEL"] == "⌘⇧K", "custom label persisted")
        require(saved["GLOBAL_HOTKEY_BACKEND"] == "registered", "backend persisted")
        let text = try String(contentsOf: envURL, encoding: .utf8)
        require(text.contains("# preserve this comment"), "comments preserved")
        let attributes = try FileManager.default.attributesOfItem(atPath: envURL.path)
        let permissions = (attributes[.posixPermissions] as? NSNumber)?.intValue ?? 0
        require(permissions & 0o777 == 0o600, "saved environment remains private")
    }
}
