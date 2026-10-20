import AppKit
import BB1Core

/// Watches the system cursor's shape (30 times a second, on the main thread) and reports changes.
final class CursorWatcher {
    var onChange: ((CursorMsg) -> Void)?
    private var timer: Timer?
    private var lastSignature: UInt64 = 0

    func start() {
        DispatchQueue.main.async {
            self.lastSignature = 0
            self.timer?.invalidate()
            self.timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) { [weak self] _ in self?.check() }
            self.check()
        }
    }

    func stop() {
        DispatchQueue.main.async {
            self.timer?.invalidate()
            self.timer = nil
        }
    }

    private func check() {
        guard let cursor = NSCursor.currentSystem else { return }
        let image = cursor.image
        var rect = NSRect(origin: .zero, size: image.size)
        guard let cg = image.cgImage(forProposedRect: &rect, context: nil, hints: nil),
              let data = cg.dataProvider?.data, let bytes = CFDataGetBytePtr(data) else { return }
        // FNV-1a over the pixels plus size and hot spot: cheap, and only changes when the shape does.
        var h: UInt64 = 0xcbf29ce484222325
        func mix(_ b: UInt8) { h ^= UInt64(b); h = h &* 0x100000001b3 }
        let length = CFDataGetLength(data)
        for i in stride(from: 0, to: length, by: 4) { mix(bytes[i]); mix(bytes[i + 3]) }
        for v in [image.size.width, image.size.height, cursor.hotSpot.x, cursor.hotSpot.y] { mix(UInt8(truncatingIfNeeded: Int(v * 4))) }
        guard h != lastSignature else { return }
        lastSignature = h
        guard let png = NSBitmapImageRep(cgImage: cg).representation(using: .png, properties: [:]) else { return }
        onChange?(CursorMsg(png: png, width: image.size.width, height: image.size.height,
                            hotX: cursor.hotSpot.x, hotY: cursor.hotSpot.y))
    }
}
