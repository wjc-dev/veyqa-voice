import AppKit
import Darwin
import Foundation
import SwiftUI

private func isSettingsWindow(_ window: NSWindow) -> Bool {
    window.title == "语音输入设置" && window.styleMask.contains(.titled)
}

private func raiseSettingsWindows() {
    // The settings helper is an accessory of the menu-bar app, not a second
    // standalone application.  Keeping accessory policy prevents the raw
    // helper executable from creating a generic white icon in the Dock while
    // still allowing its settings window to become key and frontmost.
    NSApp.setActivationPolicy(.accessory)
    NSApp.unhide(nil)
    NSApp.activate(ignoringOtherApps: true)
    let windows = NSApp.windows.filter(isSettingsWindow)
    if windows.isEmpty {
        SettingsAppDelegate.openSettingsWindow?()
        return
    }
    for window in windows {
        // The helper owns only one transient settings window.  Disabling
        // minimization prevents macOS from leaving generic executable/window
        // tiles in the Dock when the user is finished configuring the app.
        window.styleMask.remove(.miniaturizable)
        window.standardWindowButton(.miniaturizeButton)?.isHidden = true
        window.deminiaturize(nil)
        window.makeKeyAndOrderFront(nil)
        window.orderFrontRegardless()
    }
}

final class SettingsAppDelegate: NSObject, NSApplicationDelegate {
    static var reopeningApp = false
    static var openSettingsWindow: (() -> Void)?
    private var raiseSignal: DispatchSourceSignal?
    private var closeObserver: NSObjectProtocol?
    private var instanceLockURL: URL?
    private var shortcutCaptureURL: URL?
    private var updateStateURL: URL?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        let environment = ProcessInfo.processInfo.environment
        let root = environment["VOICE_INPUT_DATA_ROOT"]
            ?? environment["VOICE_INPUT_ROOT"]
            ?? FileManager.default.currentDirectoryPath
        Darwin.signal(SIGUSR1, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        source.setEventHandler { raiseSettingsWindows() }
        source.resume()
        raiseSignal = source
        closeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: nil, queue: .main
        ) { notification in
            guard let window = notification.object as? NSWindow,
                  isSettingsWindow(window), !Self.reopeningApp else { return }
            // SwiftUI can retain invisible utility windows. They must not keep
            // a windowless settings helper alive after the real window closes.
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }

        let dataURL = URL(fileURLWithPath: root, isDirectory: true)
        updateStateURL = dataURL.appendingPathComponent(".settings-update-state.json")
        shortcutCaptureURL = dataURL.appendingPathComponent(".shortcut-capture")
        let instanceURL = dataURL.appendingPathComponent(".settings-instance")
        if acquireInstanceLock(instanceURL) {
            instanceLockURL = instanceURL
        } else {
            if let pidText = try? String(contentsOf: instanceURL, encoding: .utf8),
               let pid = Int32(pidText.trimmingCharacters(in: .whitespacesAndNewlines)),
               pid > 1 {
                Darwin.kill(pid, SIGUSR1)
            }
            DispatchQueue.main.async { NSApp.terminate(nil) }
            return
        }
        DispatchQueue.main.async { raiseSettingsWindows() }
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let closeObserver { NotificationCenter.default.removeObserver(closeObserver) }
        removeOwnedLock(instanceLockURL)
        removeOwnedLock(shortcutCaptureURL)
        if let url = updateStateURL,
           let data = try? Data(contentsOf: url),
           let state = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           state["pid"] as? Int32 == getpid() {
            try? FileManager.default.removeItem(at: url)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        !Self.reopeningApp
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        Self.reopeningApp ? .terminateCancel : .terminateNow
    }

    private func acquireInstanceLock(_ url: URL) -> Bool {
        for _ in 0..<2 {
            let fd = Darwin.open(url.path, O_WRONLY | O_CREAT | O_EXCL, S_IRUSR | S_IWUSR)
            if fd >= 0 {
                let pidBytes = Array(String(getpid()).utf8)
                pidBytes.withUnsafeBytes { buffer in
                    _ = Darwin.write(fd, buffer.baseAddress, buffer.count)
                }
                Darwin.close(fd)
                return true
            }
            guard let pidText = try? String(contentsOf: url, encoding: .utf8),
                  let pid = Int32(pidText.trimmingCharacters(in: .whitespacesAndNewlines))
            else {
                try? FileManager.default.removeItem(at: url)
                continue
            }
            if Darwin.kill(pid, 0) == 0 { return false }
            try? FileManager.default.removeItem(at: url)
        }
        return false
    }

    private func removeOwnedLock(_ url: URL?) {
        guard let url,
              let contents = try? String(contentsOf: url, encoding: .utf8),
              contents.trimmingCharacters(in: .whitespacesAndNewlines) == String(getpid())
        else { return }
        try? FileManager.default.removeItem(at: url)
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        DispatchQueue.main.async { raiseSettingsWindows() }
    }

    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows flag: Bool
    ) -> Bool {
        DispatchQueue.main.async { raiseSettingsWindows() }
        return true
    }
}

struct CorrectionRule: Codable, Identifiable, Hashable {
    var wrong: String
    var correct: String
    var count: Int
    var firstSeen: String?
    var lastSeen: String?
    var enabled: Bool?

    var id: String { wrong + "\u{0}" + correct }

    enum CodingKeys: String, CodingKey {
        case wrong, correct, count, enabled
        case firstSeen = "first_seen"
        case lastSeen = "last_seen"
    }
}

struct CorrectionFile: Codable {
    var version: Int
    var rules: [CorrectionRule]
    var events: [CorrectionEvent]?
}

struct CorrectionEvent: Codable {
    var wrong: String
    var correct: String
    var timestamp: String
}

struct DiagnosticSnapshot: Codable {
    var version: String?
    var engine: String?
    var model: String?
    var state: String?
    var microphone: String?
    var sampleRate: Int?
    var hotkey: String?
    var hotkeyBackend: String?
    var archiveEnabled: Bool?
    var targetLocked: Bool?
    var lastError: String?
    var updatedAt: String?
    var lastSessionOutcome: String?
    var lastSessionAudioMs: Int?
    var lastSessionVoicedMs: Int?
    var lastSessionPreviewCount: Int?
    var lastSessionFirstPreviewMs: Int?
    var lastSessionCommitted: Bool?
    var loginItemStatus: String?
    var loginItemError: String?
    var recoveryStatus: String?
    var settingsRevision: String?
    var settingsStatus: String?
    var microphoneTestStatus: String?

    enum CodingKeys: String, CodingKey {
        case version, engine, model, state, microphone, hotkey
        case hotkeyBackend = "hotkey_backend"
        case sampleRate = "sample_rate"
        case archiveEnabled = "archive_enabled"
        case targetLocked = "target_locked"
        case lastError = "last_error"
        case updatedAt = "updated_at"
        case lastSessionOutcome = "last_session_outcome"
        case lastSessionAudioMs = "last_session_audio_ms"
        case lastSessionVoicedMs = "last_session_voiced_ms"
        case lastSessionPreviewCount = "last_session_preview_count"
        case lastSessionFirstPreviewMs = "last_session_first_preview_ms"
        case lastSessionCommitted = "last_session_committed"
        case loginItemStatus = "login_item_status"
        case loginItemError = "login_item_error"
        case recoveryStatus = "recovery_status"
        case settingsRevision = "settings_revision"
        case settingsStatus = "settings_status"
        case microphoneTestStatus = "microphone_test_status"
    }
}

struct PermissionSnapshot: Codable {
    var version: String?
    var bundleIdentifier: String?
    var bundlePath: String?
    var executablePath: String?
    var executableFingerprint: String?
    var pid: Int?
    var microphone: String?
    var accessibility: String?
    var inputMonitoring: String?
    var inputMonitoringRequired: Bool?
    var allRequiredGranted: Bool?
    var hotkeyReady: Bool?
    var checkedByCurrentProcess: Bool?
    var updatedAt: String?

    enum CodingKeys: String, CodingKey {
        case version, pid, microphone, accessibility
        case bundleIdentifier = "bundle_identifier"
        case bundlePath = "bundle_path"
        case executablePath = "executable_path"
        case executableFingerprint = "executable_fingerprint"
        case inputMonitoring = "input_monitoring"
        case inputMonitoringRequired = "input_monitoring_required"
        case allRequiredGranted = "all_required_granted"
        case hotkeyReady = "hotkey_ready"
        case checkedByCurrentProcess = "checked_by_current_process"
        case updatedAt = "updated_at"
    }
}

struct PermissionRequest: Codable {
    var permission: String
    var openSettings: Bool

    enum CodingKeys: String, CodingKey {
        case permission
        case openSettings = "open_settings"
    }
}

struct UpdateSnapshot: Decodable, Equatable {
    var status = ""
    var available = false
    var automatic = true
    var checking = false
    var version = ""
    var build = ""
}

@MainActor
final class SettingsModel: ObservableObject {
    let rootURL: URL
    let dataURL: URL
    let isBundled: Bool
    let appPath: String
    let parentPID: Int32
    private var environment: [String: String] = [:]

    @Published var punctuationMode = "spaces"
    @Published var disfluencyMode = "off"
    @Published var hotkeySpec = HotkeyConfig.defaultSpec
    @Published var hotkeyLabel = HotkeyConfig.defaultLabel
    @Published var hotkeyMode = "hold"
    @Published var hotkeyBackend = HotkeyConfig.backend(for: HotkeyConfig.defaultSpec)
    @Published var launchAtLogin = true
    @Published var showMenuBarIcon = false
    @Published var reopeningApp = false {
        didSet {
            SettingsAppDelegate.reopeningApp = reopeningApp
            for window in NSApp.windows {
                window.standardWindowButton(.closeButton)?.isEnabled = !reopeningApp
            }
        }
    }
    @Published var reopenStatus = ""

    @Published var transcriptionService = "qwen"
    @Published var credentialSource = "personal"
    @Published var promotionStatus = ""
    @Published var promotionRemainingSeconds = 6000
    private var promotionContents: Data?
    @Published var qwenModel = SpeechModels.defaultModel("qwen")
    @Published var doubaoModel = SpeechModels.defaultModel("doubao")
    @Published var customASRBaseURL = ""
    @Published var customASRAPIKey = ""
    @Published var customASRModel = ""
    @Published var modelSnapshots: [String: SpeechModelSnapshot] = [:]
    @Published var refreshingModels = false
    @Published var modelCatalogStatus = ""
    private var modelRefreshTask: Task<Void, Never>?
    private var modelRefreshID = UUID()
    private var modelRefreshAttempts: [String: Date] = [:]

    @Published var qwenAPIKey = ""
    @Published var checkingConnection = false
    @Published var connectionStatus = ""
    @Published var onboardingComplete = true
    @Published var setupConnectionApplied = false
    @Published var applyingSettings = false
    @Published var trialText = ""
    @Published var saveStatus = ""
    @Published var saveFailed = false
    @Published var updateInfo = UpdateSnapshot()
    private var draftTracker = SettingsDraftTracker()

    private var editableDraft: [String] {
        [credentialSource, transcriptionService, qwenModel, doubaoModel, customASRBaseURL, customASRAPIKey,
         customASRModel, qwenAPIKey, qwenAPIHost, qwenWorkspace, qwenRegion, qwenLanguage,
         doubaoAppKey, doubaoAccessKey, doubaoBoostingTableID, hotkeySpec, hotkeyLabel,
         hotkeyMode, punctuationMode, personalContext, customVocabulary,
         String(launchAtLogin), String(showMenuBarIcon), String(qwenContextEnabled),
         String(recentMemoryEnabled)]
    }

