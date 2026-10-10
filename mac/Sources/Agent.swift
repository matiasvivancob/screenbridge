// Network side of the menu-bar app: announces this Mac, accepts one PC session at a time and
// creates/removes virtual displays on request. Protocol: docs/PROTOCOL.md.

import AppKit
import Foundation

let controlPort: UInt16 = 47800
let discoveryPort: UInt16 = 47801

final class Agent {
    /// Main-queue callbacks for the UI.
    var onStatus: ((String, Bool) -> Void)? // text, active
    var approve: ((_ pcName: String, _ done: @escaping (Bool) -> Void) -> Void)?

    let name = Host.current().localizedName ?? "Mac"
    private var session: Session?

    func start() {
        broadcast()
        Thread.detachNewThread { self.serve() }
    }

    /// Disconnects the current PC (menu "Stop").
    func stop() {
        session?.end(reason: "Stopped from the Mac")
    }

    // MARK: approvals (UserDefaults: pc_id -> pc name)

    var allowed: [String: String] {
        get { UserDefaults.standard.dictionary(forKey: "allowedPCs") as? [String: String] ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "allowedPCs") }
    }

    // MARK: discovery

    private func broadcast() {
        let fd = socket(AF_INET, SOCK_DGRAM, 0)
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_BROADCAST, &on, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_port = discoveryPort.bigEndian
        a.sin_addr.s_addr = INADDR_BROADCAST
        let msg = json(["type": "announce", "name": name, "port": Int(controlPort), "version": 1])
        var failures = 0
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in
            let sent = withUnsafePointer(to: &a) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    sendto(fd, msg, msg.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            if sent < 0 {
                failures += 1
                // Usually macOS Local Network privacy: the user hasn't allowed ScreenBridge yet.
                if failures == 1 || failures % 30 == 0 {
                    log("announce failed: \(String(cString: strerror(errno))) (errno \(errno), \(failures) failures)")
                }
            } else if failures > 0 {
                log("announce working again after \(failures) failures")
                failures = 0
            }
        }.fire()
    }

    // MARK: control server

    private func serve() {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_port = controlPort.bigEndian
        let bound = withUnsafePointer(to: &a) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0, listen(fd, 4) == 0 else {
            DispatchQueue.main.async { self.onStatus?("Port \(controlPort) is in use. Is ScreenBridge already running?", false) }
            return
        }
        while true {
            var peer = sockaddr_in()
            var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            let c = withUnsafeMutablePointer(to: &peer) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { accept(fd, $0, &len) }
            }
            if c < 0 { continue }
            var ip = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            inet_ntop(AF_INET, &peer.sin_addr, &ip, socklen_t(INET_ADDRSTRLEN))
            let s = Session(fd: c, ip: String(cString: ip), agent: self)
            DispatchQueue.main.async {
                self.session?.end(reason: "Another PC connected")
                self.session = s
            }
            Thread.detachNewThread { s.read() }
        }
    }

    fileprivate func ended(_ s: Session, reason: String) {
        if session === s {
            session = nil
            onStatus?(reason, false)
        }
    }
}

func json(_ o: [String: Any]) -> [UInt8] {
    Array((try? JSONSerialization.data(withJSONObject: o)) ?? Data()) + [10]
}

/// One connected PC. All state changes happen on the main queue.
final class Session {
    let fd: Int32
    let ip: String
    unowned let agent: Agent
    var pcName = "PC"
    var welcomed = false
    var screens: [Int: Screen] = [:]
    var lastPing = Date()
    var watchdog: Timer?
    var ended = false
    let writeLock = NSLock()

    init(fd: Int32, ip: String, agent: Agent) {
        self.fd = fd
        self.ip = ip
        self.agent = agent
    }

    func send(_ o: [String: Any]) {
        let b = json(o)
        writeLock.lock()
        _ = b.withUnsafeBytes { Darwin.send(fd, $0.baseAddress, b.count, 0) }
        writeLock.unlock()
    }

