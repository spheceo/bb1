import ApplicationServices
import AppKit
import BB1Core
import CoreGraphics
import Foundation
import Network

struct HostOptions {
    var headless = false
    var pipeline = PipelineSettings()
    var controlPort = BB1.controlPort
    var videoPort = BB1.videoPort
    var paceMbps = 300
    var invertScroll = false
    var dropPercent = 0.0

    static let usage = """
    usage: bb1-host [options]
      --headless          no menu bar icon; log to the terminal only
      --synthetic         stream a test pattern instead of the screen
      --codec hevc|h264   preferred codec (default hevc)
      --bitrate N         Mbps (default: 30 at 1080p, scaled by resolution)
      --fps N             frames per second (default 60)
      --port N            control port (default 47800; video uses N+1)
      --pace N            max burst rate for large frames in Mbps (default 300)
      --invert-scroll     flip scroll direction
      --drop-percent N    testing: randomly drop N% of video packets
      --show-key          print the pairing key and exit
      --new-key           make a new pairing key (old clients must re-pair) and exit
    """

    static func parse(_ args: [String]) -> HostOptions {
        var o = HostOptions()
        var i = 1
        func next() -> String {
            i += 1
            guard i < args.count else { print(usage); exit(2) }
            return args[i]
        }
        while i < args.count {
            switch args[i] {
            case "--headless": o.headless = true
            case "--synthetic": o.pipeline.synthetic = true
            case "--codec": o.pipeline.codec = next().lowercased() == "h264" ? .h264 : .hevc
            case "--bitrate": o.pipeline.bitrateMbps = Int(next())
            case "--fps": o.pipeline.fps = Int(next()) ?? 60
            case "--port":
                o.controlPort = UInt16(next()) ?? BB1.controlPort
                o.videoPort = o.controlPort + 1
            case "--pace": o.paceMbps = Int(next()) ?? 500
            case "--invert-scroll": o.invertScroll = true
            case "--drop-percent": o.dropPercent = Double(next()) ?? 0
            case "--show-key":
                print(PairingKey.loadOrCreateHostKey().display); exit(0)
            case "--new-key":
                try? FileManager.default.removeItem(at: BB1.supportDirectory().appendingPathComponent("host-key"))
                print(PairingKey.loadOrCreateHostKey().display); exit(0)
            case "-h", "--help": print(usage); exit(0)
            default:
                // Ignore arguments macOS adds when launching an app bundle.
                if args[i].hasPrefix("-psn") || args[i].hasPrefix("-NS") { i += 1; continue }
                print("unknown option \(args[i])\n\(usage)"); exit(2)
            }
            i += 1
        }
        return o
    }
}

/// One connected client.
final class HostSession {
    let id: UInt32
    let control: ControlChannel
    let keys: SessionKeys
    let name: String
    let connectedAt = Date()
    var lastPingUs = Clock.nowUs()
    var config: StreamConfig?
    /// Told the client this Mac is low on disk space.
    var warnedLowDisk = false

    private let sendQueue = DispatchQueue(label: "bb1.send", qos: .userInteractive)
    private let udp = Locked<NWConnection?>(nil)
    private var nextFrameId: UInt32 = 1
    private let paceBytesPerSecond: Double
    /// Share of full quality the network carries right now (host queue; see HostController.adaptRate).
    var rate = 1.0
    var cleanSeconds = 0
    /// Burst limit for big frames (bytes/s). Full speed normally. On a constrained path (a relay, loss) a few
    /// times the bitrate, in small steps: a Tailscale relay drops whatever doesn't fit its short queue.
    private let pace: Locked<Double>
    func setRate(_ rate: Double, bitrateMbps: Int) {
        self.rate = rate
        let constrained = min(paceBytesPerSecond, Double(max(12, bitrateMbps * 3)) * 125_000)
        pace.with { $0 = rate >= 1 ? paceBytesPerSecond : constrained }
    }
    var dropPercent = 0.0
    let sent = Locked<UInt64>(0)
    let sentBytes = Locked<UInt64>(0)
    let nacks = Locked<UInt64>(0)
    let resentPackets = Locked<UInt64>(0)
    /// Recently sent packets, so lost ones can be resent (sendQueue only).
    private var recent: [UInt32: (us: UInt64, packets: [Data])] = [:]
    var supportsV2: Bool { control.peerVersion >= 2 }
    /// The client asked for sound (version 3+ clients only).
    let audioEnabled = Locked(false)
    private var audioSeq: UInt32 = 0
    private var recentAudio: [Data] = []
    let audioPackets = Locked<UInt64>(0)

    /// Called on the capture's audio queue. Digital silence isn't sent at all (the client plays
    /// silence when nothing arrives), so an idle Mac doesn't send 400 packets a second for nothing.
    func sendAudio(_ pcm: Data, captureUs: UInt64) {
        guard audioEnabled.get, let connection = udp.get else { return }
        let silent = pcm.withUnsafeBytes { raw in
            raw.bindMemory(to: Int16.self).allSatisfy { abs(Int32($0)) <= 2 }
        }
        // Count silent chunks too (without sending them), so the client can tell a pause in the
        // sound (sequence jumps ahead) from a packet that's just late.
        audioSeq &+= 1
        if silent {
            recentAudio.removeAll()
            return
        }
        guard let packet = AudioPacket.make(seq: audioSeq, captureUs: captureUs, pcm: pcm, key: keys.video) else { return }
        connection.send(content: packet, completion: .idempotent)
        // Send each packet a second time, two packets (10 ms) later: one lost packet no longer means a gap.
        recentAudio.append(packet)
        if recentAudio.count > 2 { connection.send(content: recentAudio.removeFirst(), completion: .idempotent) }
        audioPackets.with { $0 += 1 }
    }

