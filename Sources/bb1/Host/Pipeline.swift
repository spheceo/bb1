import BB1Core
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation

struct PipelineSettings {
    var codec: VideoCodec = .hevc
    var bitrateMbps: Int?          // nil = pick from resolution
    var fps = 60
    var synthetic = false
    var showsCursor = true
}

struct StreamInfo {
    let width: Int
    let height: Int
    let codec: VideoCodec
    let fps: Int
    let bitrateMbps: Int
    let lowLatency: Bool
    let epoch: UInt16
}

struct PipelineCounters {
    var captured: UInt64 = 0
    var submitted: UInt64 = 0
    var encoded: UInt64 = 0
    var keyframes: UInt64 = 0
    var encodeAvgUs: Double = 0
    var lastCaptureUs: UInt64 = 0
    var lastSubmitUs: UInt64 = 0
    var lastEncodedUs: UInt64 = 0
    /// When the oldest frame still waiting for encoder output was submitted (0 = nothing waiting).
    var pendingSinceUs: UInt64 = 0
    var recoveries: [String: Int] = [:]
    var lastError: String?
    // Reset by the host's once-a-second stats line.
    var encodeMaxUs: UInt64 = 0
    var biggestFrameBytes = 0

    var recoveryTotal: UInt64 { UInt64(recoveries.values.reduce(0, +)) }
}

/// Capture → encode, with its own health checks. Runs only while a client is connected.
final class StreamPipeline {
    let queue = DispatchQueue(label: "bb1.pipeline", qos: .userInteractive)
    var settings: PipelineSettings

    /// Encoder thread.
    var onEncodedFrame: ((EncodedFrame) -> Void)?
    /// Audio queue. System sound from the capture stream.
    var onAudio: ((CMSampleBuffer) -> Void)?
    /// Pipeline queue. macOS stopped the screen capture (it restarts by itself).
    var onCaptureStopped: ((String) -> Void)?
    /// Pipeline queue. Called when resolution/codec/epoch changes.
    var onStreamInfo: ((StreamInfo) -> Void)?

    let counters = Locked(PipelineCounters())
    private(set) var info: StreamInfo?

    private var running = false
    private var generation = 0
    private var source: FrameSource?
    private var captureStartedUs: UInt64 = 0
    private var encoderCreatedUs: UInt64 = 0
    private var lastCaptureRestartUs: UInt64 = 0
    private var captureRetryDelay = 0.3
    private var encoder: VideoEncoder?
    private var epoch: UInt16 = 0
    private var lastBuffer: CVPixelBuffer?
    private var forceKeyframe = true
    /// The share of the full bitrate the network carries right now (0.15…1, set by the host).
    private var bitrateScale = 1.0
    private var lastKeyframeRequestUs: UInt64 = 0
    private var refreshWork: DispatchWorkItem?
    // Fault injection (debug menu / signals), to prove each recovery path works.
    private var faultFreezeCapture = false
    private var faultEncoderStalled = Locked(false)

    init(settings: PipelineSettings) {
        self.settings = settings
    }

    // MARK: Control (any thread)

    func start() { queue.async { self.startInternal() } }

    func stop() { queue.async { self.stopInternal(dropEncoder: false) } }

    /// The hardware encoder takes ~2.5 s to start the first time on this M1. Start it when sharing
    /// turns on, so a connecting client doesn't wait for it. It's kept between sessions.
    func prewarm() {
        queue.async {
            guard self.encoder == nil else { return }
            let size: (Int, Int)
            if self.settings.synthetic {
                size = (1920, 1080)
            } else {
                guard let mode = CGDisplayCopyDisplayMode(CGMainDisplayID()) else { return }
                size = (mode.pixelWidth, mode.pixelHeight)
            }
            let started = Clock.nowUs()
            self.makeEncoder(width: size.0, height: size.1)
            Log.info("pipeline: encoder ready in advance (\((Clock.nowUs() - started) / 1000) ms)")
        }
    }

    func requestKeyframe(_ reason: String) {
        queue.async {
            self.forceKeyframe = true
            let now = Clock.nowUs()
            // If the screen is idle no new frame will come, so re-encode the last one right away.
            if let buffer = self.lastBuffer, now - self.counters.get.lastCaptureUs > 30_000,
               now - self.lastKeyframeRequestUs > 100_000 {
                self.lastKeyframeRequestUs = now
                self.encode(buffer, captureUs: now, refresh: false)
            }
            Log.info("pipeline: keyframe requested (\(reason))")
        }
    }

    func setShowsCursor(_ show: Bool) {
        queue.async {
            self.settings.showsCursor = show
            self.source?.setShowsCursor(show)
        }
    }

    func restartAll(_ reason: String) {
        queue.async {
            guard self.running else { return }
            self.recordRecovery(reason)
            // After sleep or a display change, start from a fresh encoder.
            self.stopInternal(dropEncoder: true)
            self.startInternal()
        }
    }

    enum Fault: String, CaseIterable {
        case captureStops = "capture stops with an error"
        case captureFreezes = "capture silently stops sending frames"
        case encoderStalls = "encoder silently stops producing frames"
    }

