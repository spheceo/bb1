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
    /// System sound, on its own queue. Only the screen source has sound.
    var onAudio: ((CMSampleBuffer) -> Void)? { get set }
}

enum CaptureError: LocalizedError {
    case noDisplay
    var errorDescription: String? { "no display found to capture" }
}

/// Captures the main display with ScreenCaptureKit at native pixel resolution.
final class ScreenSource: NSObject, FrameSource, SCStreamOutput, SCStreamDelegate {
    private let queue: DispatchQueue
    private let fps: Int
    private var showsCursor: Bool
    private var stream: SCStream?
    private var config: SCStreamConfiguration?
    private var stopped = false
    private var onFrame: ((CVPixelBuffer, UInt64) -> Void)?
    private var onStop: ((String) -> Void)?
    private let audioQueue = DispatchQueue(label: "bb1.audio", qos: .userInteractive)
    var onAudio: ((CMSampleBuffer) -> Void)?

    init(queue: DispatchQueue, fps: Int, showsCursor: Bool) {
        self.queue = queue
        self.fps = fps
        self.showsCursor = showsCursor
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
                guard let display = content.displays.first(where: { $0.displayID == mainID }) ?? content.displays.first else {
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
                // System sound, without bb1's own.
                cfg.capturesAudio = true
                cfg.sampleRate = Int(AudioFormat.sampleRate)
                cfg.channelCount = AudioFormat.channels
                cfg.excludesCurrentProcessAudio = true

                let stream = SCStream(filter: SCContentFilter(display: display, excludingWindows: []),
                                      configuration: cfg, delegate: self)
                try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
                try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: audioQueue)
                try await stream.startCapture()
                queue.async {
                    if self.stopped {
                        stream.stopCapture { _ in }
                        return
                    }
                    self.stream = stream
                    self.config = cfg
                    Log.info("capture: started display \(display.displayID) at \(width)x\(height) @\(self.fps)fps, cursor \(self.showsCursor ? "in video" : "hidden")")
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
    }

    func setShowsCursor(_ show: Bool) {
        showsCursor = show
        guard let stream, let config else { return }
        config.showsCursor = show
        stream.updateConfiguration(config) { error in
            if let error { Log.info("capture: couldn't change cursor setting: \(error.localizedDescription)") }
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        if type == .audio {
            if sampleBuffer.isValid { onAudio?(sampleBuffer) }
            return
        }
        guard type == .screen, sampleBuffer.isValid,
              let attachments = (CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]])?.first,
              let rawStatus = attachments[.status] as? Int,
              SCFrameStatus(rawValue: rawStatus) == .complete,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        var captureUs = Clock.nowUs()
        if let displayTime = attachments[.displayTime] as? UInt64 { captureUs = Clock.machToUs(displayTime) }
        onFrame?(pixelBuffer, captureUs)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        queue.async {
            guard !self.stopped else { return }
            self.onStop?("capture stopped: \(error.localizedDescription)")
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
