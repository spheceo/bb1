import BB1Core
import CoreVideo
import Foundation
import Network

struct ClientTarget {
    var endpoint: NWEndpoint
    var label: String
    var auth: ControlChannel.ClientAuth
    /// The Tailscale address we're connecting to, if this goes over Tailscale.
    var tailscaleIP: String?
}

struct ClientCounters {
    var received: UInt64 = 0
    /// Frames handed to the decoder (received minus the ones skipped while waiting for a keyframe).
    var accepted: UInt64 = 0
    var displayed: UInt64 = 0
    var lostFrames: UInt64 = 0
    var keyframeRequests: UInt64 = 0
    var decoderResets: UInt64 = 0
    var displayResets: UInt64 = 0
    var videoPathResets: UInt64 = 0
    var reconnects: UInt64 = 0
    var bytes: UInt64 = 0
    var nacksSent: UInt64 = 0
    var recoveredByResend: UInt64 = 0
}

struct ClientStats {
    var config: StreamConfig?
    var fps: Double = 0
    var mbps: Double = 0
    var rttMs: Double = 0
    var latencyMs: Double?
    var latencyMaxMs: Double?
    var hostEncodeMs: Double = 0
    var decodeMs: Double = 0
    var counters = ClientCounters()
    var hostRecoveries: UInt64 = 0
    var connected = false
    /// "home network", "Tailscale · direct", "Tailscale · relayed via jnb", …
    var path = ""

    var recoveries: UInt64 {
        counters.decoderResets + counters.displayResets + counters.videoPathResets + counters.reconnects
    }

    var summary: String {
        var s = ""
        if let c = config { s += "\(c.width)×\(c.height) \(c.codec.name)\(c.lowLatencyEncoder ? " LL" : "") · " }
        if !path.isEmpty { s += "\(path) · " }
        s += String(format: "%.0f fps · %.1f Mbps · RTT %.1f ms", fps, mbps, rttMs)
        if let l = latencyMs, let m = latencyMaxMs { s += String(format: " · capture→shown %.1f ms (max %.0f)", l, m) }
        s += String(format: " · encode %.1f ms · decode %.1f ms", hostEncodeMs, decodeMs)
        s += " · lost \(counters.lostFrames) · resent ok \(counters.recoveredByResend) · keyframe req \(counters.keyframeRequests)"
        s += " · recoveries \(recoveries) (host \(hostRecoveries))"
        return s
    }
}

/// Connection to one host: handshake, video path, heartbeats, stall detection and automatic recovery
/// (docs/04-reliability.md). All state lives on `queue`.
final class ClientConnection {
    private(set) var target: ClientTarget
    /// Main thread: ask the user for the 6-digit code shown on the host (nil = cancel).
    var askCode: ((String, @escaping (String?) -> Void) -> Void)?
    /// Main thread: a new pairing was saved.
    var onPaired: ((KnownHost) -> Void)?
    /// Main thread: the host's cursor changed shape.
    var onCursor: ((CursorMsg) -> Void)?
    let path = Locked("")
    let queue = DispatchQueue(label: "bb1.client", qos: .userInteractive)
    let decoder = VideoDecoder()
    let audio = AudioPlayer()
    /// Whether the user wants sound (persisted by the window).
    private(set) var soundWanted = true
    var hostHasSound: Bool { (control?.peerVersion ?? 1) >= 3 }
    let counters = Locked(ClientCounters())
    private let clientName = Host.current().localizedName ?? "Mac"

    // Main-thread callbacks
    var onStatus: ((String, Bool) -> Void)?
    var onConfig: ((StreamConfig) -> Void)?
    var onNotice: ((String) -> Void)?
    var onStats: ((ClientStats) -> Void)?
    var onEnded: ((String) -> Void)?
    /// Client queue. Ask the display to flush and start over.
    var onDisplayStall: (() -> Void)?

    private var gen = 0
    private var stopped = false
    private var retryDelay = 0.25
    private var control: ControlChannel?
    private var udp: NWConnection?
    private var keys: SessionKeys?
    private var config: StreamConfig?
    private var timers: [DispatchSourceTimer] = []
    private let reassembler = Reassembler()

