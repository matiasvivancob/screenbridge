// One virtual display: creates it, captures it, encodes it and streams it to the PC over UDP.
// Wire format: docs/PROTOCOL.md. Packet:
// [frame_id u32 BE][chunk_idx u16 BE][chunk_count u16 BE][flags u8: bit0=keyframe bit1=HEVC][Annex-B bytes]
// chunk_count 0 = typed side-channel packet (status text or cursor state).
// The viewer sends control messages back on the same socket (see Screen.handle).

import CoreMedia
import Foundation
import ScreenCaptureKit
import VideoToolbox

let HDR = 9
let CHUNK = 1200

func log(_ s: String) { NSLog("%@", s) }

/// UDP socket "connected" to one viewer port on the PC.
final class Link {
    private let fd: Int32
    private let lock = NSLock()
    private var closed = false // guarded by lock

    init?(host: String, port: UInt16) {
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        var size: Int32 = 4 << 20
        setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &size, socklen_t(MemoryLayout<Int32>.size))
        var one: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        // The reader wakes up every 200 ms to notice close(); it is the only place the descriptor is closed.
        var tv = timeval(tv_sec: 0, tv_usec: 200_000)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_port = port.bigEndian
        let fd = self.fd
        let ok = inet_pton(AF_INET, host, &a.sin_addr) == 1 && withUnsafePointer(to: &a) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        } == 0
        if !ok {
            Darwin.close(fd)
            return nil
        }
    }

    func send(_ p: [UInt8], nonblocking: Bool = false) {
        if nonblocking {
            // Cursor updates must not wait behind a video burst or retry a full socket buffer.
            guard lock.try() else { return }
            defer { lock.unlock() }
            if !closed { _ = Darwin.send(fd, p, p.count, MSG_DONTWAIT) }
            return
        }
        for _ in 0..<200 {
            lock.lock()
            if closed { lock.unlock(); return }
            let r = Darwin.send(fd, p, p.count, 0)
            let err = errno
            lock.unlock()
            if r >= 0 || err != ENOBUFS { return }
            usleep(50) // socket buffer full during a keyframe burst
        }
    }

    /// After this returns nothing sends on the socket again; the reader thread closes the descriptor.
    func close() {
        lock.lock()
        closed = true
        lock.unlock()
    }

    private var isClosed: Bool {
        lock.lock()
        defer { lock.unlock() }
        return closed
    }

    /// Control messages from the viewer (see Screen.handle).
    func onMessage(_ f: @escaping (String) -> Void) {
        let fd = self.fd
        Thread.detachNewThread {
            var b = [UInt8](repeating: 0, count: 64)
            while !self.isClosed {
                let n = recv(fd, &b, b.count, 0)
                if n > 0, let m = String(bytes: b[0..<n], encoding: .utf8) { f(m) }
                else if n < 0 && errno != EAGAIN { usleep(100_000) } // ECONNREFUSED while the viewer isn't running
            }
            Darwin.close(fd)
        }
    }
}

/// Wire ids (header flags bits 1-3): see docs/PROTOCOL.md.
enum VideoCodec: Int {
    case h264 = 0, hevc = 1, av1 = 2, vp9 = 3

    var label: String {
        switch self {
        case .h264: return "H.264"
        case .hevc: return "HEVC"
        case .av1: return "AV1"
        case .vp9: return "VP9"
        }
    }

    /// Encoded on the CPU with SVT-AV1 / libvpx instead of the media engine.
    var isSoftware: Bool { self == .av1 || self == .vp9 }
}

struct EncoderSettings: Equatable {
    var codec: VideoCodec = .h264
    var fps = 60
    var speed = false // PrioritizeEncodingSpeedOverQuality
    var mbps = 30
    var lowLatencyRC = true // Apple's low-latency rate control
}

