// Network side of the menu-bar app: announces this Mac, accepts one linked PC at a time and creates/removes
// virtual displays on request. A link stays up while no display is on; displays toggle individually.
// Protocol: docs/PROTOCOL.md.

import AppKit
import Foundation

let controlPort: UInt16 = 47800
let discoveryPort: UInt16 = 47801
let maxDisplays = 8

/// One display the linked PC offers, as listed in its `layout` message.
struct LayoutDisplay {
    let id: Int
    let name: String
    let width: Int
    let height: Int
}

final class Agent {
    /// Main-queue callbacks for the UI.
    var onStatus: ((String, Bool) -> Void)? // text, any display on
    var approve: ((_ pcName: String, _ done: @escaping (Bool) -> Void) -> Void)?

    let name = Host.current().localizedName ?? "Mac"
    private var session: Session?

    func start() {
        broadcast()
        Thread.detachNewThread { self.serve() }
    }

    /// Disconnects the linked PC (menu "Disconnect"). The PC will not reconnect by itself.
    func stop() {
        session?.end(reason: "Disconnected from the Mac", tellPC: true)
    }

    /// The linked PC, its offered displays and which of them are on. Main queue only.
    var linkInfo: (pc: String, displays: [LayoutDisplay], on: Set<Int>)? {
        guard let s = session, s.welcomed, !s.ended else { return nil }
        return (s.pcName, s.layout, Set(s.screens.keys))
    }

    /// Asks the PC to turn one of its displays on or off (the PC owns the viewer windows).
    func toggle(id: Int, on: Bool) {
        session?.send(["type": "toggle", "id": id, "on": on])
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
            // A peer that stops reading must not be able to block us (or kill us with SIGPIPE).
            var one: Int32 = 1
            setsockopt(c, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
            var tv = timeval(tv_sec: 2, tv_usec: 0)
            setsockopt(c, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
            var ip = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            inet_ntop(AF_INET, &peer.sin_addr, &ip, socklen_t(INET_ADDRSTRLEN))
            let s = Session(fd: c, ip: String(cString: ip), agent: self)
            // A new connection never evicts the active PC. It becomes active only once it is approved (welcome()).
            DispatchQueue.main.asyncAfter(deadline: .now() + 10) {
                if !s.welcomed && !s.awaitingApproval { s.end(reason: "Dropped a connection that never said hello", tellPC: false) }
            }
            Thread.detachNewThread { s.read() }
        }
    }

    /// `s` has been approved: it replaces the previously active PC, if any.
    fileprivate func activate(_ s: Session) {
        if let old = session, old !== s { old.end(reason: "Another PC connected", tellPC: false) }
        session = s
    }

    fileprivate func ended(_ s: Session, reason: String) {
        if session === s {
            session = nil
            onStatus?(reason, false)
        }
    }

    /// Forgets a PC and disconnects it right away if it is the one linked now.
    func revoke(id: String) {
        allowed.removeValue(forKey: id)
        if let s = session, s.pcID == id { s.end(reason: "Removed \(s.pcName)", tellPC: true) }
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
    var pcID = ""
    var welcomed = false
    var awaitingApproval = false
    var screens: [Int: Screen] = [:]
    var layout: [LayoutDisplay] = []
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
        DispatchQueue.main.async { self.end(reason: "\(self.pcName) disconnected", tellPC: false) }
    }

    func handle(_ m: [String: Any]) {
        guard !ended else { return }
        lastPing = Date()
        switch m["type"] as? String {
        case "hello":
            let id = m["pc_id"] as? String ?? ""
            pcID = id
            pcName = (m["pc_name"] as? String).map { String($0.prefix(64)) } ?? "PC"
            guard id.count >= 16 else { send(["type": "denied"]); return end(reason: "Rejected a PC without an id", tellPC: false) }
            if agent.allowed[id] != nil {
                welcome()
            } else {
                send(["type": "pending"])
                awaitingApproval = true
                agent.onStatus?("Waiting for you to allow \(pcName)…", false)
                agent.approve?(pcName) { ok in
                    guard !self.ended else { return }
                    if ok {
                        self.agent.allowed[id] = self.pcName
                        self.welcome()
                    } else {
                        self.send(["type": "denied"])
                        self.end(reason: "Denied \(self.pcName)", tellPC: false)
                    }
                }
            }
        case "layout" where welcomed:
            layout = (m["displays"] as? [[String: Any]] ?? []).prefix(maxDisplays).compactMap { d in
                guard let id = d["id"] as? Int, (0..<maxDisplays).contains(id),
                      let w = d["width"] as? Int, let h = d["height"] as? Int else { return nil }
                let name = (d["name"] as? String).map { String($0.prefix(40)) } ?? "Display \(id + 1)"
                return LayoutDisplay(id: id, name: name, width: w, height: h)
            }
        case "add" where welcomed:
            add(m)
        case "remove" where welcomed:
            if let id = m["id"] as? Int, let s = screens.removeValue(forKey: id) { s.stop() }
            changed()
        case "ping" where welcomed:
            send(["type": "pong"]) // lets the PC detect a Mac that vanished without closing the connection
        case "stop":
            end(reason: "\(pcName) disconnected", tellPC: false)
        default:
            break // ping, or unknown messages from newer versions
        }
    }

    func welcome() {
        guard !welcomed else { return }
        welcomed = true
        agent.activate(self)
        send(["type": "welcome", "name": agent.name])
        status()
        watchdog = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            guard let self else { return }
            if Date().timeIntervalSince(self.lastPing) > 6 { self.end(reason: "Lost contact with \(self.pcName)", tellPC: false) }
        }
    }

    /// Turns one display on. The PC has already opened its viewer and is listening on `port`.
    func add(_ m: [String: Any]) {
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

        guard let d = m["display"] as? [String: Any],
              let id = d["id"] as? Int, (0..<maxDisplays).contains(id),
              let w = d["width"] as? Int, let h = d["height"] as? Int, let port = d["port"] as? Int,
              (640...7680).contains(w), (480...7680).contains(h), (1024...65535).contains(port),
              let link = Link(host: ip, port: UInt16(port)) else {
            send(["type": "error", "message": "The Mac couldn't use that display request."])
            return
        }
        if let old = screens.removeValue(forKey: id) { old.stop() }
        // The slot number is the display's identity (vendor/product/serial), so macOS remembers its arrangement.
        let s = Screen(index: id, w: w & ~1, h: h & ~1, link: link, settings: enc)
        s.onError = { [weak self] msg in
            guard let self, self.screens[id] === s else { return }
            self.screens.removeValue(forKey: id)?.stop()
            self.send(["type": "error", "message": msg])
            self.changed()
        }
        screens[id] = s
        s.start()
        changed()
    }

    /// Tells the PC which displays are on and refreshes the Mac's status.
    func changed() {
        send(["type": "state", "on": screens.keys.sorted()])
        status()
    }

    func status() {
        let n = screens.count
        let text = "Linked with \(pcName) · " + (n == 0 ? "no displays on" : "\(n) display\(n == 1 ? "" : "s") on")
        agent.onStatus?(text, n > 0)
    }

    func end(reason: String, tellPC: Bool) {
        guard !ended else { return }
        ended = true
        if tellPC { send(["type": "bye"]) } // tells the PC not to reconnect on its own
        watchdog?.invalidate()
        for s in screens.values { s.stop() }
        screens = [:] // releasing the Screens removes the virtual displays
        shutdown(fd, SHUT_RDWR)
        close(fd)
        agent.ended(self, reason: reason)
    }
}
