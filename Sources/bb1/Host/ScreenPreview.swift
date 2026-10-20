import AppKit
import BB1Core
import ScreenCaptureKit

/// A one-off, small JPEG of the main display for the dashboard on your other Macs.
enum ScreenPreview {
    /// Calls back on an arbitrary queue. Without Screen Recording permission the message has no picture.
    static func capture(maxWidth: Int, done: @escaping (PreviewMsg) -> Void) {
        let start = Clock.nowUs()
        let empty = PreviewMsg(jpeg: nil, captureMs: 0)
        guard CGPreflightScreenCaptureAccess() else { done(empty); return }
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, _ in
            guard let displays = content?.displays,
                  let display = displays.first(where: { $0.displayID == CGMainDisplayID() }) ?? displays.first else {
                done(empty)
                return
            }
            let config = SCStreamConfiguration()
            let scale = min(1, Double(maxWidth) / Double(max(display.width, 1)))
            config.width = max(1, Int(Double(display.width) * scale))
            config.height = max(1, Int(Double(display.height) * scale))
            config.showsCursor = false
            let filter = SCContentFilter(display: display, excludingWindows: [])
            SCScreenshotManager.captureImage(contentFilter: filter, configuration: config) { image, _ in
                guard let image else { done(empty); return }
                let jpeg = NSBitmapImageRep(cgImage: image).representation(using: .jpeg, properties: [.compressionFactor: 0.6])
                done(PreviewMsg(jpeg: jpeg, captureMs: Double(Clock.nowUs() - start) / 1000))
            }
        }
    }
}