final class Screen: NSObject, SCStreamOutput, SCStreamDelegate {
    let index: Int, w: Int, h: Int
    let display: CGVirtualDisplay
    let link: Link
    let q: DispatchQueue
    var stream: SCStream?
    let cfg = SCStreamConfiguration()
    /// Called on the main queue if capture fails or stops on its own.
    var onError: ((String) -> Void)?
    /// Called on the main queue when the viewer changes cursor overlay mode.
    var onCursorOverlayChange: (() -> Void)?
    private let stateLock = NSLock()
    private var stopped = false // guarded by stateLock: stop() cancels any startup still in flight
    private var cursorOverlay = false // guarded by stateLock
    private var encodeFailures = 0
    private var encoderReady = false
    // Only touched on q (or in init, before capture starts):
    var session: VTCompressionSession?
    var soft: OpaquePointer? // SBSoftEncoder*, used instead of `session` for AV1 / VP9
    let softQ = DispatchQueue(label: "softenc", qos: .userInteractive) // serial: conversion + encode
    var softBusy = false // on q
    var softPending: CVPixelBuffer? // newest frame that arrived while the encoder was busy (on q)
    var enc = EncoderSettings()
    var last: CVPixelBuffer?
    var forceKey = true
    var lastForced: UInt64 = 0
    var pts: CMTimeValue = 0
    // Only touched under sendLock:
    var frameID = UInt32.random(in: .min ... .max)
    var encodeMs: [Double] = []
    var statsSince = DispatchTime.now().uptimeNanoseconds
    let sendLock = NSLock()

    /// Must be called on the main thread (the display's dispatch queue).
    init(index: Int, w: Int, h: Int, link: Link, settings: EncoderSettings) {
        self.index = index
        self.w = w
        self.h = h
        self.link = link
        q = DispatchQueue(label: "screen\(index)")
        let d = CGVirtualDisplayDescriptor()
        d.queue = DispatchQueue.main
        d.name = "ScreenBridge \(index + 1)"
        d.maxPixelsWide = UInt32(w)
        d.maxPixelsHigh = UInt32(h)
        d.sizeInMillimeters = CGSize(width: Double(w) / 110 * 25.4, height: Double(h) / 110 * 25.4)
        // Sonoma+ needs a nonzero vendor and distinct serials; fixed values let macOS remember the arrangement.
        d.vendorID = 0x5342
        d.productID = 0x1000 + UInt32(index)
        d.serialNum = UInt32(index + 1)
        display = CGVirtualDisplay(descriptor: d)
        enc = settings
        super.init()
        setMode(fps: enc.fps)
        encoderReady = makeSession()
        link.onMessage { [weak self] m in self?.handle(m) }
    }

    func setMode(fps: Int) {
        let s = CGVirtualDisplaySettings()
        s.hiDPI = 0
        s.modes = [CGVirtualDisplayMode(width: UInt32(w), height: UInt32(h), refreshRate: CGFloat(fps))]
        if !display.apply(s) { log("display \(index + 1): \(fps) Hz mode rejected") }
    }