    var hasUnsavedChanges: Bool { draftTracker.isDirty(editableDraft) }
    func publishUpdateGuard() {
        let state: [String: Any] = ["pid": getpid(), "dirty": hasUnsavedChanges || applyingSettings]
        if let data = try? JSONSerialization.data(withJSONObject: state) {
            try? Self.privateWrite(data, to: dataURL.appendingPathComponent(".settings-update-state.json"))
        }
    }

    func reloadUpdateInfo() {
        guard let data = try? Data(contentsOf: dataURL.appendingPathComponent("updates.json")),
              let snapshot = try? JSONDecoder().decode(UpdateSnapshot.self, from: data),
              snapshot != updateInfo else { return }
        updateInfo = snapshot
    }

    func requestUpdateCheck(automatic: Bool? = nil) {
        if hasUnsavedChanges || applyingSettings {
            errorMessage = "请先保存设置，再检查更新。"
            return
        }
        do {
            var request: [String: Any] = ["id": UUID().uuidString,
                "created_at": Date().timeIntervalSince1970, "action": "check"]
            if let automatic {
                request["action"] = "automatic"
                request["enabled"] = automatic
                updateInfo.automatic = automatic
            }
            try Self.privateWrite(JSONSerialization.data(withJSONObject: request),
                to: dataURL.appendingPathComponent(".update-request.json"))
            if automatic == nil { updateInfo.status = "正在检查更新…" }
        } catch { errorMessage = "未能检查更新，请重试。" }
    }
    var saveFeedback: String {
        if applyingSettings || saveFailed { return saveStatus }
        return hasUnsavedChanges ? "有未保存的修改" : saveStatus
    }
    private var pendingSaveID: String?
    private var saveStartedAt: TimeInterval = 0
    private var closeAfterApply = false
    private var advanceAfterApply = false
    private var connectionID = UUID()
    private var connectionTask: URLSessionWebSocketTask?
    private var connectionSession: URLSession?
    @Published var qwenAPIHost = ""
    @Published var qwenWorkspace = ""
    @Published var qwenRegion = "beijing"
    @Published var qwenLanguage = "zh"
    @Published var qwenContextEnabled = false
    @Published var recentMemoryEnabled = false
    @Published var doubaoAppKey = ""
    @Published var doubaoAccessKey = ""
    @Published var doubaoBoostingTableID = ""
    @Published var personalContext = ""
    @Published var customVocabulary = ""

    @Published var learningEnabled = true
    @Published var autoReplaceEnabled = true
    @Published var learnedContextEnabled = true
    @Published var corrections: [CorrectionRule] = []
    @Published var recoveries: [RecoveryEntry] = []
    @Published var diagnostics = DiagnosticSnapshot()
    @Published var permissions = PermissionSnapshot()
    @Published var permissionsLoaded = false
    @Published private(set) var permissionProcessRunning = false
    private var diagnosticsGate = SnapshotChangeGate()
    private var permissionsGate = SnapshotChangeGate()
    private var recoveriesGate = SnapshotChangeGate()
    private let recoveryQueue = DispatchQueue(label: "veyqa.recovery-edit", qos: .utility)
    private var correctionEvents: [CorrectionEvent] = []
    @Published var notice: String?
    @Published var errorMessage: String?

    init() {
        let rootPath = ProcessInfo.processInfo.environment["VOICE_INPUT_ROOT"]
            ?? FileManager.default.currentDirectoryPath
        let dataPath = ProcessInfo.processInfo.environment["VOICE_INPUT_DATA_ROOT"]
            ?? rootPath
        rootURL = URL(fileURLWithPath: rootPath, isDirectory: true)
        dataURL = URL(fileURLWithPath: dataPath, isDirectory: true)
        isBundled = ProcessInfo.processInfo.environment["VOICE_INPUT_BUNDLED"] == "true"
        appPath = ProcessInfo.processInfo.environment["VOICE_INPUT_APP_PATH"] ?? ""
        parentPID = Int32(ProcessInfo.processInfo.environment["VOICE_INPUT_PARENT_PID"] ?? "") ?? 0
        try? FileManager.default.createDirectory(
            at: dataURL, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        launchAtLogin = isBundled
        load()
    }

    var envURL: URL { dataURL.appendingPathComponent(".env") }
    var contextURL: URL { dataURL.appendingPathComponent("personal_context.txt") }
    var customVocabularyURL: URL {
        dataURL.appendingPathComponent("custom_vocabulary.txt")
    }
    var correctionsURL: URL { dataURL.appendingPathComponent("corrections.json") }
    var recoveryURL: URL { dataURL.appendingPathComponent("recovery.json") }
    var diagnosticsURL: URL { dataURL.appendingPathComponent("diagnostics.json") }
    var permissionsURL: URL { dataURL.appendingPathComponent("permissions.json") }
    var permissionRequestURL: URL {
        dataURL.appendingPathComponent(".permission-request.json")
    }
    var shortcutCaptureURL: URL { dataURL.appendingPathComponent(".shortcut-capture") }
    var supervisorPauseURL: URL { dataURL.appendingPathComponent(".supervisor-paused") }

    func load() {
        environment = Self.readEnvironment(envURL)
        launchAtLogin = isBundled
            ? Self.bool(environment["LAUNCH_AT_LOGIN"], fallback: true)
            : false
        let storedService = environment["TRANSCRIPTION_SERVICE"] ?? "qwen"
        transcriptionService = ["qwen", "doubao", "custom"].contains(storedService) ? storedService : "qwen"
        customASRBaseURL = environment["CUSTOM_ASR_BASE_URL"] ?? ""
        customASRAPIKey = environment["CUSTOM_ASR_API_KEY"] ?? ""
        customASRModel = environment["CUSTOM_ASR_MODEL"] ?? ""
        qwenModel = SpeechModels.configuredModel(environment["QWEN_ASR_MODEL"], provider: "qwen")
        doubaoModel = SpeechModels.configuredModel(environment["DOUBAO_ASR_RESOURCE_ID"], provider: "doubao")
        for provider in ["qwen", "doubao"] {
            if let data = try? Data(contentsOf: modelCacheURL(provider)),
               let snapshot = try? JSONDecoder().decode(SpeechModelSnapshot.self, from: data) {
                modelSnapshots[provider] = snapshot
            }
        }
        punctuationMode = environment["PUNCTUATION_MODE"] ?? "spaces"
        disfluencyMode = environment["DISFLUENCY_FILTER_MODE"]
            ?? (Self.bool(environment["DISFLUENCY_FILTER_ENABLED"], fallback: false)
                ? "conservative" : "off")
        hotkeyMode = environment["FN_HOTKEY_MODE"] ?? "hold"
        let configuredHotkey = environment["VOICE_HOTKEY"]?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let storedHotkey = (configuredHotkey?.isEmpty == false ? configuredHotkey : nil)
            ?? ((environment["SINGLE_FN_HOTKEY"] ?? "false") == "true"
                ? "fn" : HotkeyConfig.missingSpec(hasExistingConfig:
                    FileManager.default.fileExists(atPath: envURL.path)))
        hotkeySpec = HotkeyConfig.isSupported(storedHotkey)
            ? storedHotkey : HotkeyConfig.defaultSpec
        hotkeyLabel = hotkeySpec == storedHotkey
            ? (environment["VOICE_HOTKEY_LABEL"] ?? Self.label(for: hotkeySpec))
            : HotkeyConfig.defaultLabel
        hotkeyBackend = Self.backend(for: hotkeySpec)
        launchAtLogin = Self.bool(environment["LAUNCH_AT_LOGIN"], fallback: isBundled)
        showMenuBarIcon = Self.bool(environment["SHOW_MENU_BAR_ICON"], fallback: false)

        qwenRegion = environment["QWEN_REGION"] ?? "beijing"
        qwenAPIKey = environment["QWEN_API_KEY"] ?? environment["DASHSCOPE_API_KEY"] ?? ""
        qwenAPIHost = environment["QWEN_API_HOST"]?.trimmingCharacters(
            in: .whitespacesAndNewlines
        ) ?? ""
        qwenWorkspace = environment["QWEN_WORKSPACE_ID"] ?? ""
        qwenLanguage = environment["QWEN_LANGUAGE"] ?? "zh"
        qwenContextEnabled = Self.bool(environment["QWEN_CONTEXT_ENABLED"], fallback: false)
        recentMemoryEnabled = Self.bool(environment["QWEN_RECENT_MEMORY_ENABLED"], fallback: false)
        doubaoAppKey = environment["DOUBAO_APP_KEY"] ?? ""
        doubaoAccessKey = environment["DOUBAO_ACCESS_KEY"] ?? ""
        credentialSource = ReleaseFeatures.credentialSource(environment)
        onboardingComplete = Self.bool(environment["SETUP_COMPLETED"], fallback:
            !qwenAPIKey.isEmpty || !doubaoAccessKey.isEmpty || !customASRModel.isEmpty)
        doubaoBoostingTableID = environment["DOUBAO_BOOSTING_TABLE_ID"] ?? ""
        // Automatic edit observation remains an internal experiment until a
        // future review/confirmation UI can make every learned rule explicit.
        learningEnabled = false
        autoReplaceEnabled = false
        learnedContextEnabled = false
        personalContext = (try? String(contentsOf: contextURL, encoding: .utf8))?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        customVocabulary = (try? String(
            contentsOf: customVocabularyURL,
            encoding: .utf8
        ))?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        reloadCorrections()
        reloadRecoveries()
        reloadDiagnostics()
        reloadPermissions()
        draftTracker.reset(editableDraft)
    }

    func reloadCorrections() {
        guard let data = try? Data(contentsOf: correctionsURL),
              let file = try? JSONDecoder().decode(CorrectionFile.self, from: data) else {
            corrections = []
            correctionEvents = []
            return
        }
        correctionEvents = file.events ?? []
        corrections = file.rules.sorted {
            if $0.count == $1.count { return ($0.lastSeen ?? "") > ($1.lastSeen ?? "") }
            return $0.count > $1.count
        }
    }

    func reloadRecoveries() {
        let contents = try? Data(contentsOf: recoveryURL)
        guard recoveriesGate.changed(contents) else { return }
        guard let data = contents else {
            recoveries = []
            return
        }
        guard let entries = try? JSONDecoder().decode([RecoveryEntry].self, from: data) else {
            errorMessage = "恢复记录无法读取，原文件未被修改。请勿清理运行数据。"
            return
        }
        recoveries = entries
    }

    func reloadDiagnostics() {
        let contents = try? Data(contentsOf: diagnosticsURL)
        guard diagnosticsGate.changed(contents) else { return }
        guard let data = contents,
              let snapshot = try? JSONDecoder().decode(DiagnosticSnapshot.self, from: data)
        else {
            diagnostics = DiagnosticSnapshot()
            return
        }
        diagnostics = snapshot
    }

    func reloadPromotionInfo() {
        guard let data = try? Data(contentsOf: dataURL.appendingPathComponent("promotion.json")),
              data != promotionContents,
              let state = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        promotionContents = data
        let status = state["status"] as? String ?? ""
        let remaining = state["remaining_seconds"] as? Int ?? promotionRemainingSeconds
        if promotionStatus != status { promotionStatus = status }
        if promotionRemainingSeconds != remaining { promotionRemainingSeconds = remaining }
    }

    private func checkBuiltinConnection() {
        guard ReleaseFeatures.builtinSpeech else { return }
        cancelConnectionCheck()
        let id = connectionID
        let requestID = UUID().uuidString
        checkingConnection = true
        connectionStatus = "正在连接\(ReleaseFeatures.builtinLabel)…"
        do {
            try Self.privateWrite(Data(requestID.utf8),
                to: dataURL.appendingPathComponent(".promotion-prefetch-request"))
        } catch {
            checkingConnection = false
            connectionStatus = "无法连接后台，请重新打开设置后重试。"
            return
        }
        Task { @MainActor in
            for _ in 0..<40 {
                try? await Task.sleep(nanoseconds: 300_000_000)
                guard self.connectionID == id else { return }
                self.reloadPromotionInfo()
                guard let data = self.promotionContents,
                      let state = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                      state["request_id"] as? String == requestID else { continue }
                self.checkingConnection = false
                self.connectionStatus = self.promotionStatus
                if state["enabled"] as? Bool == true && self.promotionRemainingSeconds > 0 {
                    self.saveSettings(advanceSetup: !self.onboardingComplete)
                }
                return
            }
            guard self.connectionID == id else { return }
            self.checkingConnection = false
            self.connectionStatus = "连接超时，请检查网络后重试；也可以使用自己的 Key。"
        }
    }

    func reloadPermissions() {
        let contents = try? Data(contentsOf: permissionsURL)
        if permissionsGate.changed(contents) {
            if let data = contents,
               let snapshot = try? JSONDecoder().decode(PermissionSnapshot.self, from: data) {
                permissions = snapshot
                if !permissionsLoaded { permissionsLoaded = true }
            } else if permissionsLoaded {
                permissionsLoaded = false
            }
        }
        // A process can die without changing its last snapshot; still notice that.
        let permissionPID = permissions.pid ?? 0
        let alive = permissionPID > 1 && Darwin.kill(Int32(permissionPID), 0) == 0
        if permissionProcessRunning != alive { permissionProcessRunning = alive }
    }

    func requestPermission(_ permission: String, openSettings: Bool = true) {
        do {
            let request = PermissionRequest(
                permission: permission,
                openSettings: openSettings
            )
            try Self.privateWrite(
                JSONEncoder.pretty.encode(request),
                to: permissionRequestURL
            )
        } catch {
            errorMessage = "无法发起权限请求：\(error.localizedDescription)"
        }
    }

    var permissionsMatchCurrentApp: Bool {
        guard permissionsLoaded, permissions.checkedByCurrentProcess == true,
              permissionProcessAlive,
              let permissionVersion = permissions.version,
              permissionVersion == diagnostics.version
        else { return false }
        if !appPath.isEmpty {
            guard let permissionPath = permissions.bundlePath,
                  URL(fileURLWithPath: permissionPath).standardizedFileURL.path
                    == URL(fileURLWithPath: appPath).standardizedFileURL.path
            else { return false }
            if let executablePath = permissions.executablePath {
                let expectedExecutable = URL(fileURLWithPath: appPath)
                    .appendingPathComponent("Contents/MacOS/VeyqaVoice")
                    .standardizedFileURL.path
                guard URL(fileURLWithPath: executablePath).standardizedFileURL.path
                        == expectedExecutable
                else { return false }
            }
        }
        return true
    }

    var permissionProcessAlive: Bool {
        permissionProcessRunning
    }

    var allRequiredPermissionsGranted: Bool {
        permissionsMatchCurrentApp && permissions.allRequiredGranted == true
    }

    var readyForDictation: Bool {
        allRequiredPermissionsGranted && permissions.hotkeyReady == true && !reopeningApp
    }

    func deleteRecoveries(ids: Set<String>) {
        let url = recoveryURL
        recoveryQueue.async {
            do {
                try RecoveryFile.delete(ids: ids, at: url)
                DispatchQueue.main.async { self.reloadRecoveries() }
            } catch {
                DispatchQueue.main.async {
                    self.errorMessage = "无法更新恢复记录，请稍后重试。原记录未被清空。"
                }
            }
        }
    }

    func copyRecovery(_ entry: RecoveryEntry) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(entry.text, forType: .string)
        notice = "恢复文字已由你主动复制到剪贴板。"
    }

