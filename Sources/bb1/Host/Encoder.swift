import BB1Core
import CoreMedia
import Foundation
import VideoToolbox

struct EncodedFrame {
    let payload: Data          // FramePayload.encode()
    let isKeyframe: Bool
    let isRefresh: Bool
    let captureUs: UInt64
    let epoch: UInt16
}

enum EncoderError: LocalizedError {
    case create(OSStatus)
    var errorDescription: String? {
        if case .create(let status) = self { return "couldn't create a hardware video encoder (\(status))" }
        return nil
    }
}

/// Hardware encoder with low-latency settings. Tries the preferred codec with Apple's
/// low-latency rate control first, then falls back (docs/03-latency-and-quality.md).
final class VideoEncoder {
    let width: Int
    let height: Int
    let epoch: UInt16
    private(set) var codec: VideoCodec = .hevc
    private(set) var lowLatency = false
    private var session: VTCompressionSession?
    private var lastPtsUs: UInt64 = 0

    /// Called on a VideoToolbox thread.
    var onOutput: ((EncodedFrame) -> Void)?
    var onError: ((String) -> Void)?

    init(width: Int, height: Int, preferred: VideoCodec, bitrateMbps: Int, fps: Int, epoch: UInt16) throws {
        self.width = width
        self.height = height
        self.epoch = epoch
        let other: VideoCodec = preferred == .hevc ? .h264 : .hevc
        let attempts: [(VideoCodec, Bool)] = [(preferred, true), (preferred, false), (other, true), (other, false)]
        var lastStatus: OSStatus = 0
        for (codec, lowLatency) in attempts {
            var spec: [CFString: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true]
            if lowLatency { spec[kVTVideoEncoderSpecification_EnableLowLatencyRateControl] = true }
            var s: VTCompressionSession?
            lastStatus = VTCompressionSessionCreate(
                allocator: nil, width: Int32(width), height: Int32(height),
                codecType: codec == .hevc ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264,
                encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil,
                compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s)
            if lastStatus == noErr, let s {
                session = s
                self.codec = codec
                self.lowLatency = lowLatency
                break
            }
            Log.info("encoder: \(codec.name)\(lowLatency ? " low-latency" : "") unavailable (\(lastStatus)), trying next")
        }
        guard let session else { throw EncoderError.create(lastStatus) }

        func set(_ key: CFString, _ value: CFTypeRef) {
            let status = VTSessionSetProperty(session, key: key, value: value)
            if status != noErr { Log.info("encoder: couldn't set \(key) (\(status))") }
        }
        set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
        set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)
        set(kVTCompressionPropertyKey_ProfileLevel,
            codec == .hevc ? kVTProfileLevel_HEVC_Main_AutoLevel : kVTProfileLevel_H264_High_AutoLevel)
        set(kVTCompressionPropertyKey_AverageBitRate, NSNumber(value: bitrateMbps * 1_000_000))
        set(kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: fps))
        // Keyframes only when the client asks for one: periodic keyframes cause bitrate spikes.
        set(kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: Int32.max))
        set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, NSNumber(value: 24 * 3600))
        set(kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2)
        set(kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2)
        set(kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2)
        if !lowLatency {
            set(kVTCompressionPropertyKey_MaxFrameDelayCount, NSNumber(value: 0))
        }
        VTCompressionSessionPrepareToEncodeFrames(session)
        Log.info("encoder: \(codec.name)\(lowLatency ? " low-latency" : "") \(width)x\(height) \(bitrateMbps) Mbps, epoch \(epoch)")
    }

    deinit { invalidate() }

    func invalidate() {
        guard let session else { return }
        VTCompressionSessionInvalidate(session)
        self.session = nil
    }

    @discardableResult
    func encode(_ pixelBuffer: CVPixelBuffer, captureUs: UInt64, forceKeyframe: Bool, refresh: Bool) -> OSStatus {
        guard let session else { return kVTInvalidSessionErr }
        // Timestamps must increase even when we re-encode the same captured frame.
        let pts = max(captureUs, lastPtsUs + 1)
        lastPtsUs = pts
        let props: CFDictionary? = forceKeyframe ? [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue!] as CFDictionary : nil
        let codec = self.codec, epoch = self.epoch
        return VTCompressionSessionEncodeFrame(
            session, imageBuffer: pixelBuffer,
            presentationTimeStamp: CMTime(value: Int64(pts), timescale: 1_000_000),
            duration: .invalid, frameProperties: props, infoFlagsOut: nil
        ) { [weak self] status, flags, sampleBuffer in
            guard let self else { return }
            if status != noErr {
                self.onError?("encode failed (\(status))")
                return
            }
            guard !flags.contains(.frameDropped), let sampleBuffer,
                  let block = CMSampleBufferGetDataBuffer(sampleBuffer) else { return }
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[CFString: Any]]
            let notSync = attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false
            let isKeyframe = !notSync

            let length = CMBlockBufferGetDataLength(block)
            var data = Data(count: length)
            data.withUnsafeMutableBytes { _ = CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: $0.baseAddress!) }

            var sets: [Data] = []
            if isKeyframe, let format = CMSampleBufferGetFormatDescription(sampleBuffer) {
                sets = VideoEncoder.parameterSets(format, codec: codec)
            }
            let payload = FramePayload(codec: codec, parameterSets: sets, sampleData: data).encode()
            self.onOutput?(EncodedFrame(payload: payload, isKeyframe: isKeyframe, isRefresh: refresh,
                                        captureUs: captureUs, epoch: epoch))
        }
    }

    private static func parameterSets(_ format: CMFormatDescription, codec: VideoCodec) -> [Data] {
        var sets: [Data] = []
        var count = 0
        var headerLength: Int32 = 0
        var index = 0
        repeat {
            var pointer: UnsafePointer<UInt8>?
            var size = 0
            let status: OSStatus
            if codec == .hevc {
                status = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &size, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &headerLength)
            } else {
                status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &size, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &headerLength)
            }
            guard status == noErr, let pointer else { break }
            sets.append(Data(bytes: pointer, count: size))
            index += 1
        } while index < count
        return sets
    }
}
