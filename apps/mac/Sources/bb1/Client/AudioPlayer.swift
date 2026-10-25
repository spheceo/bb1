import AVFoundation
import BB1Core
import os

/// Plays the host's sound through an adaptive jitter buffer (see `AudioJitterBuffer`).
final class AudioPlayer {
    private let engine = AVAudioEngine()
    private var source: AVAudioSourceNode?
    let buffer = AudioJitterBuffer()
    private(set) var running = false
    let packets = Locked<UInt64>(0)

    var muted = false {
        didSet { engine.mainMixerNode.outputVolume = muted ? 0 : 1 }
    }

    func start() {
        guard !running else { return }
        let format = AVAudioFormat(standardFormatWithSampleRate: AudioFormat.sampleRate, channels: 2)!
        let buffer = self.buffer
        let node = AVAudioSourceNode(format: format) { _, _, frameCount, bufferList -> OSStatus in
            let buffers = UnsafeMutableAudioBufferListPointer(bufferList)
            guard buffers.count >= 2,
                  let left = buffers[0].mData?.assumingMemoryBound(to: Float.self),
                  let right = buffers[1].mData?.assumingMemoryBound(to: Float.self) else { return noErr }
            buffer.read(left: left, right: right, frames: Int(frameCount))
            return noErr
        }
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: format)
        engine.mainMixerNode.outputVolume = muted ? 0 : 1
        do {
            try engine.start()
            source = node
            running = true
            Log.info("audio: playback started")
        } catch {
            Log.info("audio: couldn't start playback: \(error.localizedDescription)")
        }
    }

    func stop() {
        guard running else { return }
        engine.stop()
        if let source { engine.detach(source) }
        source = nil
        running = false
        buffer.clear()
    }

    /// Network queue. `pcm` is 240 frames of interleaved Int16 stereo. Each packet arrives twice
    /// (the host resends it 10 ms later); duplicates are ignored.
    func receive(seq: UInt32, pcm: Data) {
        guard running else { return }
        packets.with { $0 += 1 }
        pcm.withUnsafeBytes { raw in buffer.write(seq: seq, raw.bindMemory(to: Int16.self)) }
    }
}

/// Packets are stored by sequence number, so late and duplicate copies land in the right place.
/// Playback starts once `target` packets are queued. Running dry raises the target (more cushion on
/// a choppy network, up to 150 ms); 15 s without trouble lowers it again (down to 30 ms). If too much
/// builds up, playback skips ahead so sound never lags behind the picture.
final class AudioJitterBuffer {
    private static let slots = 64
    private static let frames = AudioFormat.framesPerPacket
    static let minTarget = 6, maxTarget = 30   // packets of 5 ms

    private var lock = os_unfair_lock()
    private var left = [Float](repeating: 0, count: slots * frames)
    private var right = [Float](repeating: 0, count: slots * frames)
    private var slotSeq = [UInt32](repeating: 0, count: slots)
    private var slotValid = [Bool](repeating: false, count: slots)
    private var playSeq: UInt32?
    private var playOffset = 0
    private var highest: UInt32?
    private var buffering = true
    private var target = 10
    private var calmSinceUs: UInt64 = 0
    /// Ran dry; decide when the next packet arrives whether it was the network (late packet) or a pause.
    private var dryPending = false

    // Stats (read and reset once a second)
    private var concealed: UInt64 = 0
    private var underruns: UInt64 = 0
    private var skipped: UInt64 = 0

