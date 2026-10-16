import Foundation
import Darwin

struct RecoveryEntry: Codable, Identifiable, Hashable {
    var id: String
    var text: String
    var reason: String
    var timestamp: String
}

enum RecoveryFile {
    /// Match Python RecoveryStore's stable sidecar lock; never rewrite a stale UI list.
    static func delete(ids: Set<String>, at url: URL) throws {
        guard !ids.isEmpty else { return }
        let directory = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let lockURL = directory.appendingPathComponent(".\(url.lastPathComponent).lock")
        let fd = Darwin.open(lockURL.path, O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { throw POSIXError(.EIO) }
        defer { Darwin.close(fd) }
        let deadline = ProcessInfo.processInfo.systemUptime + 2
        while flock(fd, LOCK_EX | LOCK_NB) != 0 {
            guard errno == EWOULDBLOCK else { throw POSIXError(.EIO) }
            guard ProcessInfo.processInfo.systemUptime < deadline else {
                throw NSError(domain: "RecoveryFile", code: 1,
                              userInfo: [NSLocalizedDescriptionKey: "恢复记录正在更新，请稍后重试。"])
            }
            Thread.sleep(forTimeInterval: 0.01)
        }
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        let current = try JSONDecoder().decode([RecoveryEntry].self, from: Data(contentsOf: url))
        let remaining = current.filter { !ids.contains($0.id) }
        guard remaining.count != current.count else { return }
        try SettingsEnvironment.privateWrite(JSONEncoder().encode(remaining), to: url)
    }
}

/// Avoid publishing identical on-disk snapshots back into SwiftUI every tick.
struct SnapshotChangeGate {
    private var previous: Data?
    mutating func changed(_ data: Data?) -> Bool {
        guard data != previous else { return false }
        previous = data
        return true
    }
}

enum QwenConnection {
    static func host(custom: String, region: String, workspace: String = "") -> String? {
        let value = custom.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.isEmpty {
            if !workspace.isEmpty {
                guard workspace.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }) else { return nil }
                return workspace + (region == "singapore" ? ".ap-southeast-1.maas.aliyuncs.com" : ".cn-beijing.maas.aliyuncs.com")
            }
            return region == "singapore" ? "dashscope-intl.aliyuncs.com" : "dashscope.aliyuncs.com"
        }
        guard let url = URL(string: value.contains("://") ? value : "https://" + value),
              url.scheme == "https", let host = url.host?.lowercased(),
              host.hasSuffix(".aliyuncs.com"), url.user == nil, url.password == nil,
              url.query == nil, url.fragment == nil, url.port == nil || url.port == 443
        else { return nil }
        return host
    }

    static func startRequest(taskID: String) -> [String: Any] {
        ["header": ["action": "run-task", "task_id": taskID, "streaming": "duplex"],
         "payload": ["task_group": "audio", "task": "asr", "function": "recognition",
                     "model": "qwen-audio-3.0-asr-flash-streaming",
                     "parameters": ["format": "pcm", "sample_rate": 16000], "input": [:]]]
    }
}

enum HotkeyConfig {
    static let defaultSpec = "keycode:49;mods:control+option"
    static let defaultLabel = "⌃⌥Space"
    static let modifierOnlySpecs: Set<String> = [
        "fn",
        "left_option", "right_option",
        "left_command", "right_command",
        "left_control", "right_control",
    ]
    static let registeredModifiers: Set<String> = [
        "control", "option", "shift", "command", "function",
    ]

    static func registeredSpec(keyCode: Int, modifiers: [String]) -> String? {
        guard (0...127).contains(keyCode), !modifiers.isEmpty else { return nil }
        let unique = Set(modifiers)
        guard unique.count == modifiers.count,
              unique.isSubset(of: registeredModifiers)
        else { return nil }
        return "keycode:\(keyCode);mods:\(modifiers.joined(separator: "+"))"
    }

    static func parseRegisteredSpec(_ spec: String) -> (keyCode: Int, modifiers: [String])? {
        var parts: [String: String] = [:]
        for raw in spec.split(separator: ";") {
            let pair = raw.split(separator: ":", maxSplits: 1).map(String.init)
            guard pair.count == 2, parts[pair[0]] == nil else { return nil }
            parts[pair[0]] = pair[1]
        }
        guard let keyCodeText = parts["keycode"],
              let keyCode = Int(keyCodeText),
              let modifierText = parts["mods"]
        else { return nil }
        let modifiers = modifierText.split(separator: "+").map(String.init)
        guard registeredSpec(keyCode: keyCode, modifiers: modifiers) == spec else {
            return nil
        }
        return (keyCode, modifiers)
    }

    static func isSupported(_ spec: String) -> Bool {
        modifierOnlySpecs.contains(spec) || parseRegisteredSpec(spec) != nil
    }

    static func backend(for spec: String) -> String {
        parseRegisteredSpec(spec) == nil ? "passive" : "registered"
    }
}

enum ModifierCaptureDecision: Equatable {
    case waiting
    case singleModifier(Int)
    case incompleteChord
}

struct ModifierCaptureAccumulator {
    private var pressedKeyCodes: Set<Int> = []
    private var firstKeyCode: Int?
    private var sawMultipleModifiers = false

    mutating func modifierChanged(
        keyCode: Int,
        aggregateFlagIsSet: Bool
    ) -> ModifierCaptureDecision {
        if aggregateFlagIsSet && !pressedKeyCodes.contains(keyCode) {
            pressedKeyCodes.insert(keyCode)
            if firstKeyCode == nil { firstKeyCode = keyCode }
            if pressedKeyCodes.count > 1 { sawMultipleModifiers = true }
            return .waiting
        }

        if pressedKeyCodes.contains(keyCode) {
            pressedKeyCodes.remove(keyCode)
        }
        guard pressedKeyCodes.isEmpty else { return .waiting }

        defer { reset() }
        guard let firstKeyCode else { return .waiting }
        return sawMultipleModifiers
            ? .incompleteChord
            : .singleModifier(firstKeyCode)
    }

    mutating func reset() {
        pressedKeyCodes.removeAll()
        firstKeyCode = nil
        sawMultipleModifiers = false
    }
}

enum SettingsEnvironment {
    static func read(_ url: URL) -> [String: String] {
        guard let text = try? String(contentsOf: url, encoding: .utf8) else { return [:] }
        var result: [String: String] = [:]
        for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.hasPrefix("#"), let equal = line.firstIndex(of: "=") else {
                continue
            }
            let key = line[..<equal].trimmingCharacters(in: .whitespaces)
            result[key] = String(line[line.index(after: equal)...])
        }
        return result
    }

    static func update(_ url: URL, updates: [String: String]) throws {
        let existing = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        var pending = updates
        var lines: [String] = []
        for line in existing.split(
            separator: "\n", omittingEmptySubsequences: false
        ).map(String.init) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if !trimmed.hasPrefix("#"), let equal = line.firstIndex(of: "=") {
                let key = line[..<equal].trimmingCharacters(in: .whitespaces)
                if let value = pending.removeValue(forKey: key) {
                    lines.append("\(key)=\(value)")
                    continue
                }
            }
            lines.append(line)
        }
        if lines.last != "" { lines.append("") }
        for key in pending.keys.sorted() { lines.append("\(key)=\(pending[key]!)") }
        try privateWrite(Data((lines.joined(separator: "\n") + "\n").utf8), to: url)
    }

    static func privateWrite(_ data: Data, to url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        try data.write(to: url, options: .atomic)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o600], ofItemAtPath: url.path
        )
    }
}