    func deleteCorrections(ids: Set<String>) {
        let deleted = Set(corrections.filter { ids.contains($0.id) }.map(\.id))
        corrections.removeAll { ids.contains($0.id) }
        correctionEvents.removeAll { deleted.contains($0.wrong + "\u{0}" + $0.correct) }
        persistCorrections()
    }

    func clearCorrections() {
        corrections = []
        correctionEvents = []
        persistCorrections()
    }

    func undoLastCorrection() {
        guard let event = correctionEvents.popLast() else { return }
        guard let index = corrections.firstIndex(where: {
            $0.wrong == event.wrong && $0.correct == event.correct
        }) else {
            persistCorrections()
            return
        }
        corrections[index].count -= 1
        if corrections[index].count <= 0 {
            corrections.remove(at: index)
        } else {
            corrections[index].lastSeen = correctionEvents.last(where: {
                $0.wrong == event.wrong && $0.correct == event.correct
            })?.timestamp ?? corrections[index].firstSeen
        }
        persistCorrections()
        notice = "已撤销最近一次自动学习。"
    }

    var canUndoCorrection: Bool { !correctionEvents.isEmpty }

    var customVocabularyCount: Int {
        var seen = Set<String>()
        return customVocabulary
            .components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { term in
                guard !term.isEmpty, !term.hasPrefix("#") else { return false }
                return seen.insert(term.lowercased()).inserted
            }
            .count
    }

    private func persistCorrections() {
        do {
            let data = try JSONEncoder.pretty.encode(
                CorrectionFile(version: 2, rules: corrections, events: correctionEvents)
            )
            try Self.privateWrite(data, to: correctionsURL)
        } catch {
            errorMessage = "无法更新纠错词库：\(error.localizedDescription)"
        }
    }

    func cancelConnectionCheck() {
        connectionID = UUID()
        connectionTask?.cancel(with: .goingAway, reason: nil)
        connectionSession?.invalidateAndCancel()
        connectionTask = nil
        connectionSession = nil
        checkingConnection = false
        connectionStatus = ""
    }

    var selectedModelID: String {
        if credentialSource == "builtin" { return ReleaseFeatures.builtinModel }
        switch transcriptionService {
        case "custom": return customASRModel
        case "doubao": return doubaoModel
        default: return qwenModel
        }
    }

