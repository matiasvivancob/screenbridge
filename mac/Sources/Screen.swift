// One virtual display: creates it, captures it, encodes it and streams it to the PC over UDP.
// Wire format: docs/PROTOCOL.md. Packet:
// [frame_id u32 BE][chunk_idx u16 BE][chunk_count u16 BE][flags u8: bit0=keyframe bit1=HEVC][Annex-B bytes]
// chunk_count 0 = UTF-8 status text for the viewer (encode time).
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
    let fd: Int32
    private var closed = false

    init?(host: String, port: UInt16) {
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        var size: Int32 = 4 << 20
        setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &size, socklen_t(MemoryLayout<Int32>.size))
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

    func send(_ p: [UInt8]) {
        for _ in 0..<200 {
            if Darwin.send(fd, p, p.count, 0) >= 0 || errno != ENOBUFS { return }
            usleep(50) // socket buffer full during a keyframe burst
        }
    }

    func close() {
        closed = true
        shutdown(fd, SHUT_RDWR)
        Darwin.close(fd) // wakes the receive thread, which then exits
    }

    /// Control messages from the viewer (see Screen.handle).
    func onMessage(_ f: @escaping (String) -> Void) {
        let fd = self.fd
        Thread.detachNewThread {
            var b = [UInt8](repeating: 0, count: 64)
            while !self.closed {
                let n = recv(fd, &b, b.count, 0)
                if n > 0, let m = String(bytes: b[0..<n], encoding: .utf8) { f(m) }
                else if n < 0 { usleep(100_000) } // ECONNREFUSED while the viewer isn't running
            }
        }
    }
}