    private var waitingForKeyframe = true
    private var lastFrameId: UInt32 = 0
    private var currentEpoch: UInt16 = 0
    private var lastKeyframeRequestUs: UInt64 = 0

    private var connectStartedUs: UInt64 = 0
    private var pingSeq: UInt32 = 0
    private var lastPongUs: UInt64 = 0
    private var lastPong: PongMsg?
    private var rttUs: Double = 0
    private var offsetSamples: [(rtt: UInt64, offset: Int64)] = []
    private let clockOffsetUs = Locked<Int64?>(nil)
    private let latencySamples = Locked<[Double]>([])

    private struct Sample { var us: UInt64; var sent: UInt64; var received: UInt64; var accepted: UInt64; var decoded: UInt64; var displayed: UInt64 }
    private var history: [Sample] = []
    private var udpResetUs: UInt64 = 0
    private var lastPathCheckUs: UInt64 = 0
    /// Stage checks wait until this time: a fresh stream or decoder needs a moment to get going.
    private var checksResumeUs: UInt64 = 0
    private var decoderResetUs: UInt64 = 0
    private var displayResetUs: UInt64 = 0
    private var lastStats = (us: UInt64(0), displayed: UInt64(0), bytes: UInt64(0))
    private var statsBaseline = ClientCounters()
    private let resources = ResourceUsage()

    // Frames that arrived after a gap, waiting for the missing one to be resent.
    private var pending: [UInt32: Reassembler.Frame] = [:]
    private var gapSinceUs: UInt64 = 0
    private var supportsV2: Bool { (control?.peerVersion ?? 1) >= 2 }
    private let clipboard = ClipboardSync()
    private let clipboardEnabled = ProcessInfo.processInfo.environment["BB1_NO_CLIPBOARD"] == nil
    private let displayTiming = Locked((lastUs: UInt64(0), maxGapUs: UInt64(0)))
    private var sentHistory: [(us: UInt64, sent: UInt64)] = []

    init(target: ClientTarget) {
        self.target = target
        decoder.onError = { [weak self] reason in
            guard let self else { return }
            self.queue.async {
                self.waitingForKeyframe = true
                self.requestKeyframe(reason)
            }
        }
    }

    // MARK: Public (any thread)

    func start() {
        queue.async {
            self.stopped = false
            self.connect()
        }
    }

    func stop() {
        queue.async {
            guard !self.stopped else { return }
            self.stopped = true
            self.control?.send(.bye("client closed"))
            self.queue.asyncAfter(deadline: .now() + 0.15) { self.teardown() }
        }
    }

    func send(_ message: ControlMessage) {
        queue.async { if self.keys != nil { self.control?.send(message) } }
    }

    /// Mute is done on the host side too, so no sound is sent while muted.
    func setSound(_ on: Bool) {
        queue.async {
            self.soundWanted = on
            self.audio.muted = !on
            guard self.keys != nil, self.hostHasSound else { return }
            if on { self.audio.start() }
            self.control?.send(.setAudio(on))
        }
    }

    func reconnectNow() {
        queue.async { self.scheduleReconnect("reconnect requested", delay: 0) }
    }

    /// Called by whoever shows the frame (decode queue).
    func markDisplayed(captureUs: UInt64) {
        counters.with { $0.displayed += 1 }
        let now = Clock.nowUs()
        let gap = displayTiming.with { t -> UInt64 in
            let gap = t.lastUs > 0 ? now - t.lastUs : 0
            t.lastUs = now
            t.maxGapUs = max(t.maxGapUs, gap)
            return gap
        }
        if gap > 300_000 { queue.async { self.reportGap(gap, endedUs: now) } }
        guard let offset = clockOffsetUs.get else { return }
        let latency = Double(Int64(Clock.nowUs()) + offset - Int64(captureUs)) / 1000
        if latency > 0 && latency < 5000 { latencySamples.with { $0.append(latency) } }
    }

    // MARK: Connecting

