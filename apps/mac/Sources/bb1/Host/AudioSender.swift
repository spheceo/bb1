import AVFoundation
import BB1Core
import CoreMedia
import Foundation

/// Turns ScreenCaptureKit's sound (float32, usually non-interleaved) into 5 ms chunks of
/// interleaved 16-bit stereo. Runs on the capture's audio queue.
final class AudioChunker {
    private var pending: [Int16] = []
    /// Called with each 240-frame chunk and the capture time of its first frame.
    var onChunk: ((Data, UInt64) -> Void)?
    private var loggedFormat = false

    func add(_ sample: CMSampleBuffer) {
        guard let format = CMSampleBufferGetFormatDescription(sample),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(format)?.pointee else { return }
        if !loggedFormat {
            loggedFormat = true
            Log.info("audio: capturing \(Int(asbd.mSampleRate)) Hz, \(asbd.mChannelsPerFrame) ch, \(asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 ? "planar" : "interleaved") float")
        }
        guard asbd.mFormatID == kAudioFormatLinearPCM, asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0,
              asbd.mBitsPerChannel == 32 else { return }

        var blockBuffer: CMBlockBuffer?
        var listSize = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: &listSize, bufferListOut: nil,
                                                                bufferListSize: 0, blockBufferAllocator: nil,
                                                                blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: nil)
        let raw = UnsafeMutableRawPointer.allocate(byteCount: listSize, alignment: MemoryLayout<AudioBufferList>.alignment)
        defer { raw.deallocate() }
        let list = raw.bindMemory(to: AudioBufferList.self, capacity: 1)
        guard CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample, bufferListSizeNeededOut: nil, bufferListOut: list, bufferListSize: listSize,
            blockBufferAllocator: nil, blockBufferMemoryAllocator: nil,
            flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, blockBufferOut: &blockBuffer) == noErr else { return }

        let buffers = UnsafeMutableAudioBufferListPointer(list)
        let frames = CMSampleBufferGetNumSamples(sample)
        let channels = Int(asbd.mChannelsPerFrame)
        let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0

        func sampleAt(_ frame: Int, _ channel: Int) -> Float {
            let ch = min(channel, channels - 1) // mono → both sides
            if planar {
                guard ch < buffers.count, let data = buffers[ch].mData else { return 0 }
                return data.assumingMemoryBound(to: Float.self)[frame]
            }
            guard let data = buffers[0].mData else { return 0 }
            return data.assumingMemoryBound(to: Float.self)[frame * channels + ch]
        }

        pending.reserveCapacity(pending.count + frames * 2)
        for f in 0..<frames {
            for c in 0..<2 {
                let v = max(-1, min(1, sampleAt(f, c)))
                pending.append(Int16(v * 32767))
            }
        }

        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        let endUs = pts.isValid ? UInt64(max(0, pts.seconds) * 1_000_000) + UInt64(Double(frames) / AudioFormat.sampleRate * 1_000_000) : Clock.nowUs()
        let perChunk = AudioFormat.framesPerPacket * 2
        while pending.count >= perChunk {
            let chunk = pending.prefix(perChunk)
            pending.removeFirst(perChunk)
            let leftUs = UInt64(Double(pending.count / 2) / AudioFormat.sampleRate * 1_000_000)
            let chunkUs = UInt64(Double(AudioFormat.framesPerPacket) / AudioFormat.sampleRate * 1_000_000)
            let data = chunk.withUnsafeBufferPointer { Data(buffer: $0) }
            onChunk?(data, endUs &- leftUs &- chunkUs)
        }
    }

    func reset() { pending.removeAll() }
}