    init(id: UInt32, control: ControlChannel, keys: SessionKeys, name: String, paceMbps: Int) {
        self.id = id
        self.control = control
        self.keys = keys
        self.name = name
        self.paceBytesPerSecond = Double(paceMbps) * 125_000
        pace = Locked(Double(paceMbps) * 125_000)
    }

    var hasVideoPath: Bool { udp.get != nil }

    /// Returns true if this is a new UDP path.
    func attach(udp connection: NWConnection) -> Bool {
        udp.with { current in
            if current === connection { return false }
            current?.cancel()
            current = connection
            return true
        }
    }

    func send(_ frame: EncodedFrame) {
        sendQueue.async { [self] in
            guard let connection = udp.get else { return }
            let frameId = nextFrameId
            nextFrameId &+= 1
            var flags: UInt8 = 0
            if frame.isKeyframe { flags |= VideoPacketHeader.keyframeFlag }
            if frame.isRefresh { flags |= VideoPacketHeader.refreshFlag }
            let aad = VideoPacketHeader.aad(flags: flags, epoch: frame.epoch, frameId: frameId, captureUs: frame.captureUs)
            guard let sealed = try? FrameCrypto.seal(frame.payload, key: keys.video, frameId: frameId, aad: aad) else { return }
            let packets = Packetizer.packets(sealed: sealed, flags: flags, epoch: frame.epoch, frameId: frameId, captureUs: frame.captureUs)

            let now = Clock.nowUs()
            recent[frameId] = (now, packets)
            // Long enough for a resend to a Mac on the other side of the world (RTT 300+ ms, a few tries).
            recent = recent.filter { now - $0.value.us < 2_000_000 }

            // Bigger frames (keyframes, fast motion) are paced so a Wi-Fi hop or a relay doesn't drop the burst.
            let paceBytesPerSecond = self.pace.get
            let constrained = paceBytesPerSecond < self.paceBytesPerSecond
            let pace = packets.count > (constrained ? 4 : 16)
            let step = constrained ? 8 : 32
            let start = Clock.nowUs()
            var bytes = 0
            for (i, packet) in packets.enumerated() {
                if dropPercent > 0, Double.random(in: 0..<100) < dropPercent { continue }
                connection.send(content: packet, completion: .idempotent)
                bytes += packet.count
                if pace, i % step == step - 1 {
                    let targetUs = UInt64(Double(bytes) / paceBytesPerSecond * 1_000_000)
                    let elapsed = Clock.nowUs() - start
                    if targetUs > elapsed { usleep(useconds_t(targetUs - elapsed)) }
                }
            }
            sent.with { $0 += 1 }
            sentBytes.with { $0 += UInt64(bytes) }
        }
    }

    /// Resend packets the client says it didn't get.
    func resend(_ n: NackMsg) {
        sendQueue.async { [self] in
            nacks.with { $0 += 1 }
            guard let connection = udp.get, let entry = recent[n.frameId] else { return }
            let indices = n.indices.map { $0.map(Int.init) } ?? Array(entry.packets.indices)
            var count: UInt64 = 0
            for i in indices where i < entry.packets.count {
                connection.send(content: entry.packets[i], completion: .idempotent)
                count += 1
            }
            resentPackets.with { $0 += count }
        }
    }

    func close() {
        udp.with { $0?.cancel(); $0 = nil }
        control.close(.closed("session ended"))
    }
}

/// Listens for clients, owns the pipeline, routes input.
final class HostController {
    let options: HostOptions
    /// The old shared pairing key, still accepted from version 1–3 clients (and upgraded to a pairing).
    let key = PairingKey.loadOrCreateHostKey()
    let hostId = DeviceIdentity.id

    /// Main thread: a Mac asked to pair; show (device name, code). Then: finished(paired?).
    var onPairingCode: ((String, String) -> Void)?
    var onPairingFinished: ((Bool) -> Void)?
    private var pairing: (channel: ControlChannel, code: String, deviceName: String)?
    private var ownTailscaleLogin: (login: String?, atUs: UInt64) = (nil, 0)
    /// `BB1_HOST_NAME` (tests only) lets a second test host on this Mac show up under another name.
    let hostName = ProcessInfo.processInfo.environment["BB1_HOST_NAME"] ?? Host.current().localizedName ?? "Mac"
    let queue = DispatchQueue(label: "bb1.host", qos: .userInteractive)
    let pipeline: StreamPipeline
    private let injector = InputInjector()