    private func connect() {
        gen += 1
        let g = gen
        status("Connecting to \(MacNames.display(target.label))…", connected: false)
        connectStartedUs = Clock.nowUs()
        lastPongUs = connectStartedUs
        let conn = NWConnection(to: target.endpoint, using: ControlChannel.tcpParameters())
        let label = target.label
        let channel = ControlChannel(connection: conn, role: .client(
            clientName: clientName, deviceId: DeviceIdentity.id, auth: target.auth,
            askCode: { [weak self] reply in
                DispatchQueue.main.async {
                    guard let ask = self?.askCode else { reply(nil); return }
                    ask(label, reply)
                }
            }), queue: queue)
        control = channel
        channel.onAuthenticated = { [weak self] keys in
            guard let self, g == self.gen else { return }
            self.keys = keys
            self.lastPongUs = Clock.nowUs()
            if let build = self.control?.peerBuild {
                let host = self.target.label
                DispatchQueue.main.async { HostUpdates.shared.note(build: build, for: host) }
            }
        }
        channel.onMessage = { [weak self] message in
            guard let self, g == self.gen else { return }
            self.handle(message)
        }
        channel.onClose = { [weak self] reason in
            guard let self, g == self.gen else { return }
            switch reason {
            case .rejected(let why):
                if case .device = self.target.auth, why.contains("doesn't remember") {
                    // The host forgot us: drop the stale pairing so the next try pairs again.
                    KnownHost.store.save(KnownHost.store.load().filter { $0.name != self.target.label })
                }
                self.end("Couldn't sign in: \(why.hasSuffix(".") ? why : why + ".")")
            case .closed(let why):
                if case .code = self.target.auth, self.keys == nil {
                    self.end("Pairing didn't finish (\(why)).")
                } else {
                    self.scheduleReconnect(why)
                }
            }
        }
        channel.start()
        startTimers(g)
    }

    private func startTimers(_ g: Int) {
        func every(_ seconds: Double, _ body: @escaping () -> Void) {
            let t = DispatchSource.makeTimerSource(queue: queue)
            t.schedule(deadline: .now() + seconds, repeating: seconds, leeway: .milliseconds(5))
            t.setEventHandler { [weak self] in
                guard let self, g == self.gen else { return }
                body()
            }
            t.resume()
            timers.append(t)
        }
        every(0.25) { [weak self] in self?.sendPing() }
        every(0.25) { [weak self] in self?.watchdog() }
        every(0.5) { [weak self] in self?.sendHello() }
        every(1.0) { [weak self] in self?.publishStats() }
    }

    private func teardown() {
        gen += 1
        resendTimer?.cancel()
        resendTimer = nil
        timers.forEach { $0.cancel() }
        timers.removeAll()
        control?.close(.closed("teardown"))
        control = nil
        udp?.cancel()
        udp = nil
        keys = nil
        config = nil
        reassembler.reset()
        waitingForKeyframe = true
        lastFrameId = 0
        pending.removeAll()
        gapSinceUs = 0
        sentHistory.removeAll()
        Log.sink = nil
        audio.stop()
        clipboard.stop()
        clipboard.onLocalCopy = nil
        history.removeAll()
        offsetSamples.removeAll()
        clockOffsetUs.with { $0 = nil }
        decoder.reset()
    }

    private func scheduleReconnect(_ reason: String, delay: Double? = nil) {
        guard !stopped else { return }
        Log.info("client: reconnecting: \(reason)")
        counters.with { $0.reconnects += 1 }
        teardown()
        status("Reconnecting… (\(reason))", connected: false)
        let wait = delay ?? retryDelay
        retryDelay = min(retryDelay * 2, 3)
        queue.asyncAfter(deadline: .now() + wait) { [weak self] in
            guard let self, !self.stopped, self.control == nil else { return }
            self.connect()
        }
    }

    private func end(_ reason: String) {
        Log.info("client: \(reason)")
        stopped = true
        teardown()
        DispatchQueue.main.async { self.onEnded?(reason) }
    }

    // MARK: Messages