    var serviceCredentialsReady: Bool {
        if credentialSource == "builtin" { return true }
        switch transcriptionService {
        case "custom":
            return CustomASRConnection.credentialsReady(
                address: customASRBaseURL, key: customASRAPIKey.trimmingCharacters(in: .whitespacesAndNewlines))
        case "doubao": return !doubaoAppKey.isEmpty && !doubaoAccessKey.isEmpty
        default: return !qwenAPIKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    var availableModels: [SpeechModel] {
        let provider = transcriptionService
        return SpeechModels.merge(
            [SpeechModels.option(selectedModelID, provider: provider)]
                + (modelSnapshots[provider]?.models ?? []) + SpeechModels.seed.models,
            provider: provider)
    }

    private func modelCacheURL(_ provider: String) -> URL {
        dataURL.appendingPathComponent("model-catalog-\(provider).json")
    }

    func cancelModelRefresh() {
        modelRefreshID = UUID()
        modelRefreshTask?.cancel()
        modelRefreshTask = nil
        refreshingModels = false
    }

    func refreshModels(ifStale: Bool = false) {
        cancelModelRefresh()
        let provider = transcriptionService
        guard credentialSource == "personal", provider != "custom", serviceCredentialsReady else { return }
        modelCatalogStatus = ""
        if ifStale {
            if let snapshot = modelSnapshots[provider], Date().timeIntervalSince(snapshot.checkedAt) < 86400 { return }
            if let attempt = modelRefreshAttempts[provider], Date().timeIntervalSince(attempt) < 1800 { return }
        }
        modelRefreshAttempts[provider] = Date()
        let id = modelRefreshID
        let host = QwenConnection.host(custom: qwenAPIHost, region: qwenRegion, workspace: qwenWorkspace)
        let key = qwenAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        refreshingModels = true
        modelRefreshTask = Task { @MainActor in
            defer {
                if self.modelRefreshID == id { self.refreshingModels = false; self.modelRefreshTask = nil }
            }
            do {
                let snapshot = try await SpeechModelDiscovery.refresh(provider: provider, host: host, key: key)
                guard !Task.isCancelled, self.modelRefreshID == id else { return }
                self.modelSnapshots[provider] = snapshot
                try Self.privateWrite(JSONEncoder().encode(snapshot), to: self.modelCacheURL(provider))
                self.modelCatalogStatus = "模型列表已更新"
            } catch {
                guard !Task.isCancelled, self.modelRefreshID == id else { return }
                self.modelCatalogStatus = "暂时无法刷新，仍可选择已保存的模型，或手动填写型号。"
            }
        }
    }

    func checkConnection() {
        if credentialSource == "builtin" { checkBuiltinConnection(); return }
        cancelConnectionCheck()
        guard SpeechModels.compatible(qwenModel, provider: "qwen") else {
            connectionStatus = "请先选择支持实时听写的千问模型。"
            return
        }
        let key = qwenAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard key.count >= 20, !key.contains(where: { $0.isWhitespace }) else {
            connectionStatus = "请先粘贴完整的百炼 API Key。"
            return
        }
        guard let host = QwenConnection.host(custom: qwenAPIHost, region: qwenRegion, workspace: qwenWorkspace),
              let url = URL(string: "wss://\(host)/api-ws/v1/inference") else {
            connectionStatus = "请在高级设置中填写有效的百炼 HTTPS 地址。"
            return
        }
        let id = connectionID
        checkingConnection = true
        connectionStatus = "正在检查语音服务权限，不会录音…"
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 12
        configuration.timeoutIntervalForResource = 12
        let session = URLSession(configuration: configuration)
        var request = URLRequest(url: url)
        request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        let socket = session.webSocketTask(with: request)
        connectionSession = session
        connectionTask = socket
        socket.resume()
        func finish(_ message: String, verified: Bool = false) {
            guard self.connectionID == id else { return }
            self.cancelConnectionCheck()
            self.connectionStatus = message
            if verified && !self.onboardingComplete {
                self.saveSettings(advanceSetup: true)
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 12) {
            finish("检查超时。请检查网络；海外 Key 请在高级设置中选择对应地区。")
        }
        Task { @MainActor in
            do {
                let taskID = UUID().uuidString.replacingOccurrences(of: "-", with: "")
                let data = try JSONSerialization.data(withJSONObject: QwenConnection.startRequest(taskID: taskID, model: self.qwenModel))
                try await socket.send(.string(String(decoding: data, as: UTF8.self)))
                for _ in 0..<8 {
                    let message = try await socket.receive()
                    let payload: Data
                    switch message {
                    case .string(let text): payload = Data(text.utf8)
                    case .data(let data): payload = data
                    @unknown default: continue
                    }
                    let event = try JSONSerialization.jsonObject(with: payload) as? [String: Any]
                    let header = event?["header"] as? [String: Any]
                    if header?["event"] as? String == "task-started" {
                        finish(self.onboardingComplete
                               ? "连接成功，保存后生效。"
                               : "连接成功，正在保存并应用…", verified: true)
                        return
                    }
                    if header?["event"] as? String == "task-failed" {
                        finish("服务未接受请求。请确认 Key 地区、语音模型权限和账户额度。")
                        return
                    }
                }
                finish("服务响应异常，请稍后重试。")
            } catch {
                // Don't surface request headers, API keys or server-provided content.
                finish("连接失败。请检查 Key、网络，以及高级设置中的地区和地址。")
            }
        }
    }

    func saveSettings(completeSetup: Bool = false, advanceSetup: Bool = false) {
        guard !applyingSettings else { return }
        saveFailed = false
        if completeSetup && (!setupConnectionApplied || !readyForDictation) {
            errorMessage = "请先连接语音服务并完成权限授权。"
            return
        }
        let normalizedKey = qwenAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        qwenModel = qwenModel.trimmingCharacters(in: .whitespacesAndNewlines)
        doubaoModel = doubaoModel.trimmingCharacters(in: .whitespacesAndNewlines)
        customASRModel = customASRModel.trimmingCharacters(in: .whitespacesAndNewlines)
        customASRAPIKey = customASRAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        customASRBaseURL = customASRBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard credentialSource == "builtin" || (transcriptionService == "custom"
                ? (!customASRModel.isEmpty && !customASRModel.contains(where: { $0.isWhitespace }))
                : SpeechModels.compatible(selectedModelID, provider: transcriptionService)) else {
            errorMessage = "请选择支持实时听写的模型，或检查手动填写的型号。"
            return
        }
        if credentialSource == "builtin" {
            // Preserve every personal credential and selected model while using
            // the promotional route; only the route switch is new.
        } else if transcriptionService == "qwen" {
            guard normalizedKey.count >= 20,
                  !normalizedKey.contains(where: { $0.isWhitespace }) else {
                errorMessage = "千问 API Key 格式不正确。"
                return
            }
            let normalizedHost = qwenAPIHost.trimmingCharacters(in: .whitespacesAndNewlines)
            guard QwenConnection.host(custom: normalizedHost, region: qwenRegion, workspace: qwenWorkspace) != nil else {
                errorMessage = "高级设置中的地址无效；普通百炼 Key 可将地址留空。"
                return
            }
            qwenAPIHost = normalizedHost
        } else if transcriptionService == "custom" {
            guard serviceCredentialsReady else {
                errorMessage = "请填写有效的 API 地址和 Key；本机服务可不填 Key。"
                return
            }
        } else {
            guard !doubaoAppKey.isEmpty,
                  doubaoAppKey.allSatisfy(\.isNumber),
                  doubaoAccessKey.count >= 16,
                  !doubaoAccessKey.contains(where: { $0.isWhitespace }) else {
                errorMessage = "请填写正确的豆包 App ID 和 Access Token。"
                return
            }
        }

        // The compact UI derives the backend from the chosen key. Modifier-only
        // shortcuts need the read-only event tap; ordinary combinations use
        // Carbon's single registered hotkey and avoid broad keyboard monitoring.
        hotkeyBackend = Self.backend(for: hotkeySpec)
        if hotkeyMode != "hold" && hotkeyMode != "toggle" {
            hotkeyMode = "hold"
        }
        disfluencyMode = "off"

        var updates: [String: String] = [
            "SPEECH_CREDENTIAL_SOURCE": credentialSource,
            "SETUP_COMPLETED": String(onboardingComplete || completeSetup),
            "TRANSCRIPTION_SERVICE": transcriptionService,
            "QWEN_ASR_MODEL": qwenModel,
            "DOUBAO_ASR_RESOURCE_ID": doubaoModel,
            "CUSTOM_ASR_BASE_URL": customASRBaseURL,
            "CUSTOM_ASR_API_KEY": customASRAPIKey,
            "CUSTOM_ASR_MODEL": customASRModel,
            "PUNCTUATION_MODE": punctuationMode,
            "DISFLUENCY_FILTER_ENABLED": "false",
            "DISFLUENCY_FILTER_MODE": "off",
            "VOICE_HOTKEY": hotkeySpec,
            "VOICE_HOTKEY_LABEL": hotkeyLabel,
            "FN_HOTKEY_MODE": hotkeyMode,
            "GLOBAL_HOTKEY_BACKEND": hotkeyBackend,
            "SINGLE_FN_HOTKEY": "true",
            "LAUNCH_AT_LOGIN": String(launchAtLogin),
            "SHOW_MENU_BAR_ICON": String(showMenuBarIcon),
            "QWEN_API_KEY": normalizedKey,
            "QWEN_API_HOST": qwenAPIHost,
            "QWEN_WORKSPACE_ID": qwenAPIHost.isEmpty ? qwenWorkspace : "",
            "QWEN_REGION": qwenRegion,
            "QWEN_LANGUAGE": qwenLanguage,
            "QWEN_CONTEXT_ENABLED": String(qwenContextEnabled),
            "QWEN_CONTEXT_FILE": contextURL.path,
            "CUSTOM_VOCABULARY_FILE": customVocabularyURL.path,
            "QWEN_RECENT_MEMORY_ENABLED": String(recentMemoryEnabled),
            "QWEN_RECENT_MEMORY_COUNT": "20",
            "DOUBAO_APP_KEY": doubaoAppKey,
            "DOUBAO_ACCESS_KEY": doubaoAccessKey,
            "DOUBAO_BOOSTING_TABLE_ID": doubaoBoostingTableID.trimmingCharacters(
                in: .whitespacesAndNewlines
            ),
            "EXPERIMENTAL_CORRECTION_LEARNING": "false",
            "CORRECTION_LEARNING_ENABLED": "false",
            "CORRECTION_AUTO_REPLACE": "false",
            "CORRECTION_REPLACE_MIN_COUNT": "2",
            "CORRECTION_CONTEXT_ENABLED": "false",
            "CORRECTION_CONTEXT_MIN_COUNT": "2",
        ]
        // Python expects lowercase boolean values.
        for key in updates.keys where updates[key] == "true" || updates[key] == "false" {
            updates[key] = updates[key]!.lowercased()
        }

        do {
            try Self.updateEnvironmentFile(envURL, updates: updates)
            try Self.privateWrite(Data(personalContext.utf8), to: contextURL)
            try Self.privateWrite(Data(customVocabulary.utf8), to: customVocabularyURL)
            if !isBundled {
                try restartBackgroundService()
            }
            environment.merge(updates) { _, new in new }
            draftTracker.begin(editableDraft)
            if isBundled {
                let revision = UUID().uuidString
                pendingSaveID = revision
                closeAfterApply = completeSetup
                advanceAfterApply = advanceSetup
                applyingSettings = true
                saveStartedAt = ProcessInfo.processInfo.systemUptime
                saveStatus = "已保存，正在等待生效…"
                try Self.privateWrite(Data(revision.utf8), to: dataURL.appendingPathComponent(".settings-apply-request"))
            } else {
                draftTracker.confirm()
                if advanceSetup { setupConnectionApplied = true }
                if completeSetup { onboardingComplete = true }
                notice = "设置已保存，后台语音服务已经重启。"
            }
        } catch {
            draftTracker.fail()
            applyingSettings = false
            saveFailed = true
            saveStatus = "保存失败，请重试"
            errorMessage = "保存失败：\(error.localizedDescription)"
        }
    }

    func checkAppliedSettings() {
        guard let pendingSaveID else { return }
        guard diagnostics.settingsRevision == pendingSaveID else {
            if ProcessInfo.processInfo.systemUptime - saveStartedAt > 15 {
                self.pendingSaveID = nil
                draftTracker.fail()
                applyingSettings = false
                saveFailed = true
                saveStatus = "后台暂未确认设置。请重试；若主程序已退出，请从应用程序打开 Veyqa Voice。"
            }
            return
        }
        switch diagnostics.settingsStatus {
        case "applied":
            draftTracker.confirm()
            saveStatus = onboardingComplete ? "已生效，下次听写使用新设置" : "语音服务设置已保存，可以继续"
            self.pendingSaveID = nil
            applyingSettings = false
            if advanceAfterApply { setupConnectionApplied = true }
            if closeAfterApply {
                onboardingComplete = true
                NSApp.terminate(nil)
            }
        case "pending": saveStatus = "已保存，本次听写结束后生效"
        case "error":
            draftTracker.fail()
            saveFailed = true
            saveStatus = "未能应用设置，请重试；无需重启 App"
            self.pendingSaveID = nil
            applyingSettings = false
        default: break
        }
    }

    func testMicrophone() {
        do {
            try Self.privateWrite(Data("test".utf8), to: dataURL.appendingPathComponent(".microphone-test-request"))
        } catch { errorMessage = "无法开始麦克风测试，请重试。" }
    }

    func restartAndRecheckPermissions() {
        guard isBundled, !reopeningApp else { return }
        guard !["RECORDING", "STREAMING", "PROCESSING"].contains(diagnostics.state ?? "") else {
            errorMessage = "请先完成当前听写，再重新打开。"
            return
        }
        let url = URL(fileURLWithPath: appPath).standardizedFileURL
        guard Bundle(url: url)?.bundleIdentifier == "com.wjcdev.veyqa" else {
            errorMessage = "请从“应用程序”打开 Veyqa Voice。"
            return
        }
        let running = NSRunningApplication.runningApplications(withBundleIdentifier: "com.wjcdev.veyqa")
            .first { $0.bundleURL?.standardizedFileURL == url }
        let marker = Data("permission-reopen-\(UUID().uuidString)".utf8)
        do { try Self.privateWrite(marker, to: supervisorPauseURL) }
        catch { errorMessage = "暂时无法重新打开，请稍后再试。"; return }
        reopeningApp = true
        reopenStatus = "正在重新打开，设置会保留在当前步骤…"
        if let running { Darwin.kill(running.processIdentifier, SIGTERM) }
        let deadline = Date().addingTimeInterval(10)
        func openAfterExit() {
            if let running, !running.isTerminated {
                if Date() >= deadline {
                    self.reopeningApp = false
                    self.reopenStatus = "退出尚未完成，请结束听写后再试。"
                    if (try? Data(contentsOf: self.supervisorPauseURL)) == marker {
                        try? FileManager.default.removeItem(at: self.supervisorPauseURL)
                    }
                    return
                }
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { openAfterExit() }
                return
            }
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = false
            configuration.arguments = ["--background-login"]
            var finished = false
            func finish(_ success: Bool) {
                guard !finished else { return }
                finished = true
                self.reopeningApp = false
                if success {
                    self.reopenStatus = "已重新打开，正在检查权限和快捷键…"
                    self.reloadPermissions()
                    self.requestPermission("all", openSettings: false)
                } else {
                    self.reopenStatus = "打开未完成，请点击“重新打开 App”再试。"
                    if (try? Data(contentsOf: self.supervisorPauseURL)) == marker {
                        try? FileManager.default.removeItem(at: self.supervisorPauseURL)
                    }
                }
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { finish(false) }
            NSWorkspace.shared.openApplication(at: url, configuration: configuration) { app, error in
                DispatchQueue.main.async {
                    finish(app != nil && error == nil)
                }
            }
        }
        openAfterExit()
    }

    func quitVoiceInput() {
        try? Data("paused-by-user".utf8).write(to: supervisorPauseURL, options: .atomic)
        if permissionProcessAlive, let pid = permissions.pid, pid > 1 { kill(Int32(pid), SIGTERM) }
        NSApp.terminate(nil)
    }

    private func restartBackgroundService() throws {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        task.arguments = [
            "kickstart", "-k",
            "gui/\(getuid())/com.whisper-input-next",
        ]
        try task.run()
        task.waitUntilExit()
        if task.terminationStatus != 0 {
            throw NSError(domain: "VoiceInputSettings", code: Int(task.terminationStatus),
                          userInfo: [NSLocalizedDescriptionKey: "后台服务重启失败"])
        }
    }

    static func bool(_ value: String?, fallback: Bool) -> Bool {
        guard let value else { return fallback }
        return value.lowercased() == "true"
    }

    static func label(for spec: String) -> String {
        switch spec {
        case "fn": return "地球仪 / Fn（🌐）"
        case "left_option": return "左 Option（⌥）"
        case "right_option": return "右 Option（⌥）"
        case "left_command": return "左 Command（⌘）"
        case "right_command": return "右 Command（⌘）"
        case "left_control": return "左 Control（⌃）"
        case "right_control": return "右 Control（⌃）"
        default: return "自定义快捷键"
        }
    }

    static func backend(for spec: String) -> String {
        HotkeyConfig.backend(for: spec)
    }

    static func readEnvironment(_ url: URL) -> [String: String] {
        SettingsEnvironment.read(url)
    }

    static func updateEnvironmentFile(_ url: URL, updates: [String: String]) throws {
        try SettingsEnvironment.update(url, updates: updates)
    }

    static func privateWrite(_ data: Data, to url: URL) throws {
        try SettingsEnvironment.privateWrite(data, to: url)
    }
}

extension JSONEncoder {
    static var pretty: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return encoder
    }
}

struct HotkeyRecorder: View {
    @Binding var spec: String
    @Binding var label: String
    let captureLockURL: URL
    @State private var recording = false

    var body: some View {
        HStack(spacing: 10) {
            Button {
                if recording {
                    updateCaptureLock(false)
                    recording = false
                } else {
                    // Write the lock before moving focus to the recorder view.
                    // Otherwise a very fast Option/Fn press can still trigger
                    // the running voice shortcut instead of being captured.
                    updateCaptureLock(true)
                    recording = true
                }
            } label: {
                HStack(spacing: 7) {
                    Image(systemName: recording ? "keyboard.badge.ellipsis" : "keyboard")
                    Text(recording ? "请按快捷键…" : label)
                        .monospacedDigit()
                }
                .frame(minWidth: 150)
            }
            .controlSize(.large)
            .buttonStyle(.bordered)

            Menu("常用选项") {
                Button("⌃⌥Space") {
                    choose("keycode:49;mods:control+option", "⌃⌥Space")
                }
                Button("地球仪 / Fn（🌐）") { choose("fn", "地球仪 / Fn（🌐）") }
                Button("左 Option（⌥）") { choose("left_option", "左 Option（⌥）") }
                Button("右 Option（⌥）") { choose("right_option", "右 Option（⌥）") }
                Button("左 Command（⌘）") { choose("left_command", "左 Command（⌘）") }
                Button("右 Command（⌘）") { choose("right_command", "右 Command（⌘）") }
                Button("右 Control（⌃）") { choose("right_control", "右 Control（⌃）") }
            }
            .menuStyle(.borderlessButton)
            .fixedSize()

            if recording {
                Button("取消") {
                    updateCaptureLock(false)
                    recording = false
                }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            }
        }
        .background(
            HotkeyCaptureView(isRecording: $recording, spec: $spec, label: $label)
                .frame(width: 0, height: 0)
        )
        .onChange(of: recording) { active in
            updateCaptureLock(active)
        }
        .onDisappear {
            updateCaptureLock(false)
        }
    }

    private func choose(_ newSpec: String, _ newLabel: String) {
        spec = newSpec
        label = newLabel
        recording = false
    }

    private func updateCaptureLock(_ active: Bool) {
        if active {
            try? Data(String(getpid()).utf8).write(to: captureLockURL, options: .atomic)
            return
        }
        guard let contents = try? String(contentsOf: captureLockURL, encoding: .utf8),
              contents.trimmingCharacters(in: .whitespacesAndNewlines) == String(getpid())
        else { return }
        try? FileManager.default.removeItem(at: captureLockURL)
    }
}

final class HotkeyCaptureNSView: NSView {
    var onEvent: ((NSEvent) -> Bool)?

    override var acceptsFirstResponder: Bool { true }

    override func keyDown(with event: NSEvent) {
        if onEvent?(event) != true { super.keyDown(with: event) }
    }

    override func flagsChanged(with event: NSEvent) {
        if onEvent?(event) != true { super.flagsChanged(with: event) }
    }
}

struct HotkeyCaptureView: NSViewRepresentable {
    @Binding var isRecording: Bool
    @Binding var spec: String
    @Binding var label: String

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeNSView(context: Context) -> HotkeyCaptureNSView {
        let view = HotkeyCaptureNSView(frame: .zero)
        view.onEvent = { [weak coordinator = context.coordinator] event in
            coordinator?.capture(event) ?? false
        }
        return view
    }

    func updateNSView(_ nsView: HotkeyCaptureNSView, context: Context) {
        context.coordinator.parent = self
        nsView.onEvent = { [weak coordinator = context.coordinator] event in
            coordinator?.capture(event) ?? false
        }
        if isRecording {
            context.coordinator.setActive(true)
            DispatchQueue.main.async {
                nsView.window?.makeFirstResponder(nsView)
            }
        } else if nsView.window?.firstResponder === nsView {
            context.coordinator.setActive(false)
            nsView.window?.makeFirstResponder(nil)
        } else {
            context.coordinator.setActive(false)
        }
    }

    final class Coordinator {
        var parent: HotkeyCaptureView
        private var localMonitor: Any?
        private var modifierCapture = ModifierCaptureAccumulator()

        private static let modifierOnlyChoices: [
            UInt16: (flag: NSEvent.ModifierFlags, spec: String, label: String)
        ] = [
            63: (.function, "fn", "地球仪 / Fn（🌐）"),
            58: (.option, "left_option", "左 Option（⌥）"),
            61: (.option, "right_option", "右 Option（⌥）"),
            55: (.command, "left_command", "左 Command（⌘）"),
            54: (.command, "right_command", "右 Command（⌘）"),
            59: (.control, "left_control", "左 Control（⌃）"),
            62: (.control, "right_control", "右 Control（⌃）"),
        ]

        init(_ parent: HotkeyCaptureView) { self.parent = parent }

        deinit {
            if let localMonitor { NSEvent.removeMonitor(localMonitor) }
        }

        func setActive(_ active: Bool) {
            if active, localMonitor == nil {
                localMonitor = NSEvent.addLocalMonitorForEvents(
                    matching: [.keyDown, .flagsChanged]
                ) { [weak self] event in
                    guard let self else { return event }
                    return self.capture(event) ? nil : event
                }
            } else if !active, let localMonitor {
                NSEvent.removeMonitor(localMonitor)
                self.localMonitor = nil
                modifierCapture.reset()
            }
        }

        func capture(_ event: NSEvent) -> Bool {
            if event.type == .flagsChanged {
                let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
                guard let choice = Self.modifierOnlyChoices[event.keyCode] else {
                    return false
                }
                switch modifierCapture.modifierChanged(
                    keyCode: Int(event.keyCode),
                    aggregateFlagIsSet: flags.contains(choice.flag)
                ) {
                case .waiting:
                    break
                case .singleModifier(let keyCode):
                    if let single = Self.modifierOnlyChoices[UInt16(keyCode)] {
                        complete(spec: single.spec, label: single.label)
                    }
                case .incompleteChord:
                    NSSound.beep()
                }
                return true
            }

            if event.keyCode == 53 { // Escape cancels recording.
                DispatchQueue.main.async { self.parent.isRecording = false }
                return true
            }
            let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            let modifierNames = Self.modifierNames(flags)
            let keyName = Self.keyName(event)
            guard !keyName.isEmpty, !modifierNames.isEmpty else {
                NSSound.beep()
                return true
            }
            guard let newSpec = HotkeyConfig.registeredSpec(
                keyCode: Int(event.keyCode),
                modifiers: modifierNames.map(\.name)
            ) else {
                NSSound.beep()
                return true
            }
            let newLabel = modifierNames.map(\.symbol).joined() + keyName
            complete(spec: newSpec, label: newLabel)
            return true
        }

        func complete(spec: String, label: String) {
            modifierCapture.reset()
            DispatchQueue.main.async {
                self.parent.spec = spec
                self.parent.label = label
                self.parent.isRecording = false
            }
        }

        static func modifierNames(_ flags: NSEvent.ModifierFlags) -> [(name: String, symbol: String)] {
            var result: [(String, String)] = []
            if flags.contains(.control) { result.append(("control", "⌃")) }
            if flags.contains(.option) { result.append(("option", "⌥")) }
            if flags.contains(.shift) { result.append(("shift", "⇧")) }
            if flags.contains(.command) { result.append(("command", "⌘")) }
            if flags.contains(.function) { result.append(("function", "fn ")) }
            return result
        }

        static let functionKeyCodes: [UInt16: String] = [
            122: "F1", 120: "F2", 99: "F3", 118: "F4", 96: "F5", 97: "F6",
            98: "F7", 100: "F8", 101: "F9", 109: "F10", 103: "F11", 111: "F12",
            105: "F13", 107: "F14", 113: "F15", 106: "F16", 64: "F17", 79: "F18",
            80: "F19", 90: "F20",
        ]

        static func keyName(_ event: NSEvent) -> String {
            if let function = functionKeyCodes[event.keyCode] { return function }
            let names: [UInt16: String] = [
                49: "Space", 36: "↩", 48: "Tab", 51: "⌫", 117: "⌦",
                123: "←", 124: "→", 125: "↓", 126: "↑",
            ]
            if let name = names[event.keyCode] { return name }
            return event.charactersIgnoringModifiers?.uppercased() ?? ""
        }
    }
}

private struct PermissionBadge: View {
    let status: String
    let optional: Bool

    var body: some View {
        let granted = status == "granted"
        let label = optional && !granted
            ? "当前无需"
            : (granted ? "已授权" : statusLabel)
        Label(label, systemImage: granted ? "checkmark.circle.fill" : icon)
            .font(.callout.weight(.semibold))
            .foregroundStyle(granted ? Color.green : (optional ? Color.secondary : Color.orange))
    }

    private var statusLabel: String {
        switch status {
        case "not_determined": return "等待授权"
        case "denied": return "已拒绝"
        case "restricted": return "受到限制"
        case "unavailable": return "无法检测"
        default: return "未授权"
        }
    }

    private var icon: String {
        optional ? "minus.circle" : "exclamationmark.circle.fill"
    }
}

private struct PermissionRow: View {
    let title: String
    let detail: String
    let systemImage: String
    let status: String
    let required: Bool
    let action: () -> Void

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: systemImage)
                .font(.title2)
                .foregroundStyle(Color.accentColor)
                .frame(width: 30)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.headline)
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 18)
            if status == "granted" || !required {
                PermissionBadge(status: status, optional: !required)
            } else {
                Button("授权") { action() }
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(.vertical, 8)
    }
}

