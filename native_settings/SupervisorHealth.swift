import Foundation

/// A quiet microphone is healthy. Only a missing process heartbeat is a fault.
struct SupervisorHealth {
    private var observedPID: Int32 = 0
    private var firstSeen: TimeInterval = 0
    private var lastCheck: TimeInterval = 0

    mutating func needsRecovery(
        pid: Int32, heartbeatPID: Int32?, timestamp: TimeInterval?, now: TimeInterval
    ) -> Bool {
        if pid != observedPID || now - lastCheck > 15 || now < lastCheck {
            observedPID = pid
            firstSeen = now
        }
        lastCheck = now
        // Includes startup, wake, and resumed processes: don't restart on wake.
        guard now - firstSeen > 90 else { return false }
        guard heartbeatPID == pid, let timestamp else { return true }
        return now - timestamp > 30
    }
}