    private func handle(_ message: ControlMessage) {
        switch message {
        case .welcome(let hostName):
            retryDelay = 0.25
            Log.info("client: connected to \(hostName) (protocol v\(control?.peerVersion ?? 1))")
            status("Connected to \(hostName)", connected: true)
            if supportsV2 {
                // Send this side's log lines to the host too, so everything is in one place.
                let g = gen
                Log.sink = { [weak self] line in
                    guard let self else { return }
                    self.queue.async { if g == self.gen { self.control?.send(.clientLog(line)) } }
                }
                if clipboardEnabled {
                    clipboard.onLocalCopy = { [weak self] message in self?.send(.clipboard(message)) }
                    clipboard.start()
                }
                if hostHasSound && soundWanted && ProcessInfo.processInfo.environment["BB1_NO_AUDIO"] == nil {
                    audio.start()
                    control?.send(.setAudio(true))
                }
            } else {
                Log.info("client: this host is an older version: no packet resend or clipboard")
            }
        case .streamConfig(let c):
            let old = config
            config = c
            Log.info("client: stream \(c.width)x\(c.height) \(c.codec.name)\(c.lowLatencyEncoder ? " low-latency" : "") \(c.bitrateMbps) Mbps, epoch \(c.epoch)")
            if old?.sessionId != c.sessionId || old?.udpPort != c.udpPort || udp == nil { openVideoPath(c) }
            if old?.epoch != c.epoch {
                currentEpoch = c.epoch
                waitingForKeyframe = true
                checksResumeUs = Clock.nowUs() + 3_000_000
            }
            DispatchQueue.main.async { self.onConfig?(c) }
        case .pong(let p):
            handlePong(p)
        case .notice(let text):
            Log.info("client: host says: \(text)")
            DispatchQueue.main.async { self.onNotice?(text) }
        case .bye(let reason):
            end("The host ended the session: \(reason)")
        case .clipboard(let c):
            if clipboardEnabled { clipboard.apply(c) }
        case .cursor(let c):
            DispatchQueue.main.async { self.onCursor?(c) }
        case .paired(let p):
            // Remember this host; from now on we sign in with this secret (no code, no Tailscale needed).
            let known = KnownHost(hostId: p.hostId, name: p.hostName, secret: p.secret,
                                  tailscaleIP: target.tailscaleIP, address: target.label, pairedAt: Date())
            var hosts = KnownHost.store.load().filter { $0.hostId != p.hostId }
            hosts.append(known)
            KnownHost.store.save(hosts)
            target.auth = .device(secret: p.secret)
            Log.info("client: paired with \(p.hostName); this Mac will sign in automatically from now on")
            DispatchQueue.main.async { self.onPaired?(known) }
        default:
            break
        }
    }

    private func requestKeyframe(_ reason: String) {
        let now = Clock.nowUs()
        guard now - lastKeyframeRequestUs > 200_000 else { return }
        lastKeyframeRequestUs = now
        counters.with { $0.keyframeRequests += 1 }
        control?.send(.requestKeyframe(reason: reason))
    }

    // MARK: Video path

    private func openVideoPath(_ c: StreamConfig) {
        udp?.cancel()
        reassembler.reset()
        var host = control?.remoteHost
        if host == nil, case .hostPort(let h, _) = target.endpoint { host = h }
        guard let host, let port = NWEndpoint.Port(rawValue: c.udpPort) else {
            Log.info("client: can't work out the host's address for video")
            return
        }
        let params = NWParameters.udp
        params.serviceClass = .interactiveVideo
        let conn = NWConnection(host: host, port: port, using: params)
        udp = conn
        checksResumeUs = Clock.nowUs() + 3_000_000
        let g = gen
        conn.stateUpdateHandler = { [weak self] state in
            guard let self, g == self.gen else { return }
            if case .failed(let error) = state { Log.info("client: video path failed: \(error)") }
        }
        conn.start(queue: queue)
        receive(conn, g)
        // A few hellos in quick succession: the first one can arrive before the host is ready for it.
        for delay in [0.0, 0.05, 0.15, 0.3] {
            queue.asyncAfter(deadline: .now() + delay) { [weak self] in
                guard let self, g == self.gen else { return }
                self.sendHello()
            }
        }
        Log.info("client: video path to \(host):\(c.udpPort)")
    }

    private func sendHello() {
        guard let udp, let keys, let config else { return }
        udp.send(content: UDPHello.make(sessionId: config.sessionId, key: keys.video), completion: .idempotent)
    }

    private func receive(_ conn: NWConnection, _ g: Int) {
        conn.receiveMessage { [weak self] data, _, _, error in
            guard let self, g == self.gen, conn === self.udp else { return }
            if let data { self.handleDatagram(data) }
            if error == nil { self.receive(conn, g) }
        }
    }

