import Foundation

/// Apps whose sound a sharing Mac can leave out ("Don't share sound from" in Settings). Their windows
/// stay in the picture; only their sound is left out.
public enum SoundApps {
    public struct App {
        public let id: String
        public let name: String
        /// For "Discord, Chrome" on the button.
        public let shortName: String
        /// Any bundle id starting with `id` is this app (Helium's builds don't all use the same one).
        public let anyIdStartingWith: Bool
    }

    public static let all = [
        App(id: "com.hnc.Discord", name: "Discord", shortName: "Discord", anyIdStartingWith: false),
        App(id: "com.google.Chrome", name: "Google Chrome", shortName: "Chrome", anyIdStartingWith: false),
        App(id: "net.imput.helium", name: "Helium", shortName: "Helium", anyIdStartingWith: true),
    ]

    public static let mutedByDefault = ["com.hnc.Discord"]

    /// The app a running process belongs to. Its helpers count: Chromium and Electron apps play sound
    /// from a helper process (com.google.Chrome.helper, com.hnc.Discord.helper, …).
    public static func app(forBundleId bundleId: String) -> App? {
        all.first { app in
            bundleId == app.id || bundleId.hasPrefix(app.id + ".") || (app.anyIdStartingWith && bundleId.hasPrefix(app.id))
        }
    }

    /// Should a running process with this bundle id be left out of the sound?
    public static func isMuted(_ bundleId: String, muted: Set<String>) -> Bool {
        guard let app = app(forBundleId: bundleId) else { return false }
        return muted.contains(app.id)
    }
}
