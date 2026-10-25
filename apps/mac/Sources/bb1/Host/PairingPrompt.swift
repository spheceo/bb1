import AppKit

/// Host: a floating window with the 6-digit code while another Mac pairs.
final class PairingPrompt: NSObject {
    private var panel: NSPanel?
    var onCancel: (() -> Void)?

    func show(device: String, code: String) {
        dismiss()
        let p = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 380, height: 220),
                        styleMask: [.titled, .closable, .nonactivatingPanel, .hudWindow], backing: .buffered, defer: false)
        p.title = "bb1 pairing"
        p.level = .floating
        p.isReleasedWhenClosed = false
        p.hidesOnDeactivate = false
        p.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]

        let title = NSTextField(wrappingLabelWithString: "\(device) wants to pair with this Mac")
        title.font = .systemFont(ofSize: 14, weight: .semibold)
        title.alignment = .center
        let spaced = String(code.prefix(3)) + " " + String(code.suffix(3))
        let digits = NSTextField(labelWithString: spaced)
        digits.font = .monospacedDigitSystemFont(ofSize: 44, weight: .semibold)
        digits.alignment = .center
        let hint = NSTextField(wrappingLabelWithString: "Type this code on \(device). If you didn't ask for this, press Cancel.")
        hint.font = .systemFont(ofSize: 11)
        hint.textColor = .secondaryLabelColor
        hint.alignment = .center
        let cancel = NSButton(title: "Cancel", target: self, action: #selector(cancelTapped))
        let stack = NSStackView(views: [title, digits, hint, cancel])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 18, left: 20, bottom: 18, right: 20)
        p.contentView = stack
        p.center()
        p.orderFrontRegardless()
        panel = p
        NSSound(named: "Glass")?.play()
    }

    func dismiss() {
        panel?.close()
        panel = nil
    }

    @objc private func cancelTapped() {
        dismiss()
        onCancel?()
    }
}
