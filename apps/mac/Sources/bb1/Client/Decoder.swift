import AVFoundation
import BB1Core
import CoreMedia
import Foundation
import VideoToolbox

/// Hardware decoder. Recreates its session whenever new parameter sets arrive.
final class VideoDecoder {
    let queue = DispatchQueue(label: "bb1.decode", qos: .userInteractive)
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var parameterSets: [Data] = []
    private var codec: VideoCodec?

    /// Decode queue.
    var onFrame: ((CVPixelBuffer, UInt64) -> Void)?
    /// Decode queue. The connection asks for a keyframe when this fires.
    var onError: ((String) -> Void)?

    let decoded = Locked<UInt64>(0)
    let decodeAvgUs = Locked<Double>(0)

    func submit(_ payload: FramePayload, captureUs: UInt64) {
        queue.async { self.decode(payload, captureUs: captureUs) }
    }

    func reset() {
        queue.async { self.invalidate() }
    }

    private func invalidate() {
        if let session { VTDecompressionSessionInvalidate(session) }
        session = nil
        format = nil
        parameterSets = []
        codec = nil
    }

    private func decode(_ payload: FramePayload, captureUs: UInt64) {
        if !payload.parameterSets.isEmpty && (payload.parameterSets != parameterSets || payload.codec != codec) {
            invalidate()
            guard makeSession(payload) else {
                onError?("couldn't create a decoder for this stream")
                return
            }
        }
        guard let session, let format else {
            onError?("no decoder yet")
            return
        }
        guard let sample = VideoDecoder.sampleBuffer(payload.sampleData, format: format) else {
            onError?("bad frame data")
            return
        }
        let started = Clock.nowUs()
        let status = VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample, flags: [._1xRealTimePlayback],
                                                       infoFlagsOut: nil) { [weak self] status, _, imageBuffer, _, _ in
            guard let self else { return }
            guard status == noErr, let imageBuffer else {
                self.onError?("decode failed (\(status))")
                return
            }
            self.decoded.with { $0 += 1 }
            let us = Double(Clock.nowUs() - started)
            self.decodeAvgUs.with { $0 = $0 == 0 ? us : $0 * 0.95 + us * 0.05 }
            self.onFrame?(imageBuffer, captureUs)
        }
        if status != noErr {
            // Typically kVTInvalidSessionErr after sleep or a GPU change: start over from the next keyframe.
            invalidate()
            onError?("decoder session failed (\(status))")
        }
    }

    private func makeSession(_ payload: FramePayload) -> Bool {
        guard let fd = VideoDecoder.formatDescription(payload.codec, payload.parameterSets) else { return false }
        let spec: [CFString: Any] = [kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true]
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var s: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(allocator: nil, formatDescription: fd,
                                                  decoderSpecification: spec as CFDictionary,
                                                  imageBufferAttributes: attrs as CFDictionary,
                                                  outputCallback: nil, decompressionSessionOut: &s)
        guard status == noErr, let s else {
            Log.info("decoder: create failed (\(status))")
            return false
        }
        VTSessionSetProperty(s, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        session = s
        format = fd
        parameterSets = payload.parameterSets
        codec = payload.codec
        let dims = CMVideoFormatDescriptionGetDimensions(fd)
        Log.info("decoder: \(payload.codec.name) \(dims.width)x\(dims.height)")
        return true
    }

    private static func formatDescription(_ codec: VideoCodec, _ sets: [Data]) -> CMVideoFormatDescription? {
        let buffers = sets.map { d -> UnsafeMutablePointer<UInt8> in
            let p = UnsafeMutablePointer<UInt8>.allocate(capacity: max(d.count, 1))
            d.copyBytes(to: p, count: d.count)
            return p
        }
        defer { buffers.forEach { $0.deallocate() } }
        let pointers = buffers.map { UnsafePointer($0) }
        let sizes = sets.map(\.count)
        var fd: CMFormatDescription?
        let status = pointers.withUnsafeBufferPointer { pp in
            sizes.withUnsafeBufferPointer { sp in
                codec == .hevc
                    ? CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                        allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                        parameterSetPointers: pp.baseAddress!, parameterSetSizes: sp.baseAddress!,
                        nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fd)
                    : CMVideoFormatDescriptionCreateFromH264ParameterSets(
                        allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                        parameterSetPointers: pp.baseAddress!, parameterSetSizes: sp.baseAddress!,
                        nalUnitHeaderLength: 4, formatDescriptionOut: &fd)
            }
        }
        if status != noErr { Log.info("decoder: bad parameter sets (\(status))") }
        return status == noErr ? fd : nil
    }

    private static func sampleBuffer(_ data: Data, format: CMVideoFormatDescription) -> CMSampleBuffer? {
        var block: CMBlockBuffer?
        let n = data.count
        guard n > 0, CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: n, blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil, offsetToData: 0, dataLength: n, flags: kCMBlockBufferAssureMemoryNowFlag,
            blockBufferOut: &block) == noErr, let block else { return nil }
        let copied = data.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: n)
        }
        guard copied == noErr else { return nil }
        var sample: CMSampleBuffer?
        var size = n
        guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
                                        sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
                                        sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample) == noErr
        else { return nil }
        return sample
    }
}

/// Shows decoded frames immediately: no buffering, no frame pacing.
final class FrameRenderer {
    let layer: AVSampleBufferDisplayLayer
    private var format: CMVideoFormatDescription?
    let failures = Locked<UInt64>(0)

    init(layer: AVSampleBufferDisplayLayer) { self.layer = layer }

    /// Any thread (we call it from the decode queue).
    func enqueue(_ pixelBuffer: CVPixelBuffer) {
        if format == nil || !CMVideoFormatDescriptionMatchesImageBuffer(format!, imageBuffer: pixelBuffer) {
            var fd: CMVideoFormatDescription?
            CMVideoFormatDescriptionCreateForImageBuffer(allocator: nil, imageBuffer: pixelBuffer, formatDescriptionOut: &fd)
            format = fd
        }
        guard let format else { return }
        var timing = CMSampleTimingInfo(duration: .invalid,
                                        presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
                                        decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReadyWithImageBuffer(allocator: nil, imageBuffer: pixelBuffer, formatDescription: format,
                                                       sampleTiming: &timing, sampleBufferOut: &sample) == noErr,
              let sample else { return }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        let renderer = layer.sampleBufferRenderer
        if renderer.status == .failed || renderer.requiresFlushToResumeDecoding {
            failures.with { $0 += 1 }
            renderer.flush(removingDisplayedImage: false, completionHandler: nil)
        }
        renderer.enqueue(sample)
    }

    func reset() {
        layer.sampleBufferRenderer.flush(removingDisplayedImage: false, completionHandler: nil)
        format = nil
    }
}