    /// (Re)creates the encoder from `enc`. False if it couldn't be set up.
    @discardableResult
    func makeSession() -> Bool {
        if let s = session { VTCompressionSessionInvalidate(s) }
        session = nil
        // Destroyed on its own queue so an encode in flight finishes first (softQ never waits on q).
        softQ.sync {
            if let e = self.soft { sb_soft_destroy(e) }
            self.soft = nil
        }
        softBusy = false
        softPending = nil
        if enc.codec.isSoftware {
            if sb_soft_available(Int32(enc.codec.rawValue)) == 0 {
                log("display \(index + 1): \(enc.codec.label) isn't available in this build, using H.264")
                enc.codec = .h264
            } else {
                var err = [CChar](repeating: 0, count: 160)
                soft = sb_soft_create(Int32(enc.codec.rawValue), Int32(w), Int32(h), Int32(enc.fps), Int32(enc.mbps * 1000), &err, err.count)
                guard soft != nil else {
                    log("display \(index + 1): \(enc.codec.label) encoder failed: \(String(cString: err))")
                    return false
                }
                forceKey = true
                pts = 0
                log("display \(index + 1): \(enc.codec.label) (software), \(enc.fps) fps, \(enc.mbps) Mbps")
                return true
            }
        }
        var spec: [String: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true]
        if enc.lowLatencyRC { spec[kVTVideoEncoderSpecification_EnableLowLatencyRateControl as String] = true }
        func create() -> OSStatus {
            VTCompressionSessionCreate(
                allocator: nil, width: Int32(w), height: Int32(h),
                codecType: enc.codec == .hevc ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264,
                encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil, compressedDataAllocator: nil,
                outputCallback: nil, refcon: nil, compressionSessionOut: &session)
        }
        var st = create()
        if st != noErr && enc.lowLatencyRC {
            log("display \(index + 1): low-latency rate control unavailable (\(st)), using normal rate control")
            spec.removeValue(forKey: kVTVideoEncoderSpecification_EnableLowLatencyRateControl as String)
            st = create()
        }
        guard st == noErr, let s = session else { log("display \(index + 1): encoder failed (\(st))"); return false }
        var props: [CFString: CFTypeRef] = [
            kVTCompressionPropertyKey_RealTime: kCFBooleanTrue,
            kVTCompressionPropertyKey_ProfileLevel: enc.codec == .hevc ? kVTProfileLevel_HEVC_Main_AutoLevel : kVTProfileLevel_H264_ConstrainedHigh_AutoLevel,
            kVTCompressionPropertyKey_AllowFrameReordering: kCFBooleanFalse,
            kVTCompressionPropertyKey_AverageBitRate: NSNumber(value: enc.mbps * 1_000_000),
            kVTCompressionPropertyKey_ExpectedFrameRate: NSNumber(value: enc.fps),
        ]
        if enc.speed { props[kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality] = kCFBooleanTrue }
        for (k, v) in props {
            let r = VTSessionSetProperty(s, key: k, value: v)
            if r != noErr { log("display \(index + 1): \(k) rejected (\(r))") }
        }
        let prep = VTCompressionSessionPrepareToEncodeFrames(s)
        if prep != noErr { log("display \(index + 1): encoder prepare failed (\(prep))") }
        forceKey = true
        pts = 0
        log("display \(index + 1): \(enc.codec.label), \(enc.fps) fps, \(enc.mbps) Mbps\(enc.speed ? ", speed priority" : "")\(enc.lowLatencyRC ? "" : ", standard rate control")")
        return prep == noErr
    }