    func write(seq: UInt32, _ samples: UnsafeBufferPointer<Int16>) {
        os_unfair_lock_lock(&lock)
        defer { os_unfair_lock_unlock(&lock) }
        if let p = playSeq {
            if seq &- p > 0x8000_0000 { return }                  // already played
            if dryPending {
                dryPending = false
                if seq &- p <= 2 {
                    // The packet we ran out waiting for showed up late: the network needs more cushion.
                    underruns += 1
                    target = min(Self.maxTarget, target + 2)
                    calmSinceUs = Clock.nowUs()
                } else {
                    // The host paused (it doesn't send silence): start right at the new sound.
                    resetLocked()
                }
            } else if seq &- p >= UInt32(Self.slots) {
                resetLocked()                                     // far ahead: start over
            }
        }
        let slot = Int(seq % UInt32(Self.slots))
        if slotValid[slot] && slotSeq[slot] == seq { return }      // duplicate copy
        let base = slot * Self.frames
        for i in 0..<min(Self.frames, samples.count / 2) {
            left[base + i] = Float(samples[i * 2]) / 32768
            right[base + i] = Float(samples[i * 2 + 1]) / 32768
        }
        slotSeq[slot] = seq
        slotValid[slot] = true
        if playSeq == nil { playSeq = seq; playOffset = 0 }
        if highest == nil || seq &- highest! < 0x8000_0000 { highest = seq }
    }

    func read(left out0: UnsafeMutablePointer<Float>, right out1: UnsafeMutablePointer<Float>, frames: Int) {
        os_unfair_lock_lock(&lock)
        defer { os_unfair_lock_unlock(&lock) }
        let now = Clock.nowUs()
        if calmSinceUs == 0 { calmSinceUs = now }
        var i = 0
        if buffering, depth >= target { buffering = false }
        if !buffering, depth > target + 10, let h = highest {
            // Too far behind: jump to `target` packets before the newest.
            let newPlay = h &- UInt32(target) &+ 1
            while let p = playSeq, p != newPlay { invalidate(p); playSeq = p &+ 1; skipped += 1 }
            playOffset = 0
        }
        while i < frames && !buffering {
            guard let p = playSeq else { buffering = true; break }
            let slot = Int(p % UInt32(Self.slots))
            let have = slotValid[slot] && slotSeq[slot] == p
            if !have && depth <= 1 {
                buffering = true
                dryPending = true
                break
            }
            let n = min(frames - i, Self.frames - playOffset)
            if have {
                let base = slot * Self.frames + playOffset
                for k in 0..<n { out0[i + k] = left[base + k]; out1[i + k] = right[base + k] }
            } else {
                for k in 0..<n { out0[i + k] = 0; out1[i + k] = 0 }  // lost packet: brief silence
                if playOffset == 0 { concealed += 1 }
            }
            i += n
            playOffset += n
            if playOffset == Self.frames {
                invalidate(p)
                playSeq = p &+ 1
                playOffset = 0
            }
        }
        while i < frames { out0[i] = 0; out1[i] = 0; i += 1 }
        if now - calmSinceUs > 15_000_000, target > Self.minTarget {
            target -= 1
            calmSinceUs = now
        }
    }

    /// Packets queued from the play position to the newest one.
    private var depth: Int {
        guard let p = playSeq, let h = highest, h &- p < 0x8000_0000 else { return 0 }
        return Int(h &- p) + 1
    }

    private func invalidate(_ seq: UInt32) {
        let slot = Int(seq % UInt32(Self.slots))
        if slotSeq[slot] == seq { slotValid[slot] = false }
    }

    private func resetLocked() {
        for k in 0..<Self.slots { slotValid[k] = false }
        playSeq = nil
        highest = nil
        playOffset = 0
        buffering = true
        dryPending = false
    }

    func clear() {
        os_unfair_lock_lock(&lock)
        resetLocked()
        os_unfair_lock_unlock(&lock)
    }

    /// For the once-a-second stats line.
    func takeStats() -> (concealed: UInt64, underruns: UInt64, skipped: UInt64, bufferMs: Int) {
        os_unfair_lock_lock(&lock)
        defer {
            concealed = 0; underruns = 0; skipped = 0
            os_unfair_lock_unlock(&lock)
        }
        return (concealed, underruns, skipped, target * 5)
    }
}