    func inject(_ fault: Fault) {
        queue.async {
            Log.info("pipeline: FAULT INJECTED: \(fault.rawValue)")
            switch fault {
            case .captureStops: self.scheduleCaptureRetry("injected: capture stopped")
            case .captureFreezes: self.faultFreezeCapture = true
            case .encoderStalls: self.faultEncoderStalled.with { $0 = true }
            }
        }
    }

    /// Called every 250 ms by the host. `lastPointerUs` is when pointer input last arrived.
    func checkHealth(lastPointerUs: UInt64, recentInputUs: [UInt64] = []) {
        queue.async {
            guard self.running, self.source != nil, self.encoder != nil else { return }
            let now = Clock.nowUs()
            let c = self.counters.get
            // Pointer moved (the cursor is in the video, so the screen changed) but capture produced nothing.
            if self.settings.showsCursor, !self.settings.synthetic,
               lastPointerUs > c.lastCaptureUs, now - lastPointerUs > 600_000,
               now - self.captureStartedUs > 2_000_000, now - self.lastCaptureRestartUs > 3_000_000,
               now - self.encoderCreatedUs > 2_000_000 {
                self.restartCapture("capture stalled after input")
                return
            }
            // Without the cursor in the video, use clicks and keys instead. One click can easily change
            // nothing, so it takes 3+ inputs and 4 s without a single new frame → restart capture (invisible).
            let unanswered = recentInputUs.filter { $0 > c.lastCaptureUs }
            if !self.settings.showsCursor, !self.settings.synthetic,
               unanswered.count >= 3, let first = unanswered.min(), now - first > 4_000_000,
               now - self.captureStartedUs > 5_000_000, now - self.lastCaptureRestartUs > 10_000_000,
               now - self.encoderCreatedUs > 2_000_000 {
                self.restartCapture("capture silent after input")
                return
            }
            // A frame went into the encoder and nothing has come out since.
            if c.pendingSinceUs > 0, now - c.pendingSinceUs > 800_000 {
                self.rebuildEncoder("encoder stalled")
            }
        }
    }

    // MARK: Internals (pipeline queue)

    private func startInternal() {
        guard !running else { return }
        running = true
        forceKeyframe = true
        counters.with { $0.pendingSinceUs = 0 }
        encoderCreatedUs = Clock.nowUs() // give a reused encoder the same grace period as a new one
        startCapture()
    }

    private func stopInternal(dropEncoder: Bool) {
        running = false
        generation += 1
        refreshWork?.cancel()
        source?.stop()
        source = nil
        lastBuffer = nil
        forceKeyframe = true
        if dropEncoder {
            encoder?.invalidate()
            encoder = nil
            info = nil
        }
    }

    /// Sharing turned off: release the encoder too.
    func shutdown() { queue.sync { self.stopInternal(dropEncoder: true) } }

    private func startCapture() {
        generation += 1
        let gen = generation
        let source: FrameSource = settings.synthetic
            ? SyntheticSource(queue: queue, fps: settings.fps)
            : ScreenSource(queue: queue, fps: settings.fps, showsCursor: settings.showsCursor)
        self.source = source
        source.onAudio = { [weak self] sample in
            guard let self, gen == self.generation else { return }
            self.onAudio?(sample)
        }
        captureStartedUs = Clock.nowUs()
        source.start(onFrame: { [weak self] buffer, captureUs in
            guard let self, gen == self.generation else { return }
            self.handleFrame(buffer, captureUs: captureUs)
        }, onStop: { [weak self] reason in
            guard let self, gen == self.generation else { return }
            Log.info("pipeline: \(reason)")
            self.onCaptureStopped?(reason)
            self.scheduleCaptureRetry(reason)
        }, completion: { [weak self] result in
            guard let self, gen == self.generation else { return }
            switch result {
            case .success(let size):
                self.captureRetryDelay = 0.3
                self.counters.with { $0.lastError = nil }
                self.ensureEncoder(width: size.width, height: size.height)
            case .failure(let error):
                Log.info("pipeline: capture failed to start: \(error.localizedDescription)")
                self.counters.with { $0.lastError = "Screen capture: \(error.localizedDescription)" }
                self.scheduleCaptureRetry("capture failed to start")
            }
        })
    }

