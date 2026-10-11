// Run on a Mac inside the GUI session (Terminal.app): samples the real system cursor with the app's own
// CursorShapeWatcher and writes /tmp/sb_cursor.ppm (alpha-composited on white) and /tmp/sb_cursor.txt.
//
//   cd mac && swiftc -O -import-objc-header Sources/Bridge.h Sources/Screen.swift Sources/Agent.swift \
//       Sources/Cursor.swift ../tools/cursor_probe.swift -o /tmp/cursor_probe && /tmp/cursor_probe
import AppKit
import CoreGraphics

_ = NSApplication.shared
var report = ""
let loc = CGEvent(source: nil)?.location
report += "pointer location: \(loc.map { "\($0.x),\($0.y)" } ?? "unavailable")\n"
if let h = dlopen(nil, RTLD_LAZY), let p = dlsym(h, "CGCursorIsVisible") {
    let f = unsafeBitCast(p, to: (@convention(c) () -> boolean_t).self)
    report += "CGCursorIsVisible: symbol present, returns \(f())\n"
} else {
    report += "CGCursorIsVisible: symbol NOT present (the pointer is always treated as visible)\n"
}
if let c = NSCursor.currentSystem {
    report += "NSCursor.currentSystem: image \(c.image.size) hotSpot \(c.hotSpot) reps \(c.image.representations.count)\n"
} else {
    report += "NSCursor.currentSystem: nil\n"
}

let watcher = CursorShapeWatcher { _ in }
if let s = watcher.sample() {
    var opaque = 0, partial = 0
    for i in stride(from: 3, to: s.bgra.count, by: 4) {
        let a = s.bgra[s.bgra.startIndex + i]
        if a == 255 { opaque += 1 } else if a > 0 { partial += 1 }
    }
    // Premultiplied sanity: no colour channel may exceed alpha.
    var invalid = 0
    for i in stride(from: 0, to: s.bgra.count, by: 4) {
        let a = s.bgra[s.bgra.startIndex + i + 3]
        for k in 0..<3 where s.bgra[s.bgra.startIndex + i + k] > a { invalid += 1 }
    }
    report += "shape id \(s.id): \(s.w)x\(s.h) hotspot (\(s.hx),\(s.hy)) opaque \(opaque) partial \(partial) premultiplied-violations \(invalid)\n"
    var ppm = Data("P6\n\(s.w) \(s.h)\n255\n".utf8)
    for p in 0..<(s.w * s.h) {
        let o = s.bgra.startIndex + p * 4
        let a = Int(s.bgra[o + 3])
        for k in [2, 1, 0] { // BGRA -> RGB over white, premultiplied
            ppm.append(UInt8(min(255, Int(s.bgra[o + k]) + (255 - a))))
        }
    }
    try? ppm.write(to: URL(fileURLWithPath: "/tmp/sb_cursor.ppm"))
    // Mark the hotspot row/col so orientation is unambiguous in the dump.
    report += "top-left 8x8 alpha:\n"
    for y in 0..<min(8, s.h) {
        var line = ""
        for x in 0..<min(8, s.w) { line += String(format: "%3d ", s.bgra[s.bgra.startIndex + (y * s.w + x) * 4 + 3]) }
        report += line + "\n"
    }
} else {
    report += "sample() returned nil\n"
}
try? report.write(toFile: "/tmp/sb_cursor.txt", atomically: true, encoding: .utf8)
print(report)
