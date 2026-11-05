import AppKit
import BB1Core
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
@preconcurrency import ScreenCaptureKit

/// Something that produces frames on the pipeline queue.
protocol FrameSource: AnyObject {
    /// `onFrame` and `completion` are called on the queue given at init.
    func start(onFrame: @escaping (CVPixelBuffer, UInt64) -> Void,
               onStop: @escaping (String) -> Void,
               completion: @escaping (Result<(width: Int, height: Int), Error>) -> Void)
    func stop()
    func setShowsCursor(_ show: Bool)
    /// Bundle ids of apps whose sound is left out (their windows stay in the picture).
    func setMutedApps(_ ids: Set<String>)
    /// System sound, on its own queue. Only the screen source has sound.
    var onAudio: ((CMSampleBuffer) -> Void)? { get set }
}

enum CaptureError: LocalizedError {
    case noDisplay
    var errorDescription: String? { "no display found to capture" }
}

/// Captures one display (the main one unless told otherwise) with ScreenCaptureKit at native pixel
/// resolution. The sound comes from
/// a second, audio-only stream (`SoundCapture`), so apps can be left out of the sound without
/// leaving their windows out of the picture. Sendable because its state is only touched on `queue`.
final class ScreenSource: NSObject, FrameSource, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let queue: DispatchQueue
    private let fps: Int
    private var showsCursor: Bool
    private var stream: SCStream?
    private var config: SCStreamConfiguration?
    private var stopped = false
    private var reportedStop = false
    private var onFrame: ((CVPixelBuffer, UInt64) -> Void)?
    private var onStop: ((String) -> Void)?
    private let sound: SoundCapture
    /// nil: the main display. One that's gone falls back to the main display too.
    private let displayID: CGDirectDisplayID?
    var onAudio: ((CMSampleBuffer) -> Void)?

    init(queue: DispatchQueue, fps: Int, showsCursor: Bool, mutedApps: Set<String>, displayID: CGDirectDisplayID? = nil) {
        self.queue = queue
        self.fps = fps
        self.showsCursor = showsCursor
        self.displayID = displayID
        sound = SoundCapture(queue: queue, mutedApps: mutedApps)
        super.init()
        sound.onAudio = { [weak self] sample in self?.onAudio?(sample) }
    }

    func start(onFrame: @escaping (CVPixelBuffer, UInt64) -> Void,
               onStop: @escaping (String) -> Void,
               completion: @escaping (Result<(width: Int, height: Int), Error>) -> Void) {
        self.onFrame = onFrame
        self.onStop = onStop
        Task {
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
                let mainID = CGMainDisplayID()
                let wanted = self.displayID ?? mainID
                guard let display = content.displays.first(where: { $0.displayID == wanted })
                        ?? content.displays.first(where: { $0.displayID == mainID }) ?? content.displays.first else {
                    throw CaptureError.noDisplay
                }
                let mode = CGDisplayCopyDisplayMode(display.displayID)
                let width = mode?.pixelWidth ?? display.width
                let height = mode?.pixelHeight ?? display.height

                let cfg = SCStreamConfiguration()
                cfg.width = width
                cfg.height = height
                cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
                cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
                cfg.queueDepth = 5
                cfg.showsCursor = showsCursor
                cfg.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
                cfg.colorSpaceName = CGColorSpace.sRGB
                // No sound here: SoundCapture has its own stream (an app left out of this one would
                // vanish from the picture too).
                cfg.capturesAudio = false

                let stream = SCStream(filter: SCContentFilter(display: display, excludingWindows: []),
                                      configuration: cfg, delegate: self)
                try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
                try await stream.startCapture()
                queue.async {
                    if self.stopped {
                        stream.stopCapture { _ in }
                        return
                    }
                    self.stream = stream
                    self.config = cfg
                    // The client may have changed the cursor setting while capture was starting.
                    if cfg.showsCursor != self.showsCursor { self.setShowsCursor(self.showsCursor) }
                    Log.info("capture: started display \(display.displayID) at \(width)x\(height) @\(self.fps)fps, cursor \(self.showsCursor ? "in video" : "hidden")")
                    self.sound.start(displayID: display.displayID)
                    completion(.success((width, height)))
                }
            } catch {
                queue.async { completion(.failure(error)) }
            }
        }
    }

    func stop() {
        stopped = true
        onFrame = nil
        onStop = nil
        stream?.stopCapture { _ in }
        stream = nil
        sound.stop()
    }

    func setMutedApps(_ ids: Set<String>) { sound.setMutedApps(ids) }

    func setShowsCursor(_ show: Bool) {
        showsCursor = show
        guard let stream, let config else { return } // not started yet: applied once it starts
        config.showsCursor = show
        stream.updateConfiguration(config) { error in
            if let error { Log.info("capture: couldn't change cursor setting: \(error.localizedDescription)") }
            else { Log.info("capture: cursor \(show ? "in video" : "hidden (client draws its own)")") }
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid,
              let attachments = (CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]])?.first,
              let rawStatus = attachments[.status] as? Int else { return }
        // macOS stopped the stream. This frame arrives seconds before didStopWithError, so restart now.
        if SCFrameStatus(rawValue: rawStatus) == .stopped {
            reportStop("capture stopped: the system ended the stream")
            return
        }
        guard SCFrameStatus(rawValue: rawStatus) == .complete,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        var captureUs = Clock.nowUs()
        if let displayTime = attachments[.displayTime] as? UInt64 { captureUs = Clock.machToUs(displayTime) }
        onFrame?(pixelBuffer, captureUs)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        queue.async { self.reportStop("capture stopped: \(error.localizedDescription) (\((error as NSError).code))") }
    }

    /// Once per stream, on the pipeline queue.
    private func reportStop(_ reason: String) {
        guard !stopped, !reportedStop else { return }
        reportedStop = true
        onStop?(reason)
    }
}