    private func handleDatagram(_ data: Data) {
        counters.with { $0.bytes += UInt64(data.count) }
        if data.first == AudioPacket.type {
            if let keys, let (seq, _, pcm) = AudioPacket.open(data, key: keys.video) { audio.receive(seq: seq, pcm: pcm) }
            return
        }
        let frame = reassembler.add(data, nowUs: Clock.nowUs())
        if let frame {
            counters.with { $0.received += 1 }
            process(frame)
        }
        if reassembler.hasPending || gapSinceUs > 0 { startResendTimer() }
    }

    /// Puts frames back in order. After a gap, later frames wait (briefly) for the missing one to be resent.
    private func process(_ frame: Reassembler.Frame) {
        guard keys != nil else { return }
        if frame.wasRetransmitted { counters.with { $0.recoveredByResend += 1 } }
        if lastFrameId != 0, frame.frameId <= lastFrameId { return }
        if frame.isKeyframe {
            if frame.epoch &- currentEpoch > 0x8000 { return } // keyframe from an older encoder
            // A keyframe doesn't depend on anything before it.
            for id in pending.keys where id < frame.frameId { pending[id] = nil }
            deliver(frame)
            drainPending()
            return
        }
        if waitingForKeyframe {
            requestKeyframe("waiting for keyframe")
            return
        }
        if frame.frameId == lastFrameId &+ 1 && frame.epoch == currentEpoch {
            deliver(frame)
            drainPending()
        } else if frame.epoch != currentEpoch {
            giveUpOnGap("frame from a restarted encoder")
        } else {
            pending[frame.frameId] = frame
            if gapSinceUs == 0 { gapSinceUs = Clock.nowUs() }
            if !supportsV2 { giveUpOnGap("lost frame") } // an old host can't resend
        }
    }

    private func drainPending() {
        while !waitingForKeyframe, let next = pending.removeValue(forKey: lastFrameId &+ 1) {
            guard next.epoch == currentEpoch || next.isKeyframe else {
                giveUpOnGap("frame from a restarted encoder")
                return
            }
            deliver(next)
        }
        gapSinceUs = pending.isEmpty ? 0 : Clock.nowUs()
    }

    /// The missing frame isn't coming: start over from a keyframe.
    private func giveUpOnGap(_ reason: String) {
        let firstPending = pending.keys.min() ?? (lastFrameId &+ 2)
        let missing = firstPending > lastFrameId ? UInt64(firstPending - lastFrameId - 1) : 1
        counters.with { $0.lostFrames += max(missing, 1) }
        pending.removeAll()
        gapSinceUs = 0
        waitingForKeyframe = true
        requestKeyframe(reason)
    }

    private func deliver(_ frame: Reassembler.Frame) {
        guard let keys,
              let plain = try? FrameCrypto.open(frame.sealed, key: keys.video, frameId: frame.frameId, aad: frame.aad),
              let payload = FramePayload.decode(plain) else {
            giveUpOnGap("frame failed to decrypt")
            return
        }
        lastFrameId = frame.frameId
        currentEpoch = frame.epoch
        waitingForKeyframe = false
        reassembler.floor = frame.frameId
        counters.with { $0.accepted += 1 }
        decoder.submit(payload, captureUs: frame.captureUs)
    }

    private var resendTimer: DispatchSourceTimer?

