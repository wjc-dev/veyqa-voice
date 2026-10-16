import AppKit
import Darwin
import Foundation

private let bundleIdentifier = "com.wjcdev.veyqa"
private let checkInterval: UInt32 = 5
private let crashWindow: TimeInterval = 60
private let crashLimit = 3
private let crashCooldown: UInt32 = 300
private let pauseURL = FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent("Library/Application Support/Veyqa/.supervisor-paused")
private let healthURL = pauseURL.deletingLastPathComponent().appendingPathComponent("health.json")

private func log(_ message: String) {
    let stamp = ISO8601DateFormatter().string(from: Date())
    let data = Data("\(stamp) \(message)\n".utf8)
    try? FileHandle.standardError.write(contentsOf: data)
}

private func enclosingAppURL() -> URL? {
    // SMAppService may launch BundleProgram with a relative argv[0], for
    // example "Contents/Resources/VeyqaSupervisor". _NSGetExecutablePath
    // resolves the actual image path regardless of launchd's working directory.
    var bufferSize: UInt32 = 0
    _ = _NSGetExecutablePath(nil, &bufferSize)
    guard bufferSize > 0 else { return nil }
    let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufferSize))
    defer { buffer.deallocate() }
    guard _NSGetExecutablePath(buffer, &bufferSize) == 0 else { return nil }

    var current = URL(
        fileURLWithFileSystemRepresentation: buffer,
        isDirectory: false,
        relativeTo: nil
    ).resolvingSymlinksInPath().standardizedFileURL
    while current.path != "/" {
        if current.pathExtension == "app" { return current }
        current.deleteLastPathComponent()
    }

    func verifiedHost(_ candidate: URL?) -> URL? {
        guard let candidate else { return nil }
        let resolved = candidate.resolvingSymlinksInPath().standardizedFileURL
        guard resolved.pathExtension == "app",
              FileManager.default.fileExists(atPath: resolved.path),
              Bundle(url: resolved)?.bundleIdentifier == bundleIdentifier else {
            return nil
        }
        return resolved
    }

    // launchd may isolate LaunchServices and preserve BundleProgram as a
    // relative image path. Prefer a live process URL, then the documented
    // installation locations, and only then ask LaunchServices.
    if let liveURL = NSRunningApplication.runningApplications(
        withBundleIdentifier: bundleIdentifier
    ).compactMap({ $0.bundleURL }).compactMap(verifiedHost).first {
        return liveURL
    }
    let standardCandidates = [
        URL(fileURLWithPath: "/Applications/Veyqa Voice.app"),
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Applications/Veyqa Voice.app"),
    ]
    if let installedURL = standardCandidates.compactMap(verifiedHost).first {
        return installedURL
    }
    if let registeredURL = verifiedHost(
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleIdentifier)
    ) {
        return registeredURL
    }
    return nil
}

private func appIsRunning() -> Bool {
    !NSRunningApplication.runningApplications(
        withBundleIdentifier: bundleIdentifier
    ).isEmpty
}

private func launch(_ appURL: URL) -> Bool {
    let task = Process()
    task.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    task.arguments = ["-g", appURL.path, "--args", "--background-login", "--supervised"]
    task.standardOutput = FileHandle.nullDevice
    task.standardError = FileHandle.standardError
    do {
        try task.run()
        let deadline = Date().addingTimeInterval(15)
        while task.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.2) }
        if task.isRunning {
            task.terminate()
            log("LaunchServices timed out")
            return false
        }
        return task.terminationStatus == 0
    } catch {
        log("launch failed: \(error.localizedDescription)")
        return false
    }
}

@main
struct SupervisorMain {
static func main() {
guard let appURL = enclosingAppURL() else {
    log("cannot locate containing Veyqa Voice.app")
    exit(78)
}

log("supervisor started for \(bundleIdentifier)")
var launches: [Date] = []
var health = SupervisorHealth()
var stopping: (app: NSRunningApplication, at: Date)?
while true {
    if FileManager.default.fileExists(atPath: pauseURL.path) {
        sleep(checkInterval)
        continue
    }
    let now = Date()
    launches.removeAll { now.timeIntervalSince($0) > crashWindow }
    if let pending = stopping {
        let alive = kill(pending.app.processIdentifier, 0) == 0
        if alive && now.timeIntervalSince(pending.at) >= 5 {
            // Only the previously verified host process, never a name-based kill.
            pending.app.forceTerminate()
        }
        if !alive { stopping = nil }
        sleep(checkInterval)
        continue
    }
    if let app = NSRunningApplication.runningApplications(
        withBundleIdentifier: bundleIdentifier
    ).first(where: { $0.bundleURL?.standardizedFileURL == appURL.standardizedFileURL }) {
        let data = try? Data(contentsOf: healthURL)
        let snapshot = data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]
        if health.needsRecovery(
            pid: app.processIdentifier,
            heartbeatPID: (snapshot?["pid"] as? NSNumber)?.int32Value,
            timestamp: snapshot?["timestamp"] as? Double,
            now: now.timeIntervalSince1970
        ) && launches.count < crashLimit {
            log("functional heartbeat stalled; recovering host")
            kill(app.processIdentifier, SIGTERM)
            stopping = (app, now)
        }
        sleep(checkInterval)
        continue
    }

    if launches.count >= crashLimit {
        log("crash loop detected; cooling down for \(crashCooldown) seconds")
        sleep(crashCooldown)
        launches.removeAll()
        continue
    }

    launches.append(now)
    if launch(appURL) {
        log("Veyqa Voice launch requested")
        // LaunchServices may need a few seconds to finish starting a frozen
        // Python bundle. Do not count that normal startup time as another crash.
        sleep(10)
        continue
    }
    sleep(checkInterval)
}
}
}