struct PermissionSetupView: View {
    @ObservedObject var model: SettingsModel
    @State private var showIdentityInfo = false
    let allowsDismiss: Bool
    let onContinue: () -> Void

    var body: some View {
        VStack(spacing: 18) {
            HStack(spacing: 16) {
                Image(systemName: "waveform.circle.fill")
                    .resizable()
                    .scaledToFit()
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(.white, Color.accentColor)
                    .frame(width: 68, height: 68)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Veyqa Voice 权限")
                        .font(.system(size: 30, weight: .bold))
                    Text("允许录音和语音快捷键，让文字写入你的输入框。")
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }

            VStack(spacing: 0) {
                PermissionRow(
                    title: "麦克风",
                    detail: "采集你的语音并实时发送给所选 ASR。",
                    systemImage: "mic.fill",
                    status: model.permissions.microphone ?? "unknown",
                    required: true
                ) { model.requestPermission("microphone") }
                Divider()
                PermissionRow(
                    title: "辅助功能",
                    detail: "通过兼容粘贴写入当前光标，完成后恢复原剪贴板。",
                    systemImage: "accessibility",
                    status: model.permissions.accessibility ?? "unknown",
                    required: true
                ) { model.requestPermission("accessibility") }
                Divider()
                PermissionRow(
                    title: "输入监控",
                    detail: inputMonitoringDetail,
                    systemImage: "keyboard",
                    status: model.permissions.inputMonitoring ?? "unknown",
                    required: model.permissions.inputMonitoringRequired ?? true
                ) { model.requestPermission("input_monitoring") }
            }
            .padding(.horizontal, 18)
            .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 16))

            Button { showIdentityInfo.toggle() } label: {
                Label("查看当前应用信息", systemImage: "info.circle").frame(minHeight: 28)
            }
            .buttonStyle(.link)
            .popover(isPresented: $showIdentityInfo) {
                identityCard.padding(12).frame(width: 560)
            }