    private var tcpListener: NWListener?
    private var udpListener: NWListener?
    private var pendingUDP: [NWConnection] = []
    private var handshaking: [ControlChannel] = []
    /// Connections that only check this Mac's build or ask it to update (they don't stream).
    private var updateChannels: [ControlChannel] = []
    private(set) var session: HostSession?
    private var nextSessionId = UInt32.random(in: 1...UInt32.max / 2)
    private var lastPointerUs: UInt64 = 0
    /// Times of recent clicks and key presses (for the capture check).
    private var recentInputUs: [UInt64] = []
    private func noteInput() {
        recentInputUs.append(Clock.nowUs())
        if recentInputUs.count > 20 { recentInputUs.removeFirst(recentInputUs.count - 20) }
    }
    private var watchdog: DispatchSourceTimer?
    private var activity: NSObjectProtocol?
    private var displayDebounce: DispatchWorkItem?
    private var listenerError: String?
    private let clipboard = ClipboardSync()
    private let audio = AudioChunker()
    private let cursorWatcher = CursorWatcher()
    private var input = (moves: 0, clicks: 0, keys: 0, scrolls: 0)
    private var statsBaseline = (sent: UInt64(0), bytes: UInt64(0), captured: UInt64(0), encoded: UInt64(0),
                                 keyframes: UInt64(0), nacks: UInt64(0), resent: UInt64(0))
    private var lastWatchdogUs: UInt64 = 0
    private var watchdogTicks = 0
    private var memoryPressure: DispatchSourceMemoryPressure?
    private let resources = ResourceUsage()

    /// Main thread. Called when anything shown in the menu changes.
    var onChange: (() -> Void)?

    init(options: HostOptions) {
        self.options = options
        self.pipeline = StreamPipeline(settings: options.pipeline)
        injector.invertScroll = options.invertScroll
    }

    // MARK: Status for the menu

    struct Status {
        var clientName: String?
        var stream: StreamInfo?
        var listening: Bool
        var listenerError: String?
        var lastError: String?
        var recoveries: [String: Int]
    }

    func status() -> Status {
        queue.sync {
            let c = pipeline.counters.get
            return Status(clientName: session?.name, stream: pipeline.info, listening: tcpListener != nil,
                          listenerError: listenerError, lastError: c.lastError, recoveries: c.recoveries)
        }
    }

    static var hasScreenRecording: Bool { CGPreflightScreenCaptureAccess() }
    static var hasAccessibility: Bool { AXIsProcessTrusted() }

    // MARK: Start

    func start() {
        Log.info("host: bb1 host starting on \(hostName), pairing key \(key.display)")
        if !options.pipeline.synthetic && !HostController.hasScreenRecording && ProcessInfo.processInfo.environment["BB1_NO_PROMPT"] == nil {
            Log.info("host: Screen Recording permission missing, asking")
            CGRequestScreenCaptureAccess()
        }
        let mayPrompt = ProcessInfo.processInfo.environment["BB1_NO_PROMPT"] == nil
        if !HostController.hasAccessibility && mayPrompt {
            Log.info("host: Accessibility permission missing (needed for mouse/keyboard), asking")
            let prompt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
            _ = AXIsProcessTrustedWithOptions([prompt: true] as CFDictionary)
        }

        pipeline.onEncodedFrame = { [weak self] frame in self?.session?.send(frame) }
        audio.onChunk = { [weak self] pcm, captureUs in self?.session?.sendAudio(pcm, captureUs: captureUs) }
        pipeline.onAudio = { [weak self] sample in
            guard let self, self.session?.audioEnabled.get == true else { return }
            self.audio.add(sample)
        }
        pipeline.onCaptureStopped = { [weak self] _ in
            guard let self else { return }
            self.queue.async { if let s = self.session { self.warnIfLowOnDisk(s) } }
        }
        pipeline.onStreamInfo = { [weak self] info in
            guard let self else { return }
            self.queue.async { self.sendStreamConfig(info) }
        }

        queue.async {
            self.startListeners()
            self.startWatchdog()
            self.watchPermissions()
        }
        if options.pipeline.synthetic || HostController.hasScreenRecording { pipeline.prewarm() }
        watchDisplayChanges()
        watchSignals()
    }

    private var permissionTimer: DispatchSourceTimer?

