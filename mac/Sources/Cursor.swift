// Cursor position uses only CoreGraphics on its own queue; AppKit shape sampling stays on main.

import AppKit
import CoreGraphics
import Foundation

struct CursorShape {
    let id: UInt16
    let w: Int, h: Int, hx: Int, hy: Int
    let bgra: Data

    var message: [String: Any] {
        ["type": "cursor_shape", "id": Int(id), "w": w, "h": h, "hx": hx, "hy": hy,
         "bgra": bgra.base64EncodedString()]
    }
}

/// Entries and the timer are confined to q; only the shape id crosses queues, under shapeLock.
final class CursorFeed: @unchecked Sendable {
    static let shared = CursorFeed()
    private let q = DispatchQueue(label: "cursor", qos: .userInteractive)
    private let shapeLock = NSLock()
    private var shapeID: UInt16 = 0
    private var timer: DispatchSourceTimer?
    private var entries: [ObjectIdentifier: Entry] = [:]
    private let visibility: (@convention(c) () -> boolean_t)?

    private struct State: Equatable {
        let x: Int16, y: Int16
        let visible: Bool, onDisplay: Bool
        let shape: UInt16
    }

    private final class Entry {
        weak var screen: Screen?
        var enabled = false
        var seq: UInt16 = 0
        var last: State?
        var sentAt: UInt64 = 0
        var leavePackets = 0

        init(_ screen: Screen) { self.screen = screen }
    }

    private init() {
        // This deprecated API may disappear; in that case leave the pointer visible.
        guard let handle = dlopen(nil, RTLD_LAZY) else { visibility = nil; return }
        defer { dlclose(handle) }
        if let p = dlsym(handle, "CGCursorIsVisible") {
            visibility = unsafeBitCast(p, to: (@convention(c) () -> boolean_t).self)
        } else {
            visibility = nil
        }
    }

    func setShape(_ id: UInt16) {
        shapeLock.lock()
        shapeID = id
        shapeLock.unlock()
    }

    func register(_ screen: Screen) {
        q.async { [weak screen] in
            guard let screen else { return }
            self.entries = self.entries.filter { $0.value.screen != nil }
            let key = ObjectIdentifier(screen)
            let entry = self.entries[key] ?? Entry(screen)
            entry.enabled = true
            entry.last = nil
            entry.leavePackets = 0
            self.entries[key] = entry
            self.updateTimer()
        }
    }

    func unregister(_ screen: Screen) {
        let key = ObjectIdentifier(screen)
        q.async {
            // Keep the sequence across P0/P1 while the same screen is alive.
            self.entries[key]?.enabled = false
            self.updateTimer()
        }
    }

    private func updateTimer() {
        let running = entries.values.contains { $0.enabled && $0.screen != nil }
        if !running {
            timer?.cancel()
            timer = nil
        } else if timer == nil {
            let t = DispatchSource.makeTimerSource(queue: q)
            t.schedule(deadline: .now(), repeating: .milliseconds(4), leeway: .milliseconds(1))
            t.setEventHandler { [weak self] in self?.tick() }
            timer = t
            t.resume()
        }
    }

    private func tick() {
        guard let location = CGEvent(source: nil)?.location else { return }
        let visible = visibility.map { $0() != 0 } ?? true
        shapeLock.lock()
        let shape = shapeID
        shapeLock.unlock()
        let now = DispatchTime.now().uptimeNanoseconds
        for entry in entries.values where entry.enabled {
            guard let screen = entry.screen else { entry.enabled = false; continue }
            let bounds = CGDisplayBounds(screen.display.displayID)
            let state = State(x: coordinate(location.x - bounds.origin.x), y: coordinate(location.y - bounds.origin.y),
                              visible: visible, onDisplay: bounds.contains(location), shape: shape)
            if entry.last?.onDisplay == true && !state.onDisplay { entry.leavePackets = 3 }
            if state.onDisplay { entry.leavePackets = 0 }
            let heartbeat = state.onDisplay && now - entry.sentAt >= 250_000_000
            guard state != entry.last || heartbeat || entry.leavePackets > 0 else { continue }
            entry.seq &+= 1
            var p = [UInt8](repeating: 0, count: HDR)
            p.reserveCapacity(HDR + 10)
            p.append(0x02)
            p.append(UInt8((state.visible ? 1 : 0) | (state.onDisplay ? 2 : 0)))
            withUnsafeBytes(of: entry.seq.bigEndian) { p.append(contentsOf: $0) }
            withUnsafeBytes(of: state.x.bigEndian) { p.append(contentsOf: $0) }
            withUnsafeBytes(of: state.y.bigEndian) { p.append(contentsOf: $0) }
            withUnsafeBytes(of: state.shape.bigEndian) { p.append(contentsOf: $0) }
            screen.link.send(p, nonblocking: true)
            entry.last = state
            entry.sentAt = now
            if entry.leavePackets > 0 { entry.leavePackets -= 1 }
        }
        updateTimer()
    }