            if model.readyForDictation {
                Label("权限和快捷键已就绪，可以开始试用。", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            } else if !model.reopenStatus.isEmpty {
                Text(model.reopenStatus).font(.callout).foregroundStyle(.secondary)
            } else if model.allRequiredPermissionsGranted {
                Text("权限已开启，正在连接快捷键。准备好后会自动继续。")
                    .font(.callout).foregroundStyle(.secondary)
                Button("重新连接快捷键") {
                    try? SettingsModel.privateWrite(Data("retry".utf8),
                        to: model.dataURL.appendingPathComponent(".listener-reconnect-request"))
                }
            } else {
                Text("在系统设置中开启权限，返回后自动继续。若系统要求退出并重新打开，请按系统提示操作。")
                    .font(.callout).foregroundStyle(.secondary)
            }

            HStack {
                Menu("更多") {
                    Button("关闭设置窗口") { NSApp.terminate(nil) }
                    if model.permissionProcessAlive {
                        Divider()
                        Button("退出 Veyqa Voice", role: .destructive) {
                            model.quitVoiceInput()
                        }
                    }
                }
                .menuStyle(.borderlessButton)
                Spacer()
                Button("检查并授权") {
                    model.requestPermission("all", openSettings: false)
                }
                .buttonStyle(.bordered)
                .disabled(!model.permissionProcessAlive)
                    Button(model.reopeningApp ? "正在打开…" : "重新打开 App") {
                    model.restartAndRecheckPermissions()
                }
                .buttonStyle(.bordered)
                .disabled(model.reopeningApp)
                Button("继续") { onContinue() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.readyForDictation)
            }
        }
        .padding(28)
        .frame(width: 650)
        .onAppear { model.reloadPermissions() }
        .onChange(of: model.readyForDictation) { ready in
            if ready { onContinue() }
        }
        .interactiveDismissDisabled(!allowsDismiss && !model.allRequiredPermissionsGranted)
    }

    private var inputMonitoringDetail: String {
        if model.permissions.inputMonitoringRequired == false {
            return "当前使用系统注册组合键，不读取其他按键，因此无需此权限。"
        }
        return "右 Option、右 Command、Fn 等单修饰键需要读取全局按键事件。"
    }

    private var identityCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(
                    identityStatusText,
                    systemImage: identityStatusIcon
                )
                .font(.headline)
                .foregroundStyle(identityStatusColor)
                Spacer()
                Text("PID \(model.permissions.pid.map(String.init) ?? "—")")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            Text("Veyqa Voice v\(model.permissions.version ?? "—") · \(model.permissions.bundleIdentifier ?? "—")")
                .font(.callout.weight(.medium))
            Text(model.permissions.bundlePath ?? model.appPath)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .textSelection(.enabled)
            Text("可执行文件指纹：\(model.permissions.executableFingerprint ?? "—")")
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            Text("以上为当前应用的运行信息。")
                .font(.caption)
                .foregroundStyle(.secondary)
            if !model.permissionProcessAlive {
                Text("点击“重新打开 App”，继续当前设置。")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.red)
            }
        }
        .padding(16)
        .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
    }

    private var identityStatusText: String {
        if !model.permissionProcessAlive { return "当前主程序已退出" }
        return model.permissionsMatchCurrentApp
            ? "已确认当前运行版本" : "正在核对当前运行版本"
    }

    private var identityStatusIcon: String {
        if !model.permissionProcessAlive { return "xmark.octagon.fill" }
        return model.permissionsMatchCurrentApp
            ? "checkmark.shield.fill" : "arrow.triangle.2.circlepath"
    }

    private var identityStatusColor: Color {
        if !model.permissionProcessAlive { return .red }
        return model.permissionsMatchCurrentApp ? .green : .orange
    }
}

struct VocabularyEditorView: View {
    @Binding var vocabulary: String
    @Environment(\.dismiss) private var dismiss
    @State private var draft = ""

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("提高专有名词的识别准确率")
                        .font(.headline)
                    Text("每行添加一个专有词汇，提升识别概率。例如 user_id、seller_id、订单ID、产品名或项目代号。")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Text("词汇随识别请求发送给所选语音服务，保存后从下一次听写生效。")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                TextEditor(text: $draft)
                    .font(.body.monospaced())
                    .padding(8)
                    .scrollContentBackground(.hidden)
                    .background(Color(nsColor: .textBackgroundColor))
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .overlay {
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(Color.secondary.opacity(0.25), lineWidth: 1)
                    }
                HStack {
                    Text("共 \(termCount) 个词")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("取消") { dismiss() }
                    Button("完成") {
                        vocabulary = normalizedDraft
                        dismiss()
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
            .padding(20)
            .navigationTitle("识别词汇")
        }
        .frame(width: 560, height: 430)
        .onAppear { draft = vocabulary }
    }

    private var normalizedDraft: String {
        draft.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: "\n")
    }

    private var termCount: Int {
        Set(
            draft.components(separatedBy: .newlines)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }
                .filter { !$0.isEmpty && !$0.hasPrefix("#") }
        ).count
    }
}

struct GeneralSettingsView: View {
    @ObservedObject var model: SettingsModel
    @Binding var showPermissions: Bool
    @Binding var showVocabulary: Bool
    @Binding var showRecovery: Bool

    var body: some View {
        Form {
            Section("语音快捷键") {
                LabeledContent("快捷键") {
                    HotkeyRecorder(
                        spec: $model.hotkeySpec,
                        label: $model.hotkeyLabel,
                        captureLockURL: model.shortcutCaptureURL
                    )
                }
                Picker("操作方式", selection: $model.hotkeyMode) {
                    Text("按住说话，松开结束").tag("hold")
                    Text("按一下开始，再按一下结束").tag("toggle")
                }
                Text("按住模式：说话时不要松开。切换模式：按一下开始，再按一下结束。修改后点击“保存”。")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }

            Section("输出") {
                Picker("标点", selection: $model.punctuationMode) {
                    Text("保留自动标点").tag("auto")
                    Text("替换为空格，保留问号").tag("spaces")
                    Text("删除所有标点").tag("none")
                }
                SettingsActionRow(title: "识别词汇", detail: model.customVocabularyCount == 0
                                  ? "添加…" : "\(model.customVocabularyCount) 个词") {
                    showVocabulary = true
                }
                Text("添加 user_id、seller_id 等字段名或专有词汇，提升识别概率。")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }

            Section {
                SettingsActionRow(title: "恢复与诊断", systemImage: "lifepreserver") { showRecovery = true }
            }
            if model.isBundled {
                Section("应用") {
                    HStack {
                        Text("版本 \(model.updateInfo.version.isEmpty ? (model.diagnostics.version ?? "") : model.updateInfo.version)")
                        Spacer()
                        Button("检查更新…") { model.requestUpdateCheck() }
                            .disabled(!model.updateInfo.available || model.hasUnsavedChanges || model.applyingSettings)
                    }
                    SettingsToggleRow(title: "自动检查更新", isOn: Binding(
                        get: { model.updateInfo.automatic },
                        set: { model.requestUpdateCheck(automatic: $0) }))
                        .disabled(!model.updateInfo.available || model.hasUnsavedChanges || model.applyingSettings)
                    if !model.updateInfo.status.isEmpty {
                        Text(model.updateInfo.status).font(.callout).foregroundStyle(.secondary)
                    }
                    SettingsToggleRow(title: "登录后自动启动", isOn: $model.launchAtLogin)
                    SettingsToggleRow(title: "显示菜单栏图标", isOn: $model.showMenuBarIcon)
                    Text("点击菜单栏或 Dock 图标，随时打开设置。")
                        .font(.callout).foregroundStyle(.secondary)
                    HStack(spacing: 10) {
                        Label(
                            model.allRequiredPermissionsGranted ? "权限完整" : "权限需要处理",
                            systemImage: model.allRequiredPermissionsGranted
                                ? "checkmark.shield.fill" : "exclamationmark.shield.fill"
                        )
                        .foregroundStyle(
                            model.allRequiredPermissionsGranted ? Color.green : Color.orange
                        )
                        Spacer()
                        Button("管理权限…") { showPermissions = true }
                    }
                    Button("退出 Veyqa Voice", role: .destructive) {
                        model.quitVoiceInput()
                    }
                }
            }
        }
        .formStyle(.grouped)
    }

}

/// A single credential control for both engines. Never logs or auto-copies secrets.
struct CredentialField: View {
    let title: String
    @Binding var text: String
    @State private var isRevealed = false
    @State private var copyStatus = ""

    init(_ title: String, text: Binding<String>) {
        self.title = title
        self._text = text
    }

    var body: some View {
        HStack(spacing: 10) {
            Group {
                if isRevealed {
                    TextField(title, text: $text)
                } else {
                    SecureField(title, text: $text)
                }
            }
            .textFieldStyle(.roundedBorder)
            .autocorrectionDisabled()
            Button { isRevealed.toggle() } label: {
                Image(systemName: isRevealed ? "eye.slash" : "eye")
            }
            .help(isRevealed ? "隐藏 \(title)" : "显示 \(title)")
            .accessibilityLabel(isRevealed ? "隐藏 \(title)" : "显示 \(title)")
            .buttonStyle(SettingsIconButtonStyle())
            Button(action: copyCredential) {
                Image(systemName: copyStatus == "已复制" ? "checkmark" : "doc.on.doc")
            }
            .disabled(text.isEmpty)
            .help(copyStatus.isEmpty ? "复制 \(title)" : copyStatus)
            .accessibilityLabel(copyStatus.isEmpty ? "复制 \(title)" : copyStatus)
            .buttonStyle(SettingsIconButtonStyle())
        }
        .onDisappear { conceal() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didResignActiveNotification)) { _ in
            conceal()
        }
        .onChange(of: text) { _ in copyStatus = "" }
        .task(id: copyStatus) {
            guard !copyStatus.isEmpty else { return }
            do {
                try await Task.sleep(nanoseconds: 2_000_000_000)
                copyStatus = ""
            } catch { /* The view disappeared or feedback changed. */ }
        }
    }

    private func conceal() {
        isRevealed = false
        copyStatus = ""
    }

    private func copyCredential() {
        guard !text.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        copyStatus = pasteboard.setString(text, forType: .string) ? "已复制" : "复制失败"
    }
}

struct EngineSettingsView: View {
    @ObservedObject var model: SettingsModel
    @State private var showKeyHelp = false