struct EncoderSettings: Equatable {
    var hevc = false
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
    // Only touched on q (or in init, before capture starts):
    var session: VTCompressionSession?
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
        makeSession()
        link.onMessage { [weak self] m in self?.handle(m) }
    }

    func setMode(fps: Int) {
        let s = CGVirtualDisplaySettings()
        s.hiDPI = 0
        s.modes = [CGVirtualDisplayMode(width: UInt32(w), height: UInt32(h), refreshRate: CGFloat(fps))]
        if !display.apply(s) { log("display \(index + 1): \(fps) Hz mode rejected") }
    }

    /// (Re)creates the encoder from `enc`.
    func makeSession() {
        if let s = session { VTCompressionSessionInvalidate(s) }
        session = nil
        var spec: [String: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true]
        if enc.lowLatencyRC { spec[kVTVideoEncoderSpecification_EnableLowLatencyRateControl as String] = true }
        func create() -> OSStatus {
            VTCompressionSessionCreate(
                allocator: nil, width: Int32(w), height: Int32(h),
                codecType: enc.hevc ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264,
                encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil, compressedDataAllocator: nil,
                outputCallback: nil, refcon: nil, compressionSessionOut: &session)
        }
        var st = create()
        if st != noErr && enc.lowLatencyRC {
            log("display \(index + 1): low-latency rate control unavailable (\(st)), using normal rate control")
            spec.removeValue(forKey: kVTVideoEncoderSpecification_EnableLowLatencyRateControl as String)
            st = create()
        }
        guard st == noErr, let s = session else { log("display \(index + 1): encoder failed (\(st))"); return }
        var props: [CFString: CFTypeRef] = [
            kVTCompressionPropertyKey_RealTime: kCFBooleanTrue,
            kVTCompressionPropertyKey_ProfileLevel: enc.hevc ? kVTProfileLevel_HEVC_Main_AutoLevel : kVTProfileLevel_H264_ConstrainedHigh_AutoLevel,
            kVTCompressionPropertyKey_AllowFrameReordering: kCFBooleanFalse,
            kVTCompressionPropertyKey_AverageBitRate: NSNumber(value: enc.mbps * 1_000_000),
            kVTCompressionPropertyKey_ExpectedFrameRate: NSNumber(value: enc.fps),
        ]
        if enc.speed { props[kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality] = kCFBooleanTrue }
        for (k, v) in props {
            let r = VTSessionSetProperty(s, key: k, value: v)
            if r != noErr { log("display \(index + 1): \(k) rejected (\(r))") }
        }
        VTCompressionSessionPrepareToEncodeFrames(s)
        forceKey = true
        pts = 0
        log("display \(index + 1): \(enc.hevc ? "HEVC" : "H.264"), \(enc.fps) fps, \(enc.mbps) Mbps\(enc.speed ? ", speed priority" : "")\(enc.lowLatencyRC ? "" : ", standard rate control")")
    }

    /// Control messages from the viewer: "K", "B<mbps>", "C264"/"C265", "F60"/"F120", "S0"/"S1", "L0"/"L1".
    func handle(_ m: String) {
        if m == "K" { return requestKey() }
        q.async {
            var e = self.enc
            let v = Int(m.dropFirst())
            switch m.first {
            case "B": if let v, (1...200).contains(v) { e.mbps = v }
            case "C": e.hevc = m == "C265"
            case "F": if let v, v == 60 || v == 120 { e.fps = v }
            case "S": e.speed = m == "S1"
            case "L": e.lowLatencyRC = m == "L1"
            default: return
            }
            let old = self.enc
            guard e != old else { return }
            self.enc = e
            if e.fps != old.fps {
                DispatchQueue.main.sync { self.setMode(fps: e.fps) }
                self.cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(e.fps))
                self.stream?.updateConfiguration(self.cfg) { err in if let err { log("frame rate change: \(err)") } }
            }
            if (e.hevc, e.fps, e.speed, e.lowLatencyRC) == (old.hevc, old.fps, old.speed, old.lowLatencyRC), let s = self.session {
                VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: e.mbps * 1_000_000))
                log("display \(self.index + 1): bitrate \(e.mbps) Mbps")
            } else {
                self.makeSession()
                if let pb = self.last { self.encode(pb) } // show the change even on a static screen
            }
        }
    }

    func start() {
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, err in
            if let err {
                DispatchQueue.main.async { self.onError?("Screen capture unavailable: \(err.localizedDescription)") }
                return
            }
            guard let scd = content?.displays.first(where: { $0.displayID == self.display.displayID }) else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { self.start() } // display not visible yet
                return
            }
            let cfg = self.cfg
            cfg.width = self.w
            cfg.height = self.h
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(self.enc.fps))
            cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            cfg.showsCursor = true
            cfg.queueDepth = 6 // we keep one surface as `last`, the encoder holds a few
            let st = SCStream(filter: SCContentFilter(display: scd, excludingWindows: []), configuration: cfg, delegate: self)
            do {
                try st.addStreamOutput(self, type: .screen, sampleHandlerQueue: self.q)
            } catch {
                DispatchQueue.main.async { self.onError?("Capture setup failed: \(error.localizedDescription)") }
                return
            }
            st.startCapture { e in
                if let e { DispatchQueue.main.async { self.onError?("Capture failed: \(e.localizedDescription)") } }
            }
            self.stream = st
            log("display \(self.index + 1) (id \(self.display.displayID)): \(self.w)x\(self.h) streaming")
        }
    }

    /// Stops streaming. The virtual display disappears when this object is released.
    func stop() {
        stream?.stopCapture { _ in }
        stream = nil
        link.close()
        q.sync {
            if let s = session { VTCompressionSessionInvalidate(s) }
            session = nil
            last = nil
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
        guard let s = session else { return }
        var props: CFDictionary?
        if forceKey {
            props = [kVTEncodeFrameOptionKey_ForceKeyFrame as String: true] as CFDictionary
            forceKey = false
        }
        let hevc = enc.hevc // frames still in flight after a codec switch keep their own label
        let t0 = DispatchTime.now().uptimeNanoseconds
        // Evenly spaced timestamps, not capture times: measured on M1 Pro, wall-clock PTS makes the
        // low-latency encoder hold most frames ~12 ms (median 11.6 ms vs 1.7 ms).
        pts += 1
        _ = VTCompressionSessionEncodeFrame(
            s, imageBuffer: pb, presentationTimeStamp: CMTime(value: pts, timescale: CMTimeScale(enc.fps)),
            duration: .invalid, frameProperties: props, infoFlagsOut: nil
        ) { [weak self] status, _, sb in
            guard status == noErr, let sb, let self else { return }
            self.ship(sb, hevc: hevc, encodeNs: DispatchTime.now().uptimeNanoseconds - t0)
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

    func ship(_ sb: CMSampleBuffer, hevc: Bool, encodeNs: UInt64) {
        let att = (CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[String: Any]])?.first
        let key = !((att?[kCMSampleAttachmentKey_NotSync as String] as? Bool) ?? false)
        let sc: [UInt8] = [0, 0, 0, 1]
        var out: [UInt8] = []
        if key, let fmt = sb.formatDescription {
            let parameterSet = hevc ? CMVideoFormatDescriptionGetHEVCParameterSetAtIndex : CMVideoFormatDescriptionGetH264ParameterSetAtIndex
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
            p.append((key ? 1 : 0) | (hevc ? 2 : 0))
            p += out[c * CHUNK..<min(out.count, (c + 1) * CHUNK)]
            link.send(p)
        }
        frameID &+= 1

        // Once a second, tell the viewer how long encoding takes (chunk_count 0 = text message).
        encodeMs.append(Double(encodeNs) / 1e6)
        let now = DispatchTime.now().uptimeNanoseconds
        if now - statsSince >= 1_000_000_000 {
            encodeMs.sort()
            let text = String(format: "Mac encode %.1f ms (max %.1f)", encodeMs[encodeMs.count / 2], encodeMs.last!)
            link.send([UInt8](repeating: 0, count: HDR) + Array(text.utf8))
            encodeMs.removeAll(keepingCapacity: true)
            statsSince = now
        }
    }
}
