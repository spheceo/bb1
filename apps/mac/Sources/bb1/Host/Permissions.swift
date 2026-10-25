import AppKit
import ApplicationServices
import BB1Core

/// Screen Recording and Accessibility for bb1.
///
/// macOS remembers which app it allowed by the app's signature. When bb1's signature changes (the move
/// from the local certificate to the Developer ID, or a new certificate one day), the old entries stay
/// in System Settings looking switched on but don't work. Switching them off and on doesn't help, and
/// removing one hides bb1 from the list until it asks again. Resetting bb1's own entries (no admin
/// password needed) and asking again fixes all of that: bb1 shows up in the list, ready to switch on.
enum Permissions {
    static var screenRecording: Bool { CGPreflightScreenCaptureAccess() }
    static var accessibility: Bool { AXIsProcessTrusted() }

    private static var mayPrompt: Bool { ProcessInfo.processInfo.environment["BB1_NO_PROMPT"] == nil }
    private static let repairedKey = "bb1.permissionsRepairedForBuild"
    /// Services reset since bb1 started. A permission switched on can read as missing until bb1 restarts
    /// (Screen Recording does), so a second "Allow…" mustn't wipe the switch just turned on.
    private static var resetThisRun: Set<String> = []

    /// At start: whatever's missing is asked for. The first time a build finds one missing, bb1's entries
    /// for it are cleared first, in case they're left over from an older signature. Once per build, so
    /// someone who said no isn't asked again and again.
    static func askForMissing(screen: Bool, accessibility needAccessibility: Bool) {
        guard mayPrompt else { return }
        let missingScreen = screen && !screenRecording
        let missingAccessibility = needAccessibility && !accessibility
        guard missingScreen || missingAccessibility else { return }
        let firstTime = UserDefaults.standard.integer(forKey: repairedKey) != BB1.build
        if firstTime {
            UserDefaults.standard.set(BB1.build, forKey: repairedKey)
            Log.info("permissions: \([missingScreen ? "Screen Recording" : nil, missingAccessibility ? "Accessibility" : nil].compactMap { $0 }.joined(separator: " and ")) not working; clearing bb1's old entries and asking again")
        }
        if missingScreen {
            if firstTime { reset(["ScreenCapture"]) }
            CGRequestScreenCaptureAccess()
        }
        if missingAccessibility {
            if firstTime { reset(["Accessibility", "PostEvent", "ListenEvent"]) }
            promptAccessibility()
        }
    }

    /// The "Allow…" buttons: clear bb1's entries that aren't working, ask again, and open the right pane.
    static func fixScreenRecording() {
        if !screenRecording { reset(["ScreenCapture"]) }
        CGRequestScreenCaptureAccess()
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")!)
    }

    static func fixAccessibility() {
        if !accessibility {
            reset(["Accessibility", "PostEvent", "ListenEvent"])
            promptAccessibility()
        }
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!)
    }

    private static func promptAccessibility() {
        let prompt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        _ = AXIsProcessTrustedWithOptions([prompt: true] as CFDictionary)
    }

    /// `tccutil reset <service> com.bb1.host`: only bb1's own entries.
    private static func reset(_ services: [String]) {
        guard let id = Bundle.main.bundleIdentifier else { return }
        for service in services where resetThisRun.insert(service).inserted {
            let task = Process()
            task.executableURL = URL(fileURLWithPath: "/usr/bin/tccutil")
            task.arguments = ["reset", service, id]
            task.standardOutput = FileHandle.nullDevice
            task.standardError = FileHandle.nullDevice
            do {
                try task.run()
                task.waitUntilExit()
            } catch {
                Log.info("permissions: couldn't reset \(service): \(error.localizedDescription)")
            }
        }
    }
}