    var body: some View {
        Form {
            if ReleaseFeatures.builtinSpeech {
                Section {
                    Picker("语音服务", selection: $model.credentialSource) {
                        Text(ReleaseFeatures.builtinLabel).tag("builtin")
                        Text("自己的 API").tag("personal")
                    }
                    .pickerStyle(.segmented)
                }
            }
            if model.credentialSource == "builtin" && ReleaseFeatures.builtinSpeech {
                Section(ReleaseFeatures.builtinLabel) {
                    LabeledContent("识别模型", value: SpeechModels.option(ReleaseFeatures.builtinModel, provider: "qwen").name)
                    if !ReleaseFeatures.privateTrial {
                        LabeledContent("本月剩余", value: "\(max(0, model.promotionRemainingSeconds) / 60) 分钟")
                    }
                    Text(ReleaseFeatures.privateTrial
                         ? "推广期间无需填写 Key。语音直接发送至百炼进行识别，由 Veyqa Voice 承担调用费用。"
                         : "无需填写 Key，每台设备每月可用 100 分钟。语音直接发送至百炼进行识别。")
                        .font(.callout).foregroundStyle(.secondary)
                    Button(model.checkingConnection ? "连接中…" :
                            (model.onboardingComplete ? "检查连接" : "连接并继续")) { model.checkConnection() }
                        .disabled(model.checkingConnection || model.applyingSettings)
                    if !model.connectionStatus.isEmpty {
                        Text(model.connectionStatus).font(.callout).foregroundStyle(.secondary)
                    }
                }
            } else {
                Section {
                    Picker("供应商", selection: $model.transcriptionService) {
                        Text("千问").tag("qwen")
                        Text("豆包").tag("doubao")
                        Text("自定义").tag("custom")
                    }
                    .pickerStyle(.segmented)
                }

                if model.transcriptionService == "qwen" {
                    Section("千问 API 连接") {
                        CredentialField("API Key", text: $model.qwenAPIKey)
                        HStack {
                            credentialHelpButton
                            Spacer()
                            Button(model.checkingConnection ? "连接中…" :
                                    (model.onboardingComplete ? "检查连接" : "连接并继续")) { model.checkConnection() }
                                .disabled(model.checkingConnection || model.applyingSettings)
                        }
                        if !model.connectionStatus.isEmpty {
                            Text(model.connectionStatus).font(.callout).foregroundStyle(.secondary)
                        }
                    }
                } else if model.transcriptionService == "doubao" {
                    Section("豆包 API 连接") {
                        TextField("App ID", text: $model.doubaoAppKey)
                            .textFieldStyle(.roundedBorder)
                        CredentialField("Access Token", text: $model.doubaoAccessKey)
                        credentialHelpButton
                        if !model.onboardingComplete {
                            Button("保存并继续") { model.saveSettings(advanceSetup: true) }
                        }
                    }
                } else {
                    Section("自定义 API 连接") {
                        TextField("API 地址", text: $model.customASRBaseURL, prompt: Text("https://api.example.com/v1"))
                            .textFieldStyle(.roundedBorder)
                        CredentialField("API Key", text: $model.customASRAPIKey)
                        Text("OpenAI 兼容语音转写 · 录音结束后提交。本机服务可不填 Key。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }

                if model.serviceCredentialsReady {
                    compactModelSection
                } else {
                    Text("填写上方凭证后，即可选择识别模型。")
                        .font(.callout).foregroundStyle(.secondary)
                }

                if model.transcriptionService == "qwen" {
                    DisclosureGroup("高级设置") {
                        Picker("Key 所属地区", selection: $model.qwenRegion) {
                            Text("中国内地（北京）").tag("beijing")
                            Text("新加坡").tag("singapore")
                        }
                        Picker("识别语言", selection: $model.qwenLanguage) {
                            Text("中文优先").tag("zh")
                            Text("自动判断").tag("auto")
                            Text("粤语").tag("yue")
                            Text("英语").tag("en")
                        }
                        TextField("百炼 API 地址（留空自动选择）", text: $model.qwenAPIHost)
                            .textFieldStyle(.roundedBorder)
                    }
                } else if model.transcriptionService == "doubao" {
                    DisclosureGroup("高级设置") {
                        TextField("热词表 ID（可选）", text: $model.doubaoBoostingTableID)
                            .textFieldStyle(.roundedBorder)
                        Link("热词表配置说明 ↗", destination: URL(string: "https://www.volcengine.com/docs/6561/155739?lang=zh")!)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .disabled(model.applyingSettings)
        .task(id: "\(model.transcriptionService)-\(model.serviceCredentialsReady)") { model.refreshModels(ifStale: true) }
        .onDisappear { model.cancelModelRefresh() }
        .onChange(of: model.credentialSource) { _ in
            model.cancelConnectionCheck(); model.cancelModelRefresh()
        }
        .onChange(of: model.qwenModel) { _ in model.cancelConnectionCheck() }
        .onChange(of: model.transcriptionService) { _ in
            showKeyHelp = false
            model.cancelConnectionCheck()
        }
        .onChange(of: model.qwenAPIKey) { _ in model.cancelConnectionCheck(); model.cancelModelRefresh() }
        .onChange(of: model.qwenAPIHost) { _ in model.cancelConnectionCheck(); model.cancelModelRefresh() }
        .onChange(of: model.qwenRegion) { _ in model.cancelConnectionCheck(); model.cancelModelRefresh() }
        .onChange(of: model.qwenWorkspace) { _ in model.cancelConnectionCheck(); model.cancelModelRefresh() }
    }

    private var credentialHelpButton: some View {
        Button { showKeyHelp.toggle() } label: {
            Label("获取 API 凭证", systemImage: "questionmark.circle")
                .frame(minHeight: 28)
        }
        .buttonStyle(.link)
        .popover(isPresented: $showKeyHelp, arrowEdge: .bottom) {
            CredentialHelpView(provider: model.transcriptionService)
        }
    }

    private var documentationURL: URL {
        switch model.transcriptionService {
        case "doubao": return URL(string: "https://docs.volcengine.com/docs/DoubaoVoice/model-list?lang=zh")!
        case "custom": return URL(string: "https://developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create")!
        default: return URL(string: "https://help.aliyun.com/zh/model-studio/asr-model/")!
        }
    }

    private var compactModelSection: some View {
        Section("识别模型") {
            if model.transcriptionService == "custom" {
                TextField("模型型号", text: $model.customASRModel, prompt: Text("供应商提供的 Model ID"))
                    .textFieldStyle(.roundedBorder)
                Link("接口说明 ↗", destination: documentationURL)
                if !model.onboardingComplete {
                    Button("保存并继续") { model.saveSettings(advanceSetup: true) }
                        .disabled(model.customASRModel.isEmpty)
                }
            } else {
                Picker("模型", selection: model.transcriptionService == "qwen" ? $model.qwenModel : $model.doubaoModel) {
                    ForEach(model.availableModels) { option in Text(option.name).tag(option.id) }
                }
                HStack(alignment: .top) {
                    Text(model.selectedModelID).font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    Spacer()
                    Link("官方模型与说明 ↗", destination: documentationURL)
                    Button { model.refreshModels() } label: {
                        Image(systemName: model.refreshingModels ? "hourglass" : "arrow.clockwise")
                    }.buttonStyle(SettingsIconButtonStyle())
                        .disabled(model.refreshingModels).help("刷新官方模型列表")
                }
                if !model.modelCatalogStatus.isEmpty {
                    Text(model.modelCatalogStatus).font(.caption).foregroundStyle(.secondary)
                }
                DisclosureGroup("手动型号与目录信息") {
                    TextField(model.transcriptionService == "qwen" ? "Model ID" : "Resource ID",
                              text: model.transcriptionService == "qwen" ? $model.qwenModel : $model.doubaoModel)
                        .textFieldStyle(.roundedBorder)
                    if let snapshot = model.modelSnapshots[model.transcriptionService] {
                        Text("\(snapshot.source) · \(snapshot.checkedAt.formatted(date: .abbreviated, time: .shortened))")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }
}

struct CorrectionsSettingsView: View {
    @ObservedObject var model: SettingsModel
    @State private var selection = Set<String>()
    @State private var confirmingClear = false

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section("本机自动纠错") {
                    Toggle("自动检测语音后的人工修改", isOn: $model.learningEnabled)
                    Toggle("下次识别到相同错误时自动替换", isOn: $model.autoReplaceEnabled)
                    Toggle("高频纠错额外提示当前引擎", isOn: $model.learnedContextEnabled)
                    Text("人工修改只保存在本机；高频纠错会转换为当前引擎的识别提示和本地替换规则，减少相同错误再次发生。")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
            }
            .formStyle(.grouped)
            .frame(height: 230)

            Divider()

            if model.corrections.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "text.badge.checkmark")
                        .font(.system(size: 34))
                        .foregroundStyle(.secondary)
                    Text("还没有学习记录").font(.headline)
                    Text("语音输入后直接修改错误文字，停顿约两秒即可自动学习。")
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Table(model.corrections, selection: $selection) {
                    TableColumn("识别错误", value: \.wrong)
                    TableColumn("人工改为", value: \.correct)
                    TableColumn("次数") { Text("\($0.count)") }.width(55)
                    TableColumn("最近纠正") {
                        Text(($0.lastSeen ?? "").replacingOccurrences(of: "T", with: " ").prefix(16))
                    }.width(130)
                }
            }

            HStack {
                Button {
                    model.undoLastCorrection()
                } label: { Label("撤销最近学习", systemImage: "arrow.uturn.backward") }
                .disabled(!model.canUndoCorrection)
                Button {
                    model.reloadCorrections()
                } label: { Label("刷新", systemImage: "arrow.clockwise") }
                Button(role: .destructive) {
                    model.deleteCorrections(ids: selection)
                    selection.removeAll()
                } label: { Label("删除", systemImage: "trash") }
                .disabled(selection.isEmpty)
                Spacer()
                Button("清空词库", role: .destructive) { confirmingClear = true }
                    .disabled(model.corrections.isEmpty)
            }
            .padding(12)
        }
        .navigationTitle("共享纠错")
        .confirmationDialog("确定清空全部纠错记录？", isPresented: $confirmingClear) {
            Button("清空词库", role: .destructive) { model.clearCorrections() }
        }
    }
}

struct RecoverySettingsView: View {
    @ObservedObject var model: SettingsModel
    @State private var selection = Set<String>()
    @State private var confirmingClear = false
    @State private var pendingClearIDs = Set<String>()

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section("未写入文字") {
                    Text("最近 5 条未写入的文字，可复制后继续使用。")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
                Section("诊断") {
                    LabeledContent("版本") { Text(model.diagnostics.version ?? "未知") }
                    LabeledContent("状态") { Text(stateLabel(model.diagnostics.state)) }
                    LabeledContent("麦克风") { Text(model.diagnostics.microphone ?? "未知") }
                    LabeledContent("采样率") {
                        Text(model.diagnostics.sampleRate.map { "\($0) Hz" } ?? "未知")
                    }
                    LabeledContent("快捷键") {
                        Text(model.diagnostics.hotkey ?? model.hotkeyLabel)
                    }
                    LabeledContent("键盘模式") {
                        Text(backendLabel(model.diagnostics.hotkeyBackend ?? model.hotkeyBackend))
                    }
                    LabeledContent("后台启动") {
                        Text(loginItemLabel(model.diagnostics.loginItemStatus))
                    }
                    if let error = model.diagnostics.loginItemError, !error.isEmpty {
                        LabeledContent("后台启动错误") {
                            Text(error).foregroundStyle(.red).lineLimit(2)
                        }
                    }
                    if let error = model.diagnostics.lastError, !error.isEmpty {
                        LabeledContent("最近错误") { Text(error).foregroundStyle(.red) }
                    }
                    if let recovery = model.diagnostics.recoveryStatus, !recovery.isEmpty {
                        LabeledContent("自动恢复") { Text(recovery) }
                    }
                    Button("重新启动后台服务") { model.restartAndRecheckPermissions() }
                    if let outcome = model.diagnostics.lastSessionOutcome {
                        LabeledContent("最近一轮") {
                            Text(sessionOutcomeLabel(outcome))
                        }
                    }
                    if let audio = model.diagnostics.lastSessionAudioMs,
                       let voiced = model.diagnostics.lastSessionVoicedMs {
                        LabeledContent("音频 / 有声") {
                            Text("\(audio) ms / \(voiced) ms")
                        }
                    }
                    if let previews = model.diagnostics.lastSessionPreviewCount {
                        LabeledContent("流式预览") {
                            if let latency = model.diagnostics.lastSessionFirstPreviewMs {
                                Text("\(previews) 次 · 首次 \(latency) ms")
                            } else {
                                Text("\(previews) 次")
                            }
                        }
                    }
                    HStack {
                        Button("刷新诊断") { model.reloadDiagnostics() }
                        Button("打开运行数据文件夹") { NSWorkspace.shared.open(model.dataURL) }
                    }
                }
            }
            .formStyle(.grouped)
            .frame(height: 220)

            Divider()
            if model.recoveries.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "checkmark.circle")
                        .font(.system(size: 34))
                        .foregroundStyle(.green)
                    Text("没有待恢复的文字").font(.headline)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Table(model.recoveries, selection: $selection) {
                    TableColumn("文字") { entry in
                        Text(entry.text).lineLimit(2)
                    }
                    TableColumn("原因", value: \.reason).width(150)
                    TableColumn("时间") {
                        Text(entryDate($0.timestamp))
                    }.width(130)
                }
            }

            HStack {
                Button {
                    model.reloadRecoveries()
                } label: { Label("刷新", systemImage: "arrow.clockwise") }
                Button("复制选中") {
                    if let id = selection.first,
                       let entry = model.recoveries.first(where: { $0.id == id }) {
                        model.copyRecovery(entry)
                    }
                }
                .disabled(selection.count != 1)
                Button(role: .destructive) {
                    model.deleteRecoveries(ids: selection)
                    selection.removeAll()
                } label: { Label("删除", systemImage: "trash") }
                .disabled(selection.isEmpty)
                Spacer()
                Button("清空当前列表", role: .destructive) {
                    pendingClearIDs = Set(model.recoveries.map(\.id))
                    confirmingClear = true
                }
                    .disabled(model.recoveries.isEmpty)
            }
            .padding(12)
        }
        .navigationTitle("恢复与诊断")
        .onAppear { model.reloadRecoveries() }
        .confirmationDialog("删除当前列表中的恢复文字？新收到的记录会保留。", isPresented: $confirmingClear) {
            Button("清空当前列表", role: .destructive) { model.deleteRecoveries(ids: pendingClearIDs) }
        }
    }

    private func stateLabel(_ state: String?) -> String {
        switch state {
        case "IDLE": return "就绪，按快捷键开始"
        case "RECORDING": return "正在启动麦克风"
        case "STREAMING": return "正在听写"
        case "PROCESSING": return "正在完成识别"
        default: return state ?? "未运行"
        }
    }

    private func entryDate(_ value: String) -> String {
        String(value.replacingOccurrences(of: "T", with: " ").prefix(16))
    }

    private func sessionOutcomeLabel(_ outcome: String) -> String {
        switch outcome {
        case "committed": return "已写入"
        case "cancelled": return "已取消"
        case "network_error": return "网络错误"
        case "unexpected_error": return "识别意外中断，请重试"
        case "incomplete": return "未收到完整结果，请查看恢复文字"
        case "recovered": return "已保留识别片段，请确认后复制"
        case "not_committed": return "未写入（可恢复）"
        case "no_safe_result": return "未检测到可提交语音"
        default: return outcome
        }
    }

    private func backendLabel(_ backend: String) -> String {
        switch backend {
        case "registered": return "系统注册组合键"
        case "passive": return "只读键盘监听"
        case "off": return "已关闭"
        default: return backend
        }
    }

    private func loginItemLabel(_ status: String?) -> String {
        switch status {
        case "enabled": return "已启用并受系统管理"
        case "enabled_legacy_running": return "已启用；旧版仍在运行"
        case "requires_approval": return "需要在系统设置中允许"
        case "not_registered", "not_found": return "未启用"
        case "source_run": return "源码调试模式"
        case "disabled_for_test": return "隔离测试模式"
        case "error": return "配置失败"
        default: return status ?? "等待刷新"
        }
    }
}

/// A real editor: trial dictation exercises the same focus safety and insertion
/// path as every other app, without ever granting access to credential fields.
struct SetupTrialEditor: NSViewRepresentable {
    @Binding var text: String

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        let editor = scroll.documentView as! NSTextView
        editor.isRichText = false
        editor.isAutomaticQuoteSubstitutionEnabled = false
        editor.font = NSFont.systemFont(ofSize: 17)
        editor.textContainerInset = NSSize(width: 12, height: 12)
        editor.setAccessibilityIdentifier("veyqa.setup.trial")
        editor.setAccessibilityLabel("语音试用文本框")
        editor.delegate = context.coordinator
        scroll.hasVerticalScroller = true
        return scroll
    }
    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        if let editor = scroll.documentView as? NSTextView, editor.string != text {
            editor.string = text
        }
    }
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: SetupTrialEditor
        init(_ parent: SetupTrialEditor) { self.parent = parent }
        func textDidChange(_ notification: Notification) {
            if let editor = notification.object as? NSTextView { parent.text = editor.string }
        }
    }
}