    private func coordinate(_ value: CGFloat) -> Int16 {
        guard value.isFinite else { return 0 }
        return Int16(min(max(value.rounded(), CGFloat(Int16.min)), CGFloat(Int16.max)))
    }
}

/// Main queue only: the agent owns the watcher and retains the last valid shape for welcome().
final class CursorShapeWatcher {
    private var timer: DispatchSourceTimer?
    private var lastHash: UInt64?
    private var nextID: UInt16 = 0
    private let onChange: (CursorShape) -> Void

    init(_ onChange: @escaping (CursorShape) -> Void) { self.onChange = onChange }

    func setRunning(_ running: Bool) {
        if !running {
            timer?.cancel()
            timer = nil
        } else if timer == nil {
            let t = DispatchSource.makeTimerSource(queue: .main)
            t.schedule(deadline: .now(), repeating: .milliseconds(50), leeway: .milliseconds(5))
            t.setEventHandler { [weak self] in
                guard let self, let shape = self.sample() else { return }
                self.onChange(shape)
            }
            timer = t
            t.resume()
        }
    }

    /// Returns only a new valid shape. A missing or oversized cursor leaves the previous shape intact.
    func sample() -> CursorShape? {
        guard let cursor = NSCursor.currentSystem else { return nil }
        let image = cursor.image
        let size = image.size
        guard size.width.isFinite, size.height.isFinite,
              size.width > 0, size.height > 0, size.width <= 128, size.height <= 128 else { return nil }
        let w = Int(size.width.rounded(.up)), h = Int(size.height.rounded(.up))
        let hot = cursor.hotSpot
        guard hot.x.isFinite, hot.y.isFinite else { return nil }
        let hx = Int(min(max(hot.x.rounded(), 0), CGFloat(w - 1)))
        let hy = Int(min(max(hot.y.rounded(), 0), CGFloat(h - 1)))
        var rect = CGRect(origin: .zero, size: size)
        guard let cg = image.cgImage(forProposedRect: &rect, context: nil, hints: nil) else { return nil }
        var bytes = [UInt8](repeating: 0, count: w * h * 4)
        let rendered = bytes.withUnsafeMutableBytes { b -> Bool in
            guard let context = CGContext(data: b.baseAddress, width: w, height: h, bitsPerComponent: 8,
                                          bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(),
                                          bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue) else { return false }
            context.draw(cg, in: CGRect(x: 0, y: 0, width: size.width, height: size.height))
            return true
        }
        guard rendered else { return nil }
        // FNV-1a includes geometry, so a hotspot-only change also gets a new wire id.
        var hash: UInt64 = 0xcbf29ce484222325
        for byte in bytes { hash = (hash ^ UInt64(byte)) &* 0x100000001b3 }
        for value in [w, h, hx, hy] {
            withUnsafeBytes(of: UInt16(value).littleEndian) { b in
                for byte in b { hash = (hash ^ UInt64(byte)) &* 0x100000001b3 }
            }
        }
        guard hash != lastHash else { return nil }
        lastHash = hash
        nextID &+= 1
        let shape = CursorShape(id: nextID, w: w, h: h, hx: hx, hy: hy, bgra: Data(bytes))
        CursorFeed.shared.setShape(shape.id)
        return shape
    }

    deinit { timer?.cancel() }
}