    /// Control messages from the viewer: "K", "B<mbps>", "C264"/"C265"/"CAV1"/"CVP9", "F60"/"F120", "S0"/"S1", "L0"/"L1", "P0"/"P1".
    func handle(_ m: String) {
        if m == "K" { return requestKey() }
        q.async {
            var e = self.enc
            let v = Int(m.dropFirst())
            switch m.first {
            case "B": if let v, (1...200).contains(v) { e.mbps = v }
            case "C":
                switch m {
                case "C265": e.codec = .hevc
                case "CAV1": e.codec = .av1
                case "CVP9": e.codec = .vp9
                default: e.codec = .h264
                }
            case "F": if let v, v == 60 || v == 120 { e.fps = v }
            case "S": e.speed = m == "S1"
            case "L": e.lowLatencyRC = m == "L1"
            case "P":
                guard m == "P0" || m == "P1" else { return }
                let overlay = m == "P1"
                self.stateLock.lock()
                guard !self.stopped, self.cursorOverlay != overlay else { self.stateLock.unlock(); return }
                self.cursorOverlay = overlay
                let st = self.stream
                // Submit under the same lock as stop(), so a late P1 cannot re-register a stopped screen.
                if overlay { CursorFeed.shared.register(self) } else { CursorFeed.shared.unregister(self) }
                self.stateLock.unlock()
                self.cfg.showsCursor = !overlay
                st?.updateConfiguration(self.cfg) { err in if let err { log("cursor overlay change: \(err)") } }
                DispatchQueue.main.async { self.onCursorOverlayChange?() }
                return
            default: return
            }
            let old = self.enc
            guard e != old else { return }
            self.enc = e
            if e.fps != old.fps {
                DispatchQueue.main.async { self.setMode(fps: e.fps) }
                self.cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(e.fps))
                self.stream?.updateConfiguration(self.cfg) { err in if let err { log("frame rate change: \(err)") } }
            }
            let sameEncoder = (e.codec, e.fps, e.speed, e.lowLatencyRC) == (old.codec, old.fps, old.speed, old.lowLatencyRC)
            if sameEncoder, let s = self.session {
                VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: e.mbps * 1_000_000))
                log("display \(self.index + 1): bitrate \(e.mbps) Mbps")
            } else if sameEncoder, let sw = self.soft, sb_soft_set_bitrate(sw, Int32(e.mbps * 1000)) == 0 {
                log("display \(self.index + 1): bitrate \(e.mbps) Mbps")
            } else if self.makeSession() {
                if let pb = self.last { self.encode(pb) } // show the change even on a static screen
            } else {
                self.encoderReady = false
                DispatchQueue.main.async { self.onError?("The Mac's video encoder couldn't switch to those settings.") }
            }
        }
    }

    private var isStopped: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return stopped
    }

    var hasCursorOverlay: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return cursorOverlay && !stopped
    }

    func start() {
        guard encoderReady else {
            DispatchQueue.main.async { self.onError?("The Mac's hardware video encoder couldn't start for this display.") }
            return
        }
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
            if self.isStopped { return }
            if let err {
                DispatchQueue.main.async { self.onError?("Screen capture unavailable: \(err.localizedDescription)") }
                return
            }
            guard let scd = content?.displays.first(where: { $0.displayID == self.display.displayID }) else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { if !self.isStopped { self.start() } } // not visible yet
                return
            }
            // Configuration and back-channel changes share q, including P1 arriving before capture starts.
            self.q.async {
                if self.isStopped { return }
                let cfg = self.cfg
                cfg.width = self.w
                cfg.height = self.h
                cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(self.enc.fps))
                cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
                cfg.showsCursor = !self.hasCursorOverlay
                cfg.queueDepth = 6 // we keep one surface as `last`, the encoder holds a few
                let st = SCStream(filter: SCContentFilter(display: scd, excludingWindows: []), configuration: cfg, delegate: self)
                do {
                    try st.addStreamOutput(self, type: .screen, sampleHandlerQueue: self.q)
                } catch {
                    DispatchQueue.main.async { self.onError?("Capture setup failed: \(error.localizedDescription)") }
                    return
                }
                self.stateLock.lock()
                let cancelled = self.stopped
                if !cancelled { self.stream = st }
                self.stateLock.unlock()
                if cancelled { return }
                st.startCapture { e in
                    if let e { DispatchQueue.main.async { self.onError?("Capture failed: \(e.localizedDescription)") } }
                    if self.isStopped { st.stopCapture { _ in } } // stop() raced with startup
                }
                log("display \(self.index + 1) (id \(self.display.displayID)): \(self.w)x\(self.h) streaming")
            }
        }
    }

    /// Stops streaming. The virtual display disappears when this object is released.
    func stop() {
        stateLock.lock()
        stopped = true
        cursorOverlay = false
        CursorFeed.shared.unregister(self)
        let st = stream
        stream = nil
        stateLock.unlock()
        st?.stopCapture { _ in }
        link.close()
        // Asynchronous on purpose: the encoder queue may be waiting on the main queue, and this runs on main.
        q.async {
            if let s = self.session { VTCompressionSessionInvalidate(s) }
            self.session = nil
            self.last = nil
            self.softPending = nil
            self.softQ.async { // after any encode in flight
                if let e = self.soft { sb_soft_destroy(e) }
                self.soft = nil
            }
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sb.isValid,
              let info = (CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]])?.first,
              let raw = info[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let pb = sb.imageBuffer else { return }
        last = pb
        encode(pb)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        DispatchQueue.main.async { self.onError?("Capture stopped: \(error.localizedDescription)") }
    }

    func encode(_ pb: CVPixelBuffer) {
        if enc.codec.isSoftware { return encodeSoft(pb) }
        guard let s = session else { return }
        var props: CFDictionary?
        if forceKey {
            props = [kVTEncodeFrameOptionKey_ForceKeyFrame as String: true] as CFDictionary
            forceKey = false
        }
        let codec = enc.codec // frames still in flight after a codec switch keep their own label
        let t0 = DispatchTime.now().uptimeNanoseconds
        // Evenly spaced timestamps, not capture times: measured on M1 Pro, wall-clock PTS makes the
        // low-latency encoder hold most frames ~12 ms (median 11.6 ms vs 1.7 ms).
        pts += 1
        let r = VTCompressionSessionEncodeFrame(
            s, imageBuffer: pb, presentationTimeStamp: CMTime(value: pts, timescale: CMTimeScale(enc.fps)),
            duration: .invalid, frameProperties: props, infoFlagsOut: nil
        ) { [weak self] status, _, sb in
            guard status == noErr, let sb, let self else { return }
            self.shipVT(sb, codec: codec, encodeNs: DispatchTime.now().uptimeNanoseconds - t0)
        }
        // A session that keeps refusing frames is a dead display: say so instead of streaming nothing forever.
        if r == noErr {
            encodeFailures = 0
        } else {
            encodeFailures += 1
            if encodeFailures == 60 {
                DispatchQueue.main.async { self.onError?("The Mac's video encoder stopped accepting frames (error \(r)).") }
            }
        }
    }

    func requestKey() {
        q.async {
            let now = DispatchTime.now().uptimeNanoseconds
            guard now - self.lastForced > 100_000_000 else { return }
            self.lastForced = now
            self.forceKey = true
            // A static screen produces no new frames, so re-encode the last one to carry the keyframe.
            if let pb = self.last { self.encode(pb) }
        }
    }

    /// Software encoders (AV1, VP9): convert and encode on `softQ`, keeping only the newest frame while busy.
    func encodeSoft(_ pb: CVPixelBuffer) {
        if softBusy {
            softPending = pb
            return
        }
        guard let e = soft else { return }
        softBusy = true
        let force = forceKey
        forceKey = false
        let codec = enc.codec
        softQ.async {
            let t0 = DispatchTime.now().uptimeNanoseconds
            CVPixelBufferLockBaseAddress(pb, .readOnly)
            var out = [UInt8](repeating: 0, count: 4 << 20) // a keyframe at the highest bitrates is well under this
            var isKey: Int32 = 0
            var n: Int32 = -1
            if let y = CVPixelBufferGetBaseAddressOfPlane(pb, 0), let uv = CVPixelBufferGetBaseAddressOfPlane(pb, 1) {
                n = sb_soft_encode(
                    e, y.assumingMemoryBound(to: UInt8.self), Int32(CVPixelBufferGetBytesPerRowOfPlane(pb, 0)),
                    uv.assumingMemoryBound(to: UInt8.self), Int32(CVPixelBufferGetBytesPerRowOfPlane(pb, 1)),
                    force ? 1 : 0, &out, out.count, &isKey)
            }
            CVPixelBufferUnlockBaseAddress(pb, .readOnly)
            if n > 0 {
                self.encodeFailures = 0
                self.sendFrame(Array(out[0..<Int(n)]), key: isKey != 0, codec: codec, encodeNs: DispatchTime.now().uptimeNanoseconds - t0)
            } else if n < 0 {
                self.q.async {
                    self.encodeFailures += 1
                    if self.encodeFailures == 30 {
                        DispatchQueue.main.async { self.onError?("The \(codec.label) encoder stopped working (error \(n)).") }
                    }
                }
            }
            self.q.async {
                self.softBusy = false
                if let next = self.softPending {
                    self.softPending = nil
                    self.encodeSoft(next)
                }
            }
        }
    }

    /// Hardware (VideoToolbox) output -> Annex-B bytes -> sendFrame.
    func shipVT(_ sb: CMSampleBuffer, codec: VideoCodec, encodeNs: UInt64) {
        let att = (CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[String: Any]])?.first
        let key = !((att?[kCMSampleAttachmentKey_NotSync as String] as? Bool) ?? false)
        let sc: [UInt8] = [0, 0, 0, 1]
        var out: [UInt8] = []
        if key, let fmt = sb.formatDescription {
            let parameterSet = codec == .hevc ? CMVideoFormatDescriptionGetHEVCParameterSetAtIndex : CMVideoFormatDescriptionGetH264ParameterSetAtIndex
            var n = 0
            _ = parameterSet(fmt, 0, nil, nil, &n, nil)
            for i in 0..<n {
                var p: UnsafePointer<UInt8>?
                var len = 0
                _ = parameterSet(fmt, i, &p, &len, nil, nil)
                if let p { out += sc; out += UnsafeBufferPointer(start: p, count: len) }
            }
        }
        guard let bb = sb.dataBuffer else { return }
        var data = [UInt8](repeating: 0, count: CMBlockBufferGetDataLength(bb))
        _ = CMBlockBufferCopyDataBytes(bb, atOffset: 0, dataLength: data.count, destination: &data)
        var i = 0
        while i + 4 <= data.count { // 4-byte lengths -> Annex-B start codes
            let len = Int(data[i]) << 24 | Int(data[i + 1]) << 16 | Int(data[i + 2]) << 8 | Int(data[i + 3])
            i += 4
            guard i + len <= data.count else { break }
            out += sc
            out += data[i..<i + len]
            i += len
        }
        sendFrame(out, key: key, codec: codec, encodeNs: encodeNs)
    }

    /// Packetizes one encoded frame (any codec) and sends it; reports encode time once a second.
    func sendFrame(_ out: [UInt8], key: Bool, codec: VideoCodec, encodeNs: UInt64) {
        sendLock.lock()
        defer { sendLock.unlock() }
        let count = (out.count + CHUNK - 1) / CHUNK
        guard count > 0, count <= 65535 else { return }
        for c in 0..<count {
            var p = [UInt8]()
            p.reserveCapacity(HDR + CHUNK)
            withUnsafeBytes(of: frameID.bigEndian) { p.append(contentsOf: $0) }
            withUnsafeBytes(of: UInt16(c).bigEndian) { p.append(contentsOf: $0) }
            withUnsafeBytes(of: UInt16(count).bigEndian) { p.append(contentsOf: $0) }
            p.append(UInt8((key ? 1 : 0) | (codec.rawValue << 1)))
            p += out[c * CHUNK..<min(out.count, (c + 1) * CHUNK)]
            link.send(p)
        }
        frameID &+= 1

        // Once a second, tell the viewer how long encoding takes (side-channel kind 0x01).
        encodeMs.append(Double(encodeNs) / 1e6)
        let now = DispatchTime.now().uptimeNanoseconds
        if now - statsSince >= 1_000_000_000 {
            encodeMs.sort()
            let text = String(format: "Mac encode %.1f ms (max %.1f)", encodeMs[encodeMs.count / 2], encodeMs.last!)
            link.send([UInt8](repeating: 0, count: HDR) + [0x01] + Array(text.utf8))
            encodeMs.removeAll(keepingCapacity: true)
            statsSince = now
        }
    }
}