struct FirstRunView: View {
    @ObservedObject var model: SettingsModel
    @Binding var showPermissions: Bool

    private var step: SetupStep {
        SetupStep.current(connectionApplied: model.setupConnectionApplied,
                          permissionsGranted: model.readyForDictation)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("欢迎使用 Veyqa Voice").font(.title2.bold())
            Text("\(step.rawValue) / 3 · 连接语音服务 → 开启权限 → 试用语音输入")
                .foregroundStyle(.secondary)
            switch step {
            case .connection:
                EngineSettingsView(model: model)
            case .permissions:
                Text("语音服务设置已保存。接下来允许麦克风与键盘相关权限。")
                Text("在系统设置中打开权限，返回后会自动检查。")
                    .font(.callout).foregroundStyle(.secondary)
                Button("开启所需权限") { showPermissions = true }
                    .buttonStyle(.borderedProminent)
                Spacer()
            case .trial:
                Text("试着说一句：今天开始使用语音输入。")
                    .font(.headline)
                Text(SetupStep.instruction(hotkey: model.hotkeyLabel, mode: model.hotkeyMode))
                SetupTrialEditor(text: $model.trialText)
                    .frame(minHeight: 140, maxHeight: 220)
                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.3)))
                Text("文字会写到这里，不会自动发送。本次语音会发送给你选择的云端服务。")
                    .font(.caption).foregroundStyle(.secondary)
                if let error = model.diagnostics.lastError, !error.isEmpty {
                    Text(error).font(.callout).foregroundStyle(.orange)
                }
                HStack {
                    Button("没有收到声音？测试麦克风") { model.testMicrophone() }
                    if let status = model.diagnostics.microphoneTestStatus {
                        Text(status == "passed" ? "麦克风已收到声音" : status).font(.caption)
                    }
                }
                Spacer()
                HStack {
                    Button("返回连接设置") { model.setupConnectionApplied = false }
                    Spacer()
                    Button(model.trialText.isEmpty ? "稍后试用，关闭设置" : "完成设置") {
                        model.saveSettings(completeSetup: true)
                    }.buttonStyle(.borderedProminent).disabled(model.applyingSettings)
                }
                Text("完成后点击任意 App 的输入框，即可使用语音快捷键。点击菜单栏或 Dock 图标可打开设置。")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }.padding(20)
    }
}

struct SettingsRootView: View {
    @Environment(\.openWindow) private var openWindow
    @ObservedObject var model: SettingsModel
    @State private var showPermissions = false
    @State private var showVocabulary = false
    @State private var showRecovery = false
    @State private var evaluatedPermissionGate = false
    @State private var selectedTab = "general"
    private let permissionTimer = Timer.publish(
        every: 2.0, tolerance: 0.5, on: .main, in: .common
    ).autoconnect()

    var body: some View {
        VStack(spacing: 8) {
        if !model.onboardingComplete {
            FirstRunView(model: model, showPermissions: $showPermissions)
        } else {
        TabView(selection: $selectedTab) {
            GeneralSettingsView(
                model: model,
                showPermissions: $showPermissions,
                showVocabulary: $showVocabulary,
                showRecovery: $showRecovery
            )
                .disabled(model.applyingSettings)
                .tabItem { Label("通用", systemImage: "slider.horizontal.3") }
                .tag("general")
            EngineSettingsView(model: model)
                .tabItem { Label("语音服务", systemImage: "waveform.badge.magnifyingglass") }
                .tag("engine")
        }
        }
        HStack(spacing: 7) {
            if !model.saveFeedback.isEmpty {
                if model.applyingSettings {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: model.saveFailed ? "exclamationmark.circle" :
                          model.hasUnsavedChanges ? "circle.fill" : "checkmark.circle.fill")
                        .foregroundStyle(model.saveFailed || model.hasUnsavedChanges ? Color.orange : Color.green)
                        .font(.caption)
                        .accessibilityHidden(true)
                }
                Text(model.saveFeedback).font(.callout).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .frame(height: 24)
        .padding(.horizontal, 20)
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                if model.onboardingComplete {
                Button {
                    model.saveSettings()
                } label: {
                    Text("保存")
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut("s", modifiers: .command)
                .disabled(model.applyingSettings || !model.hasUnsavedChanges)
                }
            }
        }
        .padding(.top, 8)
        .disclosureGroupStyle(SettingsDisclosureStyle())
        .frame(width: 700, height: model.onboardingComplete ? 500 : 660)
        .onAppear {
            SettingsAppDelegate.openSettingsWindow = { openWindow(id: "settings") }
            if !model.onboardingComplete { selectedTab = "engine" }
            model.reloadDiagnostics()
            model.reloadPermissions()
            model.reloadUpdateInfo()
            model.reloadPromotionInfo()
            model.publishUpdateGuard()
            evaluatePermissionGate()
        }
        .onChange(of: model.hasUnsavedChanges) { _ in model.publishUpdateGuard() }
        .onChange(of: model.applyingSettings) { _ in model.publishUpdateGuard() }
        .onReceive(permissionTimer) { _ in
            // Finish an in-flight save even if the user switches to another app.
            guard NSApp.isActive || model.applyingSettings else { return }
            model.reloadDiagnostics()
            model.reloadUpdateInfo()
            model.reloadPromotionInfo()
            model.reloadPermissions()
            model.reloadRecoveries()
            model.checkAppliedSettings()
            evaluatePermissionGate()
        }
        .sheet(isPresented: $showPermissions) {
            PermissionSetupView(
                model: model,
                allowsDismiss: false,
                onContinue: { showPermissions = false }
            )
        }
        .sheet(isPresented: $showVocabulary) {
            VocabularyEditorView(vocabulary: $model.customVocabulary)
        }
        .sheet(isPresented: $showRecovery) {
            VStack(spacing: 12) {
                HStack {
                    Text("恢复与诊断").font(.headline)
                    Spacer()
                    Button("完成") { showRecovery = false }.keyboardShortcut(.cancelAction)
                }.padding([.top, .horizontal])
                RecoverySettingsView(model: model)
            }.frame(width: 660, height: 500)
        }
        .alert("已完成", isPresented: Binding(
            get: { model.notice != nil },
            set: { if !$0 { model.notice = nil } }
        )) {
            Button("好") { model.notice = nil }
        } message: {
            Text(model.notice ?? "")
        }
        .alert("操作未完成", isPresented: Binding(
            get: { model.errorMessage != nil },
            set: { if !$0 { model.errorMessage = nil } }
        )) {
            Button("好") { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
    }

    private func evaluatePermissionGate() {
        guard model.isBundled,
              model.permissionsLoaded,
              (model.onboardingComplete || model.setupConnectionApplied),
              !evaluatedPermissionGate
        else { return }
        // A snapshot from another PID/version/path is deliberately treated as
        // untrusted even if every boolean inside it says "granted".
        evaluatedPermissionGate = true
        showPermissions = !model.allRequiredPermissionsGranted
    }
}

@main
struct VoiceInputSettingsApp: App {
    @NSApplicationDelegateAdaptor(SettingsAppDelegate.self) private var appDelegate
    @StateObject private var model = SettingsModel()

    var body: some Scene {
        Window("语音输入设置", id: "settings") {
            SettingsRootView(model: model)
                .onAppear {
                    bringSettingsToFront()
                }
        }
        .defaultSize(width: 660, height: 500)
        .windowResizability(.contentSize)
        .commands {
            CommandGroup(replacing: .newItem) { }
        }
    }

    private func bringSettingsToFront() {
        let activate: () -> Void = { raiseSettingsWindows() }
        DispatchQueue.main.async(execute: activate)
    }
}