    /// When a missing permission gets granted, exit with an error so launchd starts a fresh copy
    /// that can use it (screen capture only picks up a new grant after a restart).
    private func watchPermissions() {
        let needScreen = !options.pipeline.synthetic && !HostController.hasScreenRecording
        let needAX = !HostController.hasAccessibility
        guard needScreen || needAX else { return }
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 2, repeating: 2)
        timer.setEventHandler {
            let gotScreen = needScreen && HostController.hasScreenRecording
            let gotAX = needAX && HostController.hasAccessibility
            guard gotScreen || gotAX else { return }
            Log.info("host: permission granted (\(gotScreen ? "Screen Recording" : "Accessibility")), restarting to use it")
            HostController.restartApp()
        }
        timer.resume()
        permissionTimer = timer
    }

    private var signalSources: [DispatchSourceSignal] = []
    private var wakeObserver: NSObjectProtocol?

    /// Screen capture only sees a new permission after a restart. Under launchd (installed host),
    /// exiting with an error makes launchd start a fresh copy; otherwise relaunch the app ourselves.
    static func restartApp() -> Never {
        if ProcessInfo.processInfo.environment["XPC_SERVICE_NAME"] != "com.bb1.host" {
            let bundle = Bundle.main.bundlePath
            let task = Process()
            if bundle.hasSuffix(".app") {
                task.executableURL = URL(fileURLWithPath: "/bin/sh")
                task.arguments = ["-c", "sleep 1; /usr/bin/open -n \"\(bundle)\""]
            } else if let exe = Bundle.main.executableURL {
                task.executableURL = exe
            }
            try? task.run()
        }
        exit(3)
    }

    /// Stop sharing this Mac: drop the client, stop listening, stop all timers.
    func stop() {
        queue.sync {
            self.endSession("sharing turned off on this Mac")
            self.tcpListener?.cancel(); self.tcpListener = nil
            self.udpListener?.cancel(); self.udpListener = nil
            self.pendingUDP.forEach { $0.cancel() }
            self.pendingUDP.removeAll()
            self.handshaking.forEach { $0.close(.closed("sharing turned off")) }
            self.watchdog?.cancel(); self.watchdog = nil
            self.permissionTimer?.cancel(); self.permissionTimer = nil
            self.memoryPressure?.cancel(); self.memoryPressure = nil
            self.signalSources.forEach { $0.cancel() }
            self.signalSources.removeAll()
        }
        CGDisplayRemoveReconfigurationCallback(HostController.displayCallback, Unmanaged.passUnretained(self).toOpaque())
        if let wakeObserver { NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver) }
        wakeObserver = nil
        pipeline.shutdown()
        Log.info("host: stopped sharing this Mac")
    }

    private static let displayCallback: CGDisplayReconfigurationCallBack = { _, flags, userInfo in
        guard let userInfo, !flags.contains(.beginConfigurationFlag) else { return }
        Unmanaged<HostController>.fromOpaque(userInfo).takeUnretainedValue().displaysChanged()
    }

    /// Testing: `kill -USR1 <pid>` makes capture fail, `kill -USR2 <pid>` stalls the encoder.
    private func watchSignals() {
        for (sig, fault) in [(SIGUSR1, StreamPipeline.Fault.captureStops), (SIGUSR2, .encoderStalls)] {
            signal(sig, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: sig, queue: queue)
            source.setEventHandler { [weak self] in self?.pipeline.inject(fault) }
            source.resume()
            signalSources.append(source)
        }
    }

    private func startListeners() {
        do {
            let tcp = try NWListener(using: ControlChannel.tcpParameters(), on: NWEndpoint.Port(rawValue: options.controlPort)!)
            tcp.service = NWListener.Service(name: hostName, type: BB1.bonjourType)
            tcp.stateUpdateHandler = { [weak self] state in self?.listenerState("control", state) }
            tcp.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            tcp.start(queue: queue)
            tcpListener = tcp

            let udpParams = NWParameters.udp
            udpParams.serviceClass = .interactiveVideo
            let udp = try NWListener(using: udpParams, on: NWEndpoint.Port(rawValue: options.videoPort)!)
            udp.stateUpdateHandler = { [weak self] state in self?.listenerState("video", state) }
            udp.newConnectionHandler = { [weak self] conn in self?.acceptUDP(conn) }
            udp.start(queue: queue)
            udpListener = udp
        } catch {
            listenerError = "Couldn't listen on port \(options.controlPort): \(error.localizedDescription)"
            Log.info("host: \(listenerError!)")
            notifyChange()
        }
    }

    private func listenerState(_ name: String, _ state: NWListener.State) {
        switch state {
        case .ready:
            Log.info("host: \(name) listener ready on port \(name == "control" ? options.controlPort : options.videoPort)")
            listenerError = nil
        case .failed(let error):
            listenerError = "\(name) listener failed: \(error.localizedDescription)"
            Log.info("host: \(listenerError!) — restarting listeners in 2 s")
            tcpListener?.cancel(); udpListener?.cancel()
            tcpListener = nil; udpListener = nil
            queue.asyncAfter(deadline: .now() + 2) { self.startListeners() }
        default: break
        }
        notifyChange()
    }

    // MARK: Control connections

    private func accept(_ conn: NWConnection) {
        var channelRef: ControlChannel?
        let channel = ControlChannel(connection: conn, role: .host(hostName: hostName, hostId: hostId, legacyKey: key, authorize: { [weak self] request, reply in
            guard let self, let channel = channelRef else { reply(.reject("host is shutting down")); return }
            self.authorize(request, channel: channel, reply: reply)
        }), queue: queue)
        channelRef = channel
        // Hold on to it until the handshake finishes; after that the session owns it.
        handshaking.append(channel)
        channel.onAuthenticated = { [weak self, weak channel] keys in
            guard let self, let channel else { return }
            self.handshaking.removeAll { $0 === channel }
            if channel.purpose == "update" {
                self.startUpdateSession(channel)
            } else {
                self.startSession(channel, keys: keys)
            }
        }
        channel.onClose = { [weak self, weak channel] reason in
            guard let self, let channel else { return }
            self.handshaking.removeAll { $0 === channel }
            self.updateChannels.removeAll { $0 === channel }
            if case .rejected(let why) = reason { Log.info("host: rejected \(channel.peerName) (\(conn.endpoint)): \(why)") }
            if self.pairing?.channel === channel { self.finishPairing(false) }
            if self.session?.control === channel { self.endSession("client disconnected") }
            channelRef = nil
        }
        channel.start()
        // A connection that never finishes the handshake gets dropped (pairing gets 2 minutes to type the code).
        func timeout(after seconds: Double) {
            queue.asyncAfter(deadline: .now() + seconds) { [weak self, weak channel] in
                guard let self, let channel, self.handshaking.contains(where: { $0 === channel }) else { return }
                if channel.authMode == "code" && seconds < 100 { timeout(after: 120); return }
                channel.close(.closed("handshake timed out"))
            }
        }
        timeout(after: 10)
    }

    // MARK: Who gets in

    private func authorize(_ request: ControlChannel.AuthRequest, channel: ControlChannel,
                           reply: @escaping (ControlChannel.AuthDecision) -> Void) {
        let settings = AppSettings.load()
        switch request.mode {
        case "device":
            if let device = PairedDevice.store.load().first(where: { $0.id == request.deviceId }) {
                reply(.secret(device.secret))
            } else {
                reply(.reject("this Mac doesn't remember \(request.deviceName) any more. Pair it again."))
            }
        case "tailscale":
            guard settings.allowTailscale else { reply(.reject("Tailscale sign-in is turned off on the host")); return }
            guard let address = request.remoteAddress, Tailscale.isTailscaleIP(address) else {
                reply(.reject("Tailscale sign-in only works over a Tailscale address"))
                return
            }
            DispatchQueue.global().async { [weak self] in
                guard let self else { return }
                let who = Tailscale.whoisLogin(address)
                let me = self.tailscaleLogin()
                if let who, let me, who == me {
                    Log.info("host: Tailscale says \(request.deviceName) belongs to \(who): letting it in")
                    reply(.tailscaleVerified)
                } else {
                    reply(.reject("\(who ?? "that device") isn't this Mac's Tailscale account\(me.map { " (\($0))" } ?? "")"))
                }
            }
        case "code":
            guard settings.allowPairing else { reply(.reject("pairing new Macs is turned off on the host")); return }
            if let p = pairing, p.channel !== channel {
                reply(.reject("another Mac is pairing right now; try again in a moment"))
                return
            }
            let code = HandshakeV4.pairingCode()
            pairing = (channel, code, request.deviceName)
            Log.info("host: \(request.deviceName) wants to pair; showing a code on this screen")
            DispatchQueue.main.async { self.onPairingCode?(request.deviceName, code) }
            if let file = ProcessInfo.processInfo.environment["BB1_TEST_PAIRING_CODE_FILE"] {
                try? code.write(toFile: file, atomically: true, encoding: .utf8) // automated tests only
            }
            reply(.code(code))
        default:
            reply(.reject("unknown sign-in method \(request.mode)"))
        }
    }

    /// This Mac's own Tailscale account, cached for 5 minutes. Blocks; call off the main thread.
    private func tailscaleLogin() -> String? {
        let now = Clock.nowUs()
        if let login = ownTailscaleLogin.login, now - ownTailscaleLogin.atUs < 300_000_000 { return login }
        let login = Tailscale.status()?.selfLogin
        queue.async { self.ownTailscaleLogin = (login, now) }
        return login
    }

    private func finishPairing(_ paired: Bool) {
        guard pairing != nil else { return }
        pairing = nil
        DispatchQueue.main.async { self.onPairingFinished?(paired) }
    }

    func cancelPairing() {
        queue.async {
            self.pairing?.channel.close(.rejected("pairing cancelled on the host"))
            self.finishPairing(false)
        }
    }

    /// After sign-in: give the client a remembered pairing (unless it already used one).
    private func rememberDevice(_ channel: ControlChannel) {
        guard channel.peerVersion >= 4, !channel.peerId.isEmpty else { return }
        var devices = PairedDevice.store.load()
        if channel.authMode == "device" {
            if let i = devices.firstIndex(where: { $0.id == channel.peerId }) {
                devices[i].lastSeen = Date()
                devices[i].name = channel.peerName
                PairedDevice.store.save(devices)
            }
            return
        }
        let secret = DeviceIdentity.randomSecret()
        devices.removeAll { $0.id == channel.peerId }
        devices.append(PairedDevice(id: channel.peerId, name: channel.peerName, secret: secret,
                                    pairedAt: Date(), lastSeen: Date(), via: channel.authMode))
        PairedDevice.store.save(devices)
        channel.send(.paired(PairedMsg(hostId: hostId, hostName: hostName, secret: secret)))
        Log.info("host: paired with \(channel.peerName) (via \(channel.authMode)); it won't need to sign in again")
        if pairing?.channel === channel { finishPairing(true) }
    }

    func forgetPairedDevices() {
        PairedDevice.store.save([])
        Log.info("host: forgot all paired devices")
    }

    private func startSession(_ channel: ControlChannel, keys: SessionKeys) {
        if session != nil { endSession("replaced by a new connection from \(channel.peerName)") }
        nextSessionId &+= 1
        let s = HostSession(id: nextSessionId, control: channel, keys: keys, name: channel.peerName, paceMbps: options.paceMbps)
        s.dropPercent = options.dropPercent
        session = s
        Log.info("host: \(s.name) connected (session \(s.id)) from \(channel.connection.endpoint)")
        channel.onMessage = { [weak self, weak s] message in
            guard let self, let s, self.session === s else { return }
            self.handle(message, from: s)
        }
        channel.send(.welcome(hostName: hostName))
        Log.info("host: \(channel.peerName) signed in via \(channel.authMode)")
        rememberDevice(channel)
        Log.info("host: client speaks protocol v\(channel.peerVersion)\(s.supportsV2 ? " (retransmit, clipboard, shared logs)" : " (old client: no retransmit or clipboard)")")
        statsBaseline = (0, 0, pipeline.counters.get.captured, pipeline.counters.get.encoded, pipeline.counters.get.keyframes, 0, 0)
        if s.supportsV2 && ProcessInfo.processInfo.environment["BB1_NO_CLIPBOARD"] == nil {
            clipboard.onLocalCopy = { [weak self, weak s] message in
                guard let self, let s else { return }
                self.queue.async { if self.session === s { s.control.send(.clipboard(message)) } }
            }
            clipboard.start()
        }
        if !HostController.hasAccessibility {
            channel.send(.notice("The host can't control the mouse or keyboard yet: allow bb1 in System Settings → Privacy & Security → Accessibility on the host."))
        }
        if let error = pipeline.counters.get.lastError {
            channel.send(.notice(error))
        }
        warnIfLowOnDisk(s)
        startGentlyIfRelayed(s)
        activity = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .latencyCritical, .idleSystemSleepDisabled, .idleDisplaySleepDisabled],
            reason: "bb1 is streaming to \(s.name)")
        pipeline.start()
        // If the pipeline was already running (client replaced), resend its config right away.
        if let info = pipeline.info { sendStreamConfig(info) }
        pipeline.requestKeyframe("new session")
        notifyChange()
    }

    // MARK: Updates

    /// A client checking this Mac's build, or asking it to update. Leaves any stream alone.
    private func startUpdateSession(_ channel: ControlChannel) {
        updateChannels.append(channel)
        channel.onMessage = { [weak self, weak channel] message in
            guard let self, let channel else { return }
            switch message {
            case .updateHost(let build): self.updateRequested(build, by: channel)
            case .requestPreview(let maxWidth): self.sendPreview(maxWidth: maxWidth, to: channel)
            case .ping(let p): // the dashboard's latency check
                channel.send(.pong(PongMsg(seq: p.seq, clientUs: p.clientUs, hostUs: Clock.nowUs(), captured: 0,
                                           encoded: 0, sent: 0, encodeAvgUs: 0, hostRecoveries: 0)))
            default: break
            }
        }
        // Don't keep idle ones around.
        queue.asyncAfter(deadline: .now() + 600) { [weak channel] in channel?.close(.closed("update session timed out")) }
    }

    /// Channels with a screen preview being captured (one at a time each).
    private var previewing: Set<ObjectIdentifier> = []

    private func sendPreview(maxWidth: Int, to channel: ControlChannel) {
        let id = ObjectIdentifier(channel)
        guard !previewing.contains(id) else { return }
        previewing.insert(id)
        ScreenPreview.capture(maxWidth: min(max(maxWidth, 160), 1600)) { [weak self, weak channel] preview in
            guard let self else { return }
            self.queue.async {
                self.previewing.remove(id)
                channel?.send(.preview(preview))
            }
        }
    }

    /// When the disk is nearly full, macOS stops screen capture while it frees up space (the picture
    /// freezes for a moment). Say why, once per session.
    private func warnIfLowOnDisk(_ s: HostSession) {
        guard !s.warnedLowDisk, let free = HostController.freeDiskBytes, free < 5_000_000_000 else { return }
        s.warnedLowDisk = true
        let gb = String(format: "%.1f GB", Double(free) / 1_000_000_000)
        Log.info("host: low on disk space (\(gb) free); macOS interrupts screen capture while it frees space")
        s.control.send(.notice("\(hostName) is almost out of disk space (\(gb) free). While macOS frees up space it pauses screen sharing, so the picture can freeze for a few seconds. Free up some space on \(hostName) to fix it."))
    }

    static var freeDiskBytes: Int64? {
        let values = try? URL(fileURLWithPath: NSHomeDirectory()).resourceValues(forKeys: [.volumeAvailableCapacityKey])
        return values?.volumeAvailableCapacity.map(Int64.init)
    }

    private func updateRequested(_ build: Int, by channel: ControlChannel) {
        Log.info("host: \(channel.peerName) asked this Mac to update from build \(BB1.build) to \(build)")
        if let s = session, s.control !== channel {
            s.control.send(.notice("bb1 on this Mac is updating to \(BB1.buildName(build)). It will be back in a moment."))
        }
        let reply: (UpdateStatusMsg) -> Void = { [weak self, weak channel] status in
            self?.queue.async { channel?.send(.updateStatus(status)) }
        }
        DispatchQueue.main.async { Updater.shared.installBuild(build, report: reply) }
    }

    private func endSession(_ reason: String) {
        guard let s = session else { return }
        Log.info("host: session \(s.id) with \(s.name) ended: \(reason)")
        session = nil
        cursorWatcher.stop()
        cursorWatcher.onChange = nil
        pipeline.setShowsCursor(true)
        clipboard.stop()
        clipboard.onLocalCopy = nil
        s.close()
        injector.releaseAll()
        pipeline.stop()
        if let activity { ProcessInfo.processInfo.endActivity(activity) }
        activity = nil
        notifyChange()
    }

    func disconnectClient() {
        queue.async {
            self.session?.control.send(.bye("disconnected from the host menu"))
            self.queue.asyncAfter(deadline: .now() + 0.2) { self.endSession("disconnected from the host menu") }
        }
    }

    private func sendStreamConfig(_ info: StreamInfo) {
        guard let s = session else { return }
        let config = StreamConfig(width: info.width, height: info.height, codec: info.codec, fps: info.fps,
                                  bitrateMbps: info.bitrateMbps, lowLatencyEncoder: info.lowLatency,
                                  udpPort: options.videoPort, sessionId: s.id, epoch: info.epoch)
        s.config = config
        s.control.send(.streamConfig(config))
        notifyChange()
    }

    private func handle(_ message: ControlMessage, from s: HostSession) {
        switch message {
        case .ping(let p):
            s.lastPingUs = Clock.nowUs()
            let c = pipeline.counters.get
            s.control.send(.pong(PongMsg(seq: p.seq, clientUs: p.clientUs, hostUs: Clock.nowUs(),
                                         captured: c.captured, encoded: c.encoded, sent: s.sent.get,
                                         encodeAvgUs: UInt64(c.encodeAvgUs), hostRecoveries: c.recoveryTotal)))
        case .requestKeyframe(let reason):
            pipeline.requestKeyframe("client: \(reason)")
        case .key(let k):
            if k.down { input.keys += 1; noteInput() }
            injector.key(k)
        case .mouseMove(let m):
            input.moves += 1
            // Only a real cursor move should produce a new frame; without Accessibility nothing moves.
            if injector.move(m) && HostController.hasAccessibility { lastPointerUs = Clock.nowUs() }
        case .mouseButton(let b):
            if b.down { input.clicks += 1; noteInput() }
            injector.button(b)
        case .scroll(let sc):
            input.scrolls += 1
            injector.scroll(sc)
        case .releaseAll:
            injector.releaseAll()
        case .capsLock(let on):
            injector.setCapsLock(on)
        case .updateHost(let build):
            updateRequested(build, by: s.control)
        case .setCursorInVideo(let inVideo):
            pipeline.setShowsCursor(inVideo)
            // The client draws its own cursor: keep it the same shape as this Mac's.
            if inVideo || !s.supportsV2 {
                cursorWatcher.stop()
            } else {
                cursorWatcher.onChange = { [weak self, weak s] cursor in
                    guard let self, let s else { return }
                    self.queue.async { if self.session === s { s.control.send(.cursor(cursor)) } }
                }
                cursorWatcher.start()
            }
        case .bye(let reason):
            endSession("client said bye: \(reason)")
        case .nack(let n):
            s.resend(n)
        case .clientLog(let line):
            Log.local("[\(s.name)] \(line)")
        case .clipboard(let c):
            clipboard.apply(c)
        case .setAudio(let on):
            Log.info("host: sound \(on ? "on" : "off") (client's choice)")
            if !on { audio.reset() }
            s.audioEnabled.with { $0 = on }
        default:
            break
        }
    }

    // MARK: Video path

    private func acceptUDP(_ conn: NWConnection) {
        pendingUDP.append(conn)
        conn.stateUpdateHandler = { [weak self, weak conn] state in
            guard let self, let conn else { return }
            if case .failed = state { conn.cancel() }
            if case .cancelled = state { self.pendingUDP.removeAll { $0 === conn } }
        }
        conn.start(queue: queue)
        receiveUDP(conn)
        // Drop flows that never say hello.
        queue.asyncAfter(deadline: .now() + 10) { [weak self, weak conn] in
            guard let self, let conn else { return }
            if let s = self.session, s.hasVideoPath, s.attach(udp: conn) == false { return }
            if self.pendingUDP.contains(where: { $0 === conn }) && self.session?.hasVideoPath != true { conn.cancel() }
        }
    }

    private func receiveUDP(_ conn: NWConnection) {
        conn.receiveMessage { [weak self, weak conn] data, _, _, error in
            guard let self, let conn else { return }
            if let data, let sessionId = UDPHello.parse(data), let s = self.session, s.id == sessionId,
               UDPHello.verify(data, key: s.keys.video) {
                if s.attach(udp: conn) {
                    Log.info("host: video path to \(conn.endpoint) ready for session \(s.id)")
                    self.pipeline.requestKeyframe("video path ready")
                }
            }
            if error == nil { self.receiveUDP(conn) }
        }
    }

    // MARK: Health

    private func startWatchdog() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 0.25, repeating: 0.25)
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            // A timer that fires very late means this Mac itself stalled (memory pressure, swapping, etc.).
            let now = Clock.nowUs()
            if self.lastWatchdogUs > 0, now - self.lastWatchdogUs > 750_000 {
                Log.info("host: HITCH this Mac stalled for \((now - self.lastWatchdogUs - 250_000) / 1000) ms (system busy?)")
            }
            self.lastWatchdogUs = now
            guard let s = self.session else { return }
            self.watchdogTicks += 1
            if self.watchdogTicks % 4 == 0 { self.logStats(s) }
            if Clock.nowUs() - s.lastPingUs > 3_000_000 {
                self.endSession("no heartbeat from the client for 3 s")
                return
            }
            self.pipeline.checkHealth(lastPointerUs: self.lastPointerUs, recentInputUs: self.recentInputUs)
        }
        timer.resume()
        watchdog = timer

        let pressure = DispatchSource.makeMemoryPressureSource(eventMask: [.warning, .critical, .normal], queue: queue)
        pressure.setEventHandler { [weak pressure] in
            guard let event = pressure?.data else { return }
            let level = event.contains(.critical) ? "CRITICAL" : event.contains(.warning) ? "warning" : "back to normal"
            Log.info("host: memory pressure \(level)")
        }
        pressure.resume()
        memoryPressure = pressure
    }

    /// Once a second while a client is connected. Counts only, never what was typed.
    private func logStats(_ s: HostSession) {
        let c = pipeline.counters.with { c -> PipelineCounters in
            let copy = c
            c.encodeMaxUs = 0
            c.biggestFrameBytes = 0
            return copy
        }
        let sent = s.sent.get, bytes = s.sentBytes.get, nacks = s.nacks.get, resent = s.resentPackets.get
        let usage = resources.sample()
        let b = statsBaseline
        let mbps = Double(bytes &- b.bytes) * 8 / 1_000_000
        Log.info(String(format: "stats: captured %llu encoded %llu sent %llu fps · %.1f Mbps · biggest frame %d KB · encode avg %.1f max %.1f ms · keyframes %llu · nacks %llu, resent %llu packets · input %d moves %d clicks %d keys %d scrolls · bb1 cpu %.0f%% mem %.0f MB",
                        c.captured &- b.captured, c.encoded &- b.encoded, sent &- b.sent, mbps, c.biggestFrameBytes / 1000,
                        c.encodeAvgUs / 1000, Double(c.encodeMaxUs) / 1000, c.keyframes &- b.keyframes,
                        nacks &- b.nacks, resent &- b.resent, input.moves, input.clicks, input.keys, input.scrolls,
                        usage.cpuPercent, usage.memoryMB))
        statsBaseline = (sent, bytes, c.captured, c.encoded, c.keyframes, nacks, resent)
        input = (0, 0, 0, 0)
        adaptRate(s, packets: (bytes &- b.bytes) / 1200, resent: resent &- b.resent)
    }

    /// Lost packets mean we're sending more than the path carries (a relay, a far-away Mac, busy Wi-Fi):
    /// back off fast, then creep back up while nothing is lost. Lower bitrate and gentler bursts.
    private func adaptRate(_ s: HostSession, packets: UInt64, resent: UInt64) {
        var rate = s.rate
        if resent >= max(4, packets / 50) {
            rate = max(0.15, rate * 0.7)
            s.cleanSeconds = 0
        } else {
            s.cleanSeconds += 1
            if s.cleanSeconds >= 4 && rate < 1 {
                rate = min(1, rate * 1.15)
                s.cleanSeconds = 2
            }
        }
        guard rate != s.rate else { return }
        setRate(rate, for: s, because: rate < s.rate ? "\(resent) packets resent" : "no loss")
    }

    private func setRate(_ rate: Double, for s: HostSession, because reason: String) {
        let full = pipeline.info.map { pipeline.settings.bitrateMbps ?? StreamPipeline.defaultBitrate(width: $0.width, height: $0.height) } ?? 30
        s.setRate(rate, bitrateMbps: max(3, Int(Double(full) * rate)))
        pipeline.setBitrateScale(rate)
        Log.info(String(format: "host: sending at %.0f%% of full quality (%@)", rate * 100, reason))
    }

    /// Over a Tailscale relay, start at a rate a relay can carry instead of losing the first keyframe.
    private func startGentlyIfRelayed(_ s: HostSession) {
        guard let address = s.control.remoteAddress, Tailscale.isTailscaleIP(address) else { return }
        DispatchQueue.global().async { [weak self] in
            guard let path = Tailscale.pathDescription(to: address, in: Tailscale.status()) else { return }
            Log.info("host: \(s.name) is connected via Tailscale, \(path)")
            guard path.hasPrefix("relayed") else { return }
            self?.queue.async {
                guard let self, self.session === s, s.rate > 0.4 else { return }
                self.setRate(0.4, for: s, because: "Tailscale relay")
            }
        }
    }

    private func watchDisplayChanges() {
        CGDisplayRegisterReconfigurationCallback(HostController.displayCallback, Unmanaged.passUnretained(self).toOpaque())
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: nil) { [weak self] _ in
            Log.info("host: system woke up")
            self?.pipeline.restartAll("system woke from sleep")
        }
    }

    fileprivate func displaysChanged() {
        queue.async {
            self.displayDebounce?.cancel()
            let work = DispatchWorkItem { [weak self] in
                Log.info("host: display configuration changed")
                self?.pipeline.restartAll("display configuration changed")
            }
            self.displayDebounce = work
            self.queue.asyncAfter(deadline: .now() + 0.5, execute: work)
        }
    }

    private func notifyChange() {
        DispatchQueue.main.async { self.onChange?() }
    }
}