    /// The 3 ms resend check runs only while something is missing or half-received, then stops
    /// itself, so an idle stream doesn't wake the CPU ~330 times a second.
    private func startResendTimer() {
        guard resendTimer == nil, supportsV2 else { return }
        let g = gen
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 0.003, repeating: 0.003, leeway: .microseconds(500))
        t.setEventHandler { [weak self] in
            guard let self, g == self.gen else { return }
            self.askForResends()
            if !self.reassembler.hasPending && self.gapSinceUs == 0 {
                self.resendTimer?.cancel()
                self.resendTimer = nil
            }
        }
        t.resume()
        resendTimer = t
    }

    /// Ask the host to resend missing packets, and give up on gaps that took too long.
    private func askForResends() {
        guard keys != nil, supportsV2 else { return }
        let now = Clock.nowUs()
        let retry = UInt64(max(rttUs * 1.5, 8_000))
        for r in reassembler.requests(nowUs: now, quietUs: 3_000, retryUs: retry, maxTries: 4) where r.frameId > lastFrameId {
            counters.with { $0.nacksSent += 1 }
            control?.send(.nack(NackMsg(frameId: r.frameId, indices: r.indices)))
        }
        if gapSinceUs > 0, now - gapSinceUs > max(80_000, UInt64(rttUs * 4)) {
            giveUpOnGap("lost frame (resend didn't arrive in time)")
        }
    }

    // MARK: Heartbeat and health

    private func sendPing() {
        guard keys != nil else { return }
        pingSeq &+= 1
        let c = counters.get
        control?.send(.ping(PingMsg(seq: pingSeq, clientUs: Clock.nowUs(), received: c.received,
                                    decoded: decoder.decoded.get, displayed: c.displayed)))
    }

    private func handlePong(_ p: PongMsg) {
        let now = Clock.nowUs()
        lastPongUs = now
        lastPong = p
        let rtt = now > p.clientUs ? now - p.clientUs : 0
        rttUs = rttUs == 0 ? Double(rtt) : rttUs * 0.8 + Double(rtt) * 0.2
        // Clock offset from the lowest-RTT sample among recent ones (least queueing noise).
        offsetSamples.append((rtt, Int64(p.hostUs) - Int64(p.clientUs + rtt / 2)))
        if offsetSamples.count > 40 { offsetSamples.removeFirst() }
        if let best = offsetSamples.min(by: { $0.rtt < $1.rtt }) { clockOffsetUs.with { $0 = best.offset } }

        sentHistory.append((now, p.sent))
        sentHistory.removeAll { now - $0.us > 30_000_000 }
        let c = counters.get
        history.append(Sample(us: now, sent: p.sent, received: c.received, accepted: c.accepted,
                              decoded: decoder.decoded.get, displayed: c.displayed))
        history.removeAll { now - $0.us > 3_000_000 }
    }

    private func watchdog() {
        let now = Clock.nowUs()
        guard keys != nil else {
            if control?.awaitingCode == true { connectStartedUs = now; return } // the user is typing the code
            if now - connectStartedUs > 6_000_000 { scheduleReconnect("handshake timed out") }
            return
        }
        if now - lastPathCheckUs > 15_000_000 { lastPathCheckUs = now; checkPath() }
        if now - lastPongUs > 2_000_000 {
            scheduleReconnect("no heartbeat from the host for 2 s")
            return
        }
        // Give a resend time to arrive from far away too (same limit as giving up on a gap).
        reassembler.expire(nowUs: now, maxAgeUs: max(250_000, UInt64(rttUs * 4) + 100_000))
        if waitingForKeyframe, now - lastKeyframeRequestUs > 300_000 { requestKeyframe("still waiting for keyframe") }

        // Compare counters now vs ~1 s ago to find a stalled stage.
        guard now > checksResumeUs, let latest = history.last,
              let past = history.last(where: { latest.us - $0.us >= 900_000 }),
              latest.us - past.us < 2_000_000 else { return }
        let sent = latest.sent &- past.sent
        let received = latest.received &- past.received
        let accepted = latest.accepted &- past.accepted
        let decoded = latest.decoded &- past.decoded
        let displayed = latest.displayed &- past.displayed

        if sent >= 3 && received == 0 {
            if udpResetUs == 0 || now - udpResetUs > 6_000_000 {
                Log.info("client: host sent \(sent) frames but none arrived; reopening the video path")
                counters.with { $0.videoPathResets += 1 }
                udpResetUs = now
                if let config { openVideoPath(config) }
                waitingForKeyframe = true
                requestKeyframe("video path reopened")
                history.removeAll()
            } else if now - udpResetUs > 2_500_000 {
                udpResetUs = 0
                scheduleReconnect("video still not arriving after reopening the video path")
            }
        } else if accepted >= 3 && decoded == 0 && now - decoderResetUs > 2_000_000 {
            Log.info("client: \(accepted) frames went to the decoder but none came out; resetting it")
            counters.with { $0.decoderResets += 1 }
            decoderResetUs = now
            checksResumeUs = now + 3_000_000
            decoder.reset()
            waitingForKeyframe = true
            requestKeyframe("decoder stalled")
        } else if decoded >= 3 && displayed == 0 && now - displayResetUs > 2_000_000 {
            Log.info("client: \(decoded) frames decoded but none shown; resetting the display")
            counters.with { $0.displayResets += 1 }
            displayResetUs = now
            onDisplayStall?()
        }
    }

    /// A pause in the picture. If the host kept sending during it, that's a stall worth logging;
    /// if it didn't, the screen was just idle.
    private func reportGap(_ gapUs: UInt64, endedUs: UInt64) {
        let startUs = endedUs - gapUs
        guard let before = sentHistory.last(where: { $0.us <= startUs + 250_000 }),
              let after = sentHistory.last else { return }
        let sentDuring = after.sent &- before.sent
        guard sentDuring >= 3 else { return }
        Log.info(String(format: "client: STALL no new picture for %.0f ms while the host sent %llu frames (RTT %.1f ms)",
                        Double(gapUs) / 1000, sentDuring, rttUs / 1000))
    }

    /// Home network vs Tailscale (and whether Tailscale found a direct route or is relaying).
    private func checkPath() {
        guard let ip = target.tailscaleIP else {
            if path.get.isEmpty { path.with { $0 = "home network" }; Log.info("client: connected over the home network") }
            return
        }
        DispatchQueue.global().async {
            let description = "Tailscale · " + (Tailscale.pathDescription(to: ip, in: Tailscale.status()) ?? "connected")
            if self.path.get != description { Log.info("client: connected via \(description)") }
            self.path.with { $0 = description }
        }
    }

    private func publishStats() {
        let now = Clock.nowUs()
        let c = counters.get
        var s = ClientStats()
        s.config = config
        s.counters = c
        s.connected = keys != nil
        s.rttMs = rttUs / 1000
        s.hostEncodeMs = Double(lastPong?.encodeAvgUs ?? 0) / 1000
        s.hostRecoveries = lastPong?.hostRecoveries ?? 0
        s.decodeMs = decoder.decodeAvgUs.get / 1000
        s.path = path.get
        if lastStats.us > 0 {
            let seconds = Double(now - lastStats.us) / 1_000_000
            s.fps = Double(c.displayed &- lastStats.displayed) / seconds
            s.mbps = Double(c.bytes &- lastStats.bytes) * 8 / 1_000_000 / seconds
        }
        lastStats = (now, c.displayed, c.bytes)
        let samples = latencySamples.with { v -> [Double] in defer { v.removeAll() }; return v }
        if !samples.isEmpty {
            s.latencyMs = samples.reduce(0, +) / Double(samples.count)
            s.latencyMaxMs = samples.max()
        }
        let soundStats = audio.buffer.takeStats()
        let usage = resources.sample()
        let maxGap = displayTiming.with { t -> UInt64 in defer { t.maxGapUs = 0 }; return t.maxGapUs }
        if keys != nil {
            let b = statsBaseline
            Log.info(String(format: "stats: shown %.0f fps · %.1f Mbps · RTT %.1f ms · capture→shown avg %@ max %@ ms · longest pause %llu ms · lost %llu · resend asks %llu, recovered %llu · keyframe req %llu · decode %.1f ms · sound %llu pkts, %llu gaps, %llu dry, buffer %d ms · bb1 cpu %.0f%% mem %.0f MB",
                            s.fps, s.mbps, s.rttMs,
                            s.latencyMs.map { String(format: "%.0f", $0) } ?? "-",
                            s.latencyMaxMs.map { String(format: "%.0f", $0) } ?? "-",
                            maxGap / 1000, c.lostFrames &- b.lostFrames, c.nacksSent &- b.nacksSent,
                            c.recoveredByResend &- b.recoveredByResend, c.keyframeRequests &- b.keyframeRequests, s.decodeMs,
                            audio.packets.with { v -> UInt64 in defer { v = 0 }; return v },
                            soundStats.concealed, soundStats.underruns, soundStats.bufferMs,
                            usage.cpuPercent, usage.memoryMB))
        }
        statsBaseline = c
        DispatchQueue.main.async { self.onStats?(s) }
    }

    private func status(_ text: String, connected: Bool) {
        DispatchQueue.main.async { self.onStatus?(text, connected) }
    }
}