    private func scheduleCaptureRetry(_ reason: String) {
        source?.stop()
        source = nil
        generation += 1
        let delay = captureRetryDelay
        captureRetryDelay = min(captureRetryDelay * 2, 5)
        recordRecovery(reason)
        queue.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self, self.running, self.source == nil else { return }
            self.startCapture()
        }
    }

    private func restartCapture(_ reason: String) {
        Log.info("pipeline: restarting capture (\(reason))")
        recordRecovery(reason)
        lastCaptureRestartUs = Clock.nowUs()
        source?.stop()
        source = nil
        forceKeyframe = true
        faultFreezeCapture = false
        startCapture()
    }

    private func ensureEncoder(width: Int, height: Int) {
        if let encoder, encoder.width == width, encoder.height == height { return }
        makeEncoder(width: width, height: height)
    }

    private func rebuildEncoder(_ reason: String) {
        guard let encoder else { return }
        Log.info("pipeline: rebuilding encoder (\(reason))")
        recordRecovery(reason)
        makeEncoder(width: encoder.width, height: encoder.height)
        if let buffer = lastBuffer { encode(buffer, captureUs: Clock.nowUs(), refresh: false) }
    }

    private func makeEncoder(width: Int, height: Int) {
        faultEncoderStalled.with { $0 = false }
        encoder?.invalidate()
        encoder = nil
        epoch &+= 1
        let bitrate = bitrate(width: width, height: height)
        do {
            let e = try VideoEncoder(width: width, height: height, preferred: settings.codec,
                                     bitrateMbps: bitrate, fps: settings.fps, epoch: epoch)
            e.onOutput = { [weak self] frame in self?.handleEncoded(frame) }
            e.onError = { [weak self] message in
                guard let self else { return }
                self.queue.async { self.rebuildEncoder(message) }
            }
            encoder = e
            encoderCreatedUs = Clock.nowUs()
            forceKeyframe = true
            counters.with { $0.lastSubmitUs = 0; $0.lastEncodedUs = 0; $0.pendingSinceUs = 0 }
            let info = StreamInfo(width: width, height: height, codec: e.codec, fps: settings.fps,
                                  bitrateMbps: bitrate, lowLatency: e.lowLatency, epoch: epoch)
            self.info = info
            onStreamInfo?(info)
        } catch {
            Log.info("pipeline: \(error.localizedDescription)")
            counters.with { $0.lastError = error.localizedDescription }
        }
    }

    private func bitrate(width: Int, height: Int) -> Int {
        let full = settings.bitrateMbps ?? StreamPipeline.defaultBitrate(width: width, height: height)
        return max(3, Int((Double(full) * bitrateScale).rounded()))
    }

    /// Send less (or more again) to match what the network carries. Takes effect on the next frame.
    func setBitrateScale(_ scale: Double) {
        queue.async {
            guard scale != self.bitrateScale else { return }
            self.bitrateScale = scale
            guard let encoder = self.encoder else { return }
            encoder.setBitrate(self.bitrate(width: encoder.width, height: encoder.height))
        }
    }

    static func defaultBitrate(width: Int, height: Int) -> Int {
        let scale = Double(width * height) / Double(1920 * 1080)
        return max(20, min(80, Int(30 * scale)))
    }

    private func handleFrame(_ buffer: CVPixelBuffer, captureUs: UInt64) {
        if faultFreezeCapture { return }
        counters.with {
            $0.captured += 1
            $0.lastCaptureUs = Clock.nowUs()
        }
        lastBuffer = buffer
        encode(buffer, captureUs: captureUs, refresh: false)
        scheduleRefresh()
    }

    /// When the screen stops changing, re-encode the last frame twice so static text sharpens up.
    private func scheduleRefresh() {
        refreshWork?.cancel()
        let gen = generation
        func refresh(after delay: Double, remaining: Int) {
            let work = DispatchWorkItem { [weak self] in
                guard let self, gen == self.generation, let buffer = self.lastBuffer else { return }
                self.encode(buffer, captureUs: Clock.nowUs(), refresh: true)
                if remaining > 1 { refresh(after: 0.25, remaining: remaining - 1) }
            }
            refreshWork = work
            queue.asyncAfter(deadline: .now() + delay, execute: work)
        }
        refresh(after: 0.12, remaining: 2)
    }

    private func encode(_ buffer: CVPixelBuffer, captureUs: UInt64, refresh: Bool) {
        guard let encoder else { return }
        let keyframe = forceKeyframe
        forceKeyframe = false
        counters.with {
            $0.submitted += 1
            $0.lastSubmitUs = Clock.nowUs()
            if $0.pendingSinceUs == 0 { $0.pendingSinceUs = $0.lastSubmitUs }
        }
        let status = encoder.encode(buffer, captureUs: captureUs, forceKeyframe: keyframe, refresh: refresh)
        if status != noErr {
            forceKeyframe = true
            rebuildEncoder("encode call failed (\(status))")
        }
    }

    private func handleEncoded(_ frame: EncodedFrame) {
        if faultEncoderStalled.get { return }
        let now = Clock.nowUs()
        counters.with {
            $0.encoded += 1
            $0.lastEncodedUs = now
            $0.pendingSinceUs = 0
            if frame.isKeyframe { $0.keyframes += 1 }
            if !frame.isRefresh {
                let raw = now > frame.captureUs ? now - frame.captureUs : 0
                let us = Double(raw)
                $0.encodeAvgUs = $0.encodeAvgUs == 0 ? us : $0.encodeAvgUs * 0.95 + us * 0.05
                $0.encodeMaxUs = max($0.encodeMaxUs, raw)
            }
            $0.biggestFrameBytes = max($0.biggestFrameBytes, frame.payload.count)
        }
        onEncodedFrame?(frame)
    }

    private func recordRecovery(_ reason: String) {
        counters.with { $0.recoveries[reason, default: 0] += 1 }
    }
}
