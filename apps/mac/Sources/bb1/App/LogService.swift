import BB1Core
import Foundation
import Network

/// Lets your other devices read this Mac's bb1 logs over Tailscale (port 47802, plain HTTP), the same
/// way the Windows app serves its own (apps/windows/src/core/logserve.rs):
///
///     curl http://kenobi:47802/                  what this Mac is, and its log files
///     curl http://kenobi:47802/logs/host.log     the whole file (?tail=200000 for the last 200 KB)
///
/// Only devices on the same Tailscale account get in (checked with `tailscale whois`), plus this Mac
/// itself. Logs hold counts and events, never what was typed or shown. `scripts/bb1-logs.sh` fetches.
final class LogService {
    static let shared = LogService()
    static let port: UInt16 = 47802

    private var listener: NWListener?
    private let queue = DispatchQueue(label: "bb1.logs")
    private var allowed: [String: (ok: Bool, at: Date)] = [:]
    private var dir: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/bb1", isDirectory: true)
    }

    func start() {
        guard listener == nil else { return }
        do {
            let l = try NWListener(using: .tcp, on: NWEndpoint.Port(rawValue: LogService.port)!)
            l.newConnectionHandler = { [weak self] c in self?.serve(c) }
            l.stateUpdateHandler = { state in
                if case .failed(let e) = state { Log.info("logs: not serving logs on port \(LogService.port) (\(e))") }
            }
            l.start(queue: queue)
            listener = l
            Log.info("logs: devices on this Tailscale account can read these logs on port \(LogService.port)")
        } catch {
            Log.info("logs: not serving logs on port \(LogService.port) (\(error))")
        }
    }

    private func serve(_ c: NWConnection) {
        c.start(queue: queue)
        c.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] data, _, _, _ in
            guard let self else { c.cancel(); return }
            let request = String(decoding: data ?? Data(), as: UTF8.self)
            let target = request.split(separator: "\r\n").first?.split(separator: " ").dropFirst().first.map(String.init) ?? "/"
            var peer = ""
            if case .hostPort(let h, _) = c.endpoint { peer = "\(h)".components(separatedBy: "%").first ?? "" }
            if peer.hasPrefix("::ffff:") { peer = String(peer.dropFirst(7)) }
            DispatchQueue.global().async {
                let verdict = self.check(peer)
                self.queue.async {
                    if let why = verdict {
                        Log.info("logs: refused \(peer) (\(why))")
                        self.respond(c, "403 Forbidden", Data("bb1: \(why)\n".utf8))
                    } else {
                        self.answer(c, target)
                    }
                }
            }
        }
    }

    /// nil = allowed. Blocks (tailscale whois); call off the main thread.
    private func check(_ peer: String) -> String? {
        if peer == "127.0.0.1" || peer == "::1" { return nil }
        guard Tailscale.isTailscaleIP(peer) else { return "only over Tailscale" }
        if let a = queue.sync(execute: { allowed[peer] }), Date().timeIntervalSince(a.at) < 300 {
            return a.ok ? nil : "not this Mac's Tailscale account"
        }
        let who = Tailscale.whoisLogin(peer)
        let me = Tailscale.status()?.selfLogin
        let ok = who != nil && who == me
        queue.sync { allowed[peer] = (ok, Date()) }
        return ok ? nil : "not this Mac's Tailscale account"
    }

    private func answer(_ c: NWConnection, _ target: String) {
        let parts = target.split(separator: "?", maxSplits: 1).map(String.init)
        let path = parts.first ?? "/"
        let tail = parts.count > 1
            ? parts[1].split(separator: "&").compactMap { $0.hasPrefix("tail=") ? UInt64($0.dropFirst(5)) : nil }.first ?? 4_000_000
            : 4_000_000
        if path == "/" {
            var text = "bb1 for Mac, \(BB1.displayName) (build \(BB1.build)), on \(Host.current().localizedName ?? "Mac")\nLogs (\(dir.path)):\n"
            let files = ((try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []).filter { $0.hasSuffix(".log") }.sorted()
            for f in files {
                let size = (try? FileManager.default.attributesOfItem(atPath: dir.appendingPathComponent(f).path)[.size] as? UInt64) ?? 0
                text += "  /logs/\(f)  \(size / 1000) KB\n"
            }
            respond(c, "200 OK", Data(text.utf8))
            return
        }
        guard path.hasPrefix("/logs/") else { respond(c, "404 Not Found", Data("bb1: try / or /logs/<name>.log\n".utf8)); return }
        let name = String(path.dropFirst(6))
        guard name.hasSuffix(".log"), !name.contains("/"), !name.contains(".."),
              let handle = FileHandle(forReadingAtPath: dir.appendingPathComponent(name).path) else {
            respond(c, "404 Not Found", Data("bb1: no such log\n".utf8))
            return
        }
        let size = handle.seekToEndOfFile()
        handle.seek(toFileOffset: size > min(tail, 50_000_000) ? size - min(tail, 50_000_000) : 0)
        let body = handle.readDataToEndOfFile()
        try? handle.close()
        respond(c, "200 OK", body)
    }

    private func respond(_ c: NWConnection, _ status: String, _ body: Data) {
        let head = "HTTP/1.0 \(status)\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n"
        c.send(content: Data(head.utf8) + body, completion: .contentProcessed { _ in c.cancel() })
    }
}