    /// Reads newline-delimited JSON until the socket closes (runs on its own thread).
    func read() {
        var buf = [UInt8]()
        var chunk = [UInt8](repeating: 0, count: 4096)
        while true {
            let n = recv(fd, &chunk, chunk.count, 0)
            if n <= 0 { break }
            buf += chunk[0..<n]
            while let nl = buf.firstIndex(of: 10) {
                let line = Data(buf[..<nl])
                buf.removeSubrange(...nl)
                if let msg = try? JSONSerialization.jsonObject(with: line) as? [String: Any] {
                    DispatchQueue.main.async { self.handle(msg) }
                }
            }
            if buf.count > 1 << 20 { break } // a line this long is not our protocol
        }
        DispatchQueue.main.async { self.end(reason: "\(self.pcName) disconnected") }
    }

    func handle(_ m: [String: Any]) {
        guard !ended else { return }
        lastPing = Date()
        switch m["type"] as? String {
        case "hello":
            let id = m["pc_id"] as? String ?? ""
            pcName = (m["pc_name"] as? String).map { String($0.prefix(64)) } ?? "PC"
            guard id.count >= 16 else { send(["type": "denied"]); return end(reason: "Rejected a PC without an id") }
            if agent.allowed[id] != nil {
                welcome()
            } else {
                send(["type": "pending"])
                agent.onStatus?("Waiting for you to allow \(pcName)…", false)
                agent.approve?(pcName) { ok in
                    guard !self.ended else { return }
                    if ok {
                        self.agent.allowed[id] = self.pcName
                        self.welcome()
                    } else {
                        self.send(["type": "denied"])
                        self.end(reason: "Denied \(self.pcName)")
                    }
                }
            }
        case "extend" where welcomed:
            extend(m)
        case "remove" where welcomed:
            if let id = m["id"] as? Int, let s = screens.removeValue(forKey: id) { s.stop() }
            if screens.isEmpty { end(reason: "\(pcName) closed its viewers") } else { status() }
        case "stop":
            end(reason: "\(pcName) stopped")
        default:
            break // ping, or unknown messages from newer versions
        }
    }

    func welcome() {
        welcomed = true
        send(["type": "welcome", "name": agent.name])
        agent.onStatus?("Connected to \(pcName)", false)
        watchdog = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            guard let self else { return }
            if Date().timeIntervalSince(self.lastPing) > 6 { self.end(reason: "Lost contact with \(self.pcName)") }
        }
    }

    func extend(_ m: [String: Any]) {
        guard CGPreflightScreenCaptureAccess() else {
            CGRequestScreenCaptureAccess()
            send(["type": "error", "message": "Screen Recording permission is missing on the Mac. Allow ScreenBridge in System Settings > Privacy & Security > Screen Recording, then quit and reopen ScreenBridge."])
            return
        }
        let st = m["settings"] as? [String: Any] ?? [:]
        var enc = EncoderSettings()
        enc.hevc = st["codec"] as? String == "hevc"
        enc.mbps = min(max(st["mbps"] as? Int ?? 30, 1), 200)
        enc.fps = st["fps"] as? Int == 120 ? 120 : 60
        enc.speed = st["speed"] as? Bool ?? false
        enc.lowLatencyRC = st["low_latency_rc"] as? Bool ?? true

        for s in screens.values { s.stop() }
        screens = [:]
        for d in (m["displays"] as? [[String: Any]] ?? []).prefix(8) {
            guard let id = d["id"] as? Int, let w = d["width"] as? Int, let h = d["height"] as? Int, let port = d["port"] as? Int,
                  (640...7680).contains(w), (480...4320).contains(h), (1024...65535).contains(port),
                  let link = Link(host: ip, port: UInt16(port)) else { continue }
            let s = Screen(index: screens.count, w: w & ~1, h: h & ~1, link: link, settings: enc)
            s.onError = { [weak self] msg in
                self?.send(["type": "error", "message": msg])
                self?.end(reason: msg)
            }
            screens[id] = s
            s.start()
        }
        send(["type": "extended", "count": screens.count])
        status()
    }

    func status() {
        agent.onStatus?("Extending to \(pcName): \(screens.count) display\(screens.count == 1 ? "" : "s")", !screens.isEmpty)
    }

    func end(reason: String) {
        guard !ended else { return }
        ended = true
        watchdog?.invalidate()
        for s in screens.values { s.stop() }
        screens = [:] // releasing the Screens removes the virtual displays
        shutdown(fd, SHUT_RDWR)
        close(fd)
        agent.ended(self, reason: reason)
    }
}