/// System sound (without bb1's own), from an audio-only stream of the same display. ScreenCaptureKit's
/// app filter leaves an app out of the picture and the sound alike, so the picture's stream keeps every
/// app and this one leaves out the muted apps. A muted app that starts later (or a helper that only
/// starts when it plays something, like Chrome's audio service) is picked up within a few seconds.
/// Control calls on `queue` (the pipeline queue); sound arrives on its own queue. Sendable because its
/// state is only touched on `queue` (the async ScreenCaptureKit calls hop back there).
final class SoundCapture: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let queue: DispatchQueue
    private let audioQueue = DispatchQueue(label: "bb1.audio", qos: .userInteractive)
    private var mutedApps: Set<String>
    private var displayID: CGDirectDisplayID?
    private var display: SCDisplay?
    private var stream: SCStream?
    private var starting = false
    private var stopped = false
    /// Processes left out right now (so the filter is only replaced when that changes).
    private var excluded: Set<pid_t>?
    private var checking = false
    private var timer: DispatchSourceTimer?
    private var observers: [NSObjectProtocol] = []
    var onAudio: ((CMSampleBuffer) -> Void)?

    init(queue: DispatchQueue, mutedApps: Set<String>) {
        self.queue = queue
        self.mutedApps = mutedApps
    }

    func start(displayID: CGDirectDisplayID) {
        guard !stopped, stream == nil, !starting else { return }
        self.displayID = displayID
        starting = true
        let muted = mutedApps
        Task {
            do {
                // Every app, not just ones with a window on screen: sound often comes from a helper.
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
                guard let display = content.displays.first(where: { $0.displayID == displayID }) ?? content.displays.first else {
                    throw CaptureError.noDisplay
                }
                let apps = SoundCapture.muted(in: content, muted)
                let cfg = SCStreamConfiguration()
                // The smallest picture it allows, once a second: only the sound is used.
                cfg.width = 2
                cfg.height = 2
                cfg.minimumFrameInterval = CMTime(value: 1, timescale: 1)
                cfg.queueDepth = 3
                cfg.showsCursor = false
                cfg.capturesAudio = true
                cfg.sampleRate = Int(AudioFormat.sampleRate)
                cfg.channelCount = AudioFormat.channels
                cfg.excludesCurrentProcessAudio = true
                let stream = SCStream(filter: SCContentFilter(display: display, excludingApplications: apps, exceptingWindows: []),
                                      configuration: cfg, delegate: self)
                // A screen output too, or ScreenCaptureKit logs every picture it has nowhere to send.
                try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: audioQueue)
                try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: audioQueue)
                try await stream.startCapture()
                queue.async {
                    self.starting = false
                    if self.stopped {
                        stream.stopCapture { _ in }
                        return
                    }
                    self.stream = stream
                    self.display = display
                    self.excluded = Set(apps.map(\.processID))
                    Log.info("sound: capturing system sound\(SoundCapture.describe(apps, muted))")
                    self.watchApps()
                    if muted != self.mutedApps { self.recheck() } // changed while it was starting
                }
            } catch {
                queue.async {
                    self.starting = false
                    Log.info("sound: capture failed to start: \(error.localizedDescription); trying again in 2 s")
                    self.retry()
                }
            }
        }
    }

    func stop() {
        stopped = true
        stream?.stopCapture { _ in }
        stream = nil
        timer?.cancel()
        timer = nil
        observers.forEach { NSWorkspace.shared.notificationCenter.removeObserver($0) }
        observers.removeAll()
    }

    func setMutedApps(_ ids: Set<String>) {
        guard ids != mutedApps else { return }
        mutedApps = ids
        Log.info("sound: apps to leave out changed to \(ids.isEmpty ? "none" : ids.sorted().joined(separator: ", "))")
        recheck()
    }

    private func retry() {
        queue.asyncAfter(deadline: .now() + 2) { [weak self] in
            guard let self, !self.stopped, self.stream == nil, let id = self.displayID else { return }
            self.start(displayID: id)
        }
    }

    /// Muted apps starting or quitting change what's left out. Helpers don't count as launched apps,
    /// so also look every few seconds while some app is muted.
    private func watchApps() {
        guard observers.isEmpty else { return }
        let center = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didLaunchApplicationNotification, NSWorkspace.didTerminateApplicationNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: nil) { [weak self] note in
                guard let self, let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
                      let id = app.bundleIdentifier, SoundApps.app(forBundleId: id) != nil else { return }
                self.queue.async { self.recheck() }
            })
        }
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 3, repeating: 3)
        timer.setEventHandler { [weak self] in
            guard let self, !self.mutedApps.isEmpty || self.excluded?.isEmpty == false else { return }
            self.recheck()
        }
        timer.resume()
        self.timer = timer
    }

    /// Look at what's running and update the filter if a different set of processes should be left out.
    private func recheck() {
        guard !stopped, let stream, let display, !checking else { return }
        checking = true
        let muted = mutedApps
        Task {
            let content = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            queue.async {
                self.checking = false
                guard let content, !self.stopped, self.stream === stream else { return }
                guard muted == self.mutedApps else { self.recheck(); return } // changed while looking
                let apps = SoundCapture.muted(in: content, muted)
                let pids = Set(apps.map(\.processID))
                guard pids != self.excluded else { return }
                self.excluded = pids
                let filter = SCContentFilter(display: display, excludingApplications: apps, exceptingWindows: [])
                stream.updateContentFilter(filter) { error in
                    if let error { Log.info("sound: couldn't change which apps are left out: \(error.localizedDescription)") }
                }
                Log.info("sound: now\(SoundCapture.describe(apps, muted))")
            }
        }
    }

    private static func muted(in content: SCShareableContent, _ muted: Set<String>) -> [SCRunningApplication] {
        guard !muted.isEmpty else { return [] }
        return content.applications.filter { SoundApps.isMuted($0.bundleIdentifier, muted: muted) }
    }

    /// " without Discord (com.hnc.Discord, com.hnc.Discord.helper)", for the log.
    private static func describe(_ apps: [SCRunningApplication], _ muted: Set<String>) -> String {
        if apps.isEmpty {
            return muted.isEmpty ? ", leaving nothing out" : ", leaving nothing out (none of \(muted.sorted().joined(separator: ", ")) is running)"
        }
        let names = Set(apps.compactMap { SoundApps.app(forBundleId: $0.bundleIdentifier)?.name }).sorted()
        let ids = Set(apps.map(\.bundleIdentifier)).sorted()
        return " without \(names.joined(separator: ", ")) (\(ids.joined(separator: ", ")))"
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio, sampleBuffer.isValid else { return }
        onAudio?(sampleBuffer)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        queue.async {
            guard self.stream === stream, !self.stopped else { return }
            Log.info("sound: capture stopped: \(error.localizedDescription) (\((error as NSError).code)); restarting in 2 s")
            self.stream = nil
            self.excluded = nil
            self.retry()
        }
    }
}

/// Moving test pattern, for testing without Screen Recording permission (`--synthetic`).
final class SyntheticSource: FrameSource {
    private let queue: DispatchQueue
    private let fps: Int
    private let width = 1920, height = 1080
    private var timer: DispatchSourceTimer?
    private var pool: CVPixelBufferPool?
    private var frameNumber = 0
    var onAudio: ((CMSampleBuffer) -> Void)?

    init(queue: DispatchQueue, fps: Int) {
        self.queue = queue
        self.fps = fps
    }

    func start(onFrame: @escaping (CVPixelBuffer, UInt64) -> Void,
               onStop: @escaping (String) -> Void,
               completion: @escaping (Result<(width: Int, height: Int), Error>) -> Void) {
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferWidthKey: width,
            kCVPixelBufferHeightKey: height,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: 1.0 / Double(fps), leeway: .milliseconds(1))
        timer.setEventHandler { [weak self] in
            guard let self, let buffer = self.makeFrame() else { return }
            onFrame(buffer, Clock.nowUs())
        }
        self.timer = timer
        timer.resume()
        Log.info("capture: synthetic test pattern \(width)x\(height) @\(fps)fps")
        queue.async { completion(.success((self.width, self.height))) }
    }

    func stop() {
        timer?.cancel()
        timer = nil
    }

    func setShowsCursor(_ show: Bool) {}
    func setMutedApps(_ ids: Set<String>) {}

    private func makeFrame() -> CVPixelBuffer? {
        guard let pool else { return nil }
        var out: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &out)
        guard let buffer = out else { return nil }
        frameNumber += 1
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }

        let yBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)!.assumingMemoryBound(to: UInt8.self)
        let yStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let boxX = (frameNumber * 12) % (width - 200)
        for y in 0..<height {
            let row = yBase + y * yStride
            memset(row, Int32(16 + (y * 200 / height)), width)
            if y >= 400 && y < 600 { memset(row + boxX, 235, 200) }
            // thin vertical lines to make blur visible
            if y < 100 { for x in stride(from: 0, to: width, by: 4) { row[x] = 235 } }
        }
        let uvBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)!
        let uvStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        memset(uvBase, 128, uvStride * height / 2)
        return buffer
    }
}
