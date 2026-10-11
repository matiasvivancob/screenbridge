//! One viewer = one window on one monitor showing one Mac display. View-only: it never sends mouse or keyboard.
//! F11 fullscreen, F10 or right-click menu, Alt+F4 close. Each viewer runs its window and its network/render
//! loop on their own threads, so several can run in one process. Wire format: docs/PROTOCOL.md.

use crate::video;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F10, VK_F11};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const HDR: usize = 9;

/// Starting stream settings; the menu changes them live.
#[derive(Clone, Debug)]
pub struct Settings {
    pub mbps: u32,
    pub codec: crate::config::Codec,
    pub fps120: bool,
    pub speed: bool,
    pub low_latency_rc: bool,
}

pub struct Spec {
    pub id: usize,
    pub port: u16,
    /// Only datagrams from this address are accepted (the Mac). None accepts any source.
    pub peer: Option<IpAddr>,
    pub monitor: RECT,
    pub fullscreen: bool,
    pub title: String,
    pub settings: Settings,
}

/// Lets the owner close a viewer from another thread.
pub struct Handle {
    hwnd: isize,
}

impl Handle {
    pub fn close(&self) {
        unsafe { _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

/// Window-thread state, stored in GWLP_USERDATA. Shared references only: SetWindowPos and friends re-enter the
/// window procedure synchronously, so fields that change use Cell/RefCell and are never borrowed across a Win32 call.
struct State {
    fullscreen: Cell<bool>,
    saved: Cell<(isize, RECT)>,
    s: RefCell<Settings>,
    cmd: mpsc::Sender<Vec<u8>>,
    size: Arc<AtomicU64>, // client size w << 32 | h, read by the render thread
}

/// Opens a viewer and returns once its window exists. `closed` gets the spec id when the user closes it.
/// Fails if the UDP port is taken.
pub fn open(spec: Spec, closed: mpsc::Sender<usize>) -> std::io::Result<Handle> {
    static MEDIA_FOUNDATION: std::sync::Once = std::sync::Once::new();
    MEDIA_FOUNDATION.call_once(|| unsafe {
        use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
        _ = MFStartup(MF_VERSION, MFSTARTUP_FULL); // kept for the process lifetime; costs nothing until a viewer exists
    });
    let sock = UdpSocket::bind(("0.0.0.0", spec.port))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || window_thread(spec, sock, closed, tx));
    Ok(Handle { hwnd: rx.recv().map_err(|_| std::io::Error::other("viewer window failed"))? })
}

fn window_thread(spec: Spec, sock: UdpSocket, closed: mpsc::Sender<usize>, created: mpsc::Sender<isize>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let s = &spec.settings;
    for c in [
        format!("B{}", s.mbps),
        s.codec.wire().into(),
        if s.fps120 { "F120" } else { "F60" }.into(),
        if s.speed { "S1" } else { "S0" }.into(),
        if s.low_latency_rc { "L1" } else { "L0" }.into(),
    ] {
        let _ = cmd_tx.send(c.into_bytes());
    }
    let size = Arc::new(AtomicU64::new(0));
    let m = spec.monitor;
    let (mw, mh) = (m.right - m.left, m.bottom - m.top);
    unsafe {
        let inst = GetModuleHandleW(None).unwrap();
        let class = w!("ScreenBridgeViewer");
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: inst.into(),
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
                hIcon: LoadIconW(Some(inst.into()), PCWSTR(1 as _)).unwrap_or_default(),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
        // Created as a normal window (60% of the monitor, centered) so F11 has somewhere to return to.
        let (ww, wh) = (mw * 3 / 5, mh * 3 / 5);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &HSTRING::from(&spec.title),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            m.left + (mw - ww) / 2,
            m.top + (mh - wh) / 2,
            ww,
            wh,
            None,
            None,
            Some(inst.into()),
            None,
        )
        .unwrap();
        let state = Box::into_raw(Box::new(State {
            fullscreen: Cell::new(false),
            saved: Cell::new((0, RECT::default())),
            s: RefCell::new(spec.settings.clone()),
            cmd: cmd_tx,
            size: size.clone(),
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        let mut r = RECT::default();
        _ = GetClientRect(hwnd, &mut r);
        size.store((r.right as u64) << 32 | r.bottom as u64, Ordering::Relaxed);
        if spec.fullscreen {
            set_fullscreen(hwnd, &*state, true);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let h = hwnd.0 as isize;
        let render = {
            let (stop, size, title, peer_ip) = (stop.clone(), size.clone(), spec.title.clone(), spec.peer);
            std::thread::spawn(move || receive(sock, h, title, size, cmd_rx, stop, peer_ip))
        };
        let _ = created.send(h);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        stop.store(true, Ordering::Relaxed);
        let _ = render.join(); // releases the swapchain before the window state goes away
        drop(Box::from_raw(state));
    }
    let _ = closed.send(spec.id);
}

struct Frame {
    id: u32,
    flags: u8,
    chunks: Vec<Vec<u8>>, // empty = not received yet (payloads are never empty)
    got: usize,
    started: Instant,
}

fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// Network + decode + present loop for one viewer. No queues: each frame is shown as soon as it decodes.
/// Largest datagram we accept: our 9-byte header plus a chunk (the sender uses 1200). Bigger is not our protocol.
const MAX_DATAGRAM: usize = HDR + 1500;
/// Most chunks one frame may have (~9.8 MB): far above any real keyframe, far below memory exhaustion.
const MAX_CHUNKS: usize = 8192;
/// A frame that has not completed this long after its first chunk is abandoned.
const FRAME_DEADLINE: Duration = Duration::from_millis(100);
/// If handling one datagram takes longer than this, the socket buffer holds stale video: drop it and resync.
const STALL: Duration = Duration::from_millis(80);

fn receive(sock: UdpSocket, hwnd: isize, title: String, size: Arc<AtomicU64>, cmds: mpsc::Receiver<Vec<u8>>, stop: Arc<AtomicBool>, peer_ip: Option<IpAddr>) {
    let hwnd = HWND(hwnd as _);
    unsafe { _ = CoInitializeEx(None, COINIT_MULTITHREADED) };
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let size = || {
        let s = size.load(Ordering::Relaxed);
        ((s >> 32) as u32, s as u32)
    };
    let set_title = |s: &str| unsafe { _ = SetWindowTextW(hwnd, &HSTRING::from(format!("{title} — {s}   [F10 menu · F11 fullscreen]"))) };
    // The GPU can be busy or resetting (driver update, sleep); keep trying instead of leaving a dead window.
    let mut gpu = loop {
        match video::Gpu::new(hwnd, monitor, size()) {
            Ok(g) => break g,
            Err(e) => {
                set_title(&format!("graphics error: {e}. Retrying…"));
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let mut dec: Option<video::Decoder> = None;
    let mut last: Option<video::Picture> = None;
    let mut drawn = size();

    sock.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
    // Keyframe bursts are hundreds of packets back to back; the default 64 KB buffer drops them.
    unsafe {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::*;
        let n = (16i32 << 20).to_ne_bytes();
        setsockopt(SOCKET(sock.as_raw_socket() as usize), SOL_SOCKET, SO_RCVBUF, Some(&n));
    }
    let mut buf = [0u8; 65536];
    let mut cur: Option<Frame> = None;
    let mut last_done: Option<u32> = None;
    let mut need_key = true;
    let mut peer: Option<SocketAddr> = None;
    let mut settings: BTreeMap<u8, Vec<u8>> = BTreeMap::new(); // menu choices, replayed if the sender restarts
    let mut settings_sent: Option<Instant> = None; // settings ride on UDP with no ack, so they are repeated every 2 s
    let mut last_req = Instant::now() - Duration::from_secs(1);
    let (mut frames, mut bytes, mut dec_time, mut stat_t) = (0u32, 0usize, Duration::ZERO, Instant::now());
    let (mut present_time, mut lost) = (Duration::ZERO, 0u32);
    let mut status = String::from("waiting for the Mac…");
    let mut mac = String::new();
    set_title(&status);

    while !stop.load(Ordering::Relaxed) {
        let mut rebuild_gpu = false;
        let mut worked = Instant::now();
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if n > HDR && n <= MAX_DATAGRAM && peer_ip.is_none_or(|ip| ip == from.ip()) => {
                worked = Instant::now();
                if peer != Some(from) {
                    settings_sent = None; // new sender (or restarted): send our settings right away
                }
                peer = Some(from);
                bytes += n;
                let id = u32::from_be_bytes(buf[0..4].try_into().unwrap());
                let idx = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                let count = u16::from_be_bytes([buf[6], buf[7]]) as usize;
                if count == 0 {
                    mac = String::from_utf8_lossy(&buf[HDR..n]).into_owned(); // sender status text
                    continue;
                }
                if idx >= count || count > MAX_CHUNKS {
                    continue;
                }
                if let Some(l) = last_done {
                    if !newer(id, l) {
                        if l.wrapping_sub(id) > 64 {
                            // far in the past: the sender restarted, resync on its next keyframe
                            last_done = None;
                            cur = None;
                            need_key = true;
                        } else {
                            continue; // late packet of a frame we already finished or dropped
                        }
                    }
                }
                if cur.as_ref().is_some_and(|f| newer(f.id, id)) {
                    continue;
                }
                if cur.as_ref().is_none_or(|f| f.id != id) {
                    if cur.is_some() {
                        need_key = true; // previous frame never completed
                        lost += 1;
                    }
                    cur = Some(Frame { id, flags: buf[8], chunks: vec![Vec::new(); count], got: 0, started: Instant::now() });
                }
                let f = cur.as_mut().unwrap();
                if f.chunks.len() != count || !f.chunks[idx].is_empty() {
                    continue;
                }
                f.chunks[idx] = buf[HDR..n].to_vec();
                f.got += 1;
                if f.got == count {
                    let f = cur.take().unwrap();
                    if last_done.is_some_and(|l| f.id != l.wrapping_add(1)) {
                        need_key = true; // whole frames went missing
                        lost += 1;
                    }
                    last_done = Some(f.id);
                    let (key, codec) = (f.flags & 1 != 0, (f.flags >> 1) & 7);
                    if key {
                        need_key = false;
                        if dec.as_ref().is_none_or(|d| d.codec != codec) {
                            dec = match video::Decoder::new(&gpu.device, codec) {
                                Ok(d) => Some(d),
                                Err(e) => {
                                    status = format!("decoder error: {e}");
                                    None
                                }
                            };
                        }
                    }
                    if let (false, Some(d)) = (need_key, dec.as_mut()) {
                        let t = Instant::now();
                        match d.decode(&f.chunks.concat()) {
                            Ok(Some(pic)) => {
                                dec_time += t.elapsed();
                                drawn = size();
                                let t = Instant::now();
                                if gpu.present(&pic, drawn).is_err() {
                                    rebuild_gpu = true; // device removed/reset, or a resize failed
                                }
                                present_time += t.elapsed();
                                last = Some(pic);
                                frames += 1;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                need_key = true;
                                // Kept in the title (the "decoder error" prefix survives the once-a-second refresh).
                                status = format!("decoder error: {e}");
                            }
                        }
                    }
                }
            }
            Ok(_) => {} // wrong size or not from the Mac
            Err(_) => {} // timeout, or e.g. ICMP-induced WSAECONNRESET after the sender went away
        }
        // Checked on every pass, not only when idle: a stream of unrelated packets must not keep a stale frame alive.
        if cur.as_ref().is_some_and(|f| f.started.elapsed() > FRAME_DEADLINE) {
            cur = None;
            need_key = true;
            lost += 1;
        }
        // A stall (GPU hiccup, window drag) leaves seconds of old video in the socket buffer; decoding it all would
        // show the past. Throw it away and resync on the next keyframe.
        if worked.elapsed() > STALL {
            let _ = sock.set_nonblocking(true);
            while sock.recv_from(&mut buf).is_ok() {}
            let _ = sock.set_nonblocking(false);
            cur = None;
            need_key = true;
            lost += 1;
        }
        if rebuild_gpu {
            // The decoder holds the old device, so it goes too; the next keyframe brings everything back.
            match video::Gpu::new(hwnd, monitor, size()) {
                Ok(g) => {
                    gpu = g;
                    dec = None;
                    last = None;
                    need_key = true;
                }
                Err(e) => {
                    set_title(&format!("graphics error: {e}"));
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        // Window resized while the screen is static: redraw the last picture at the new size.
        if size() != drawn {
            drawn = size();
            if let Some(p) = &last {
                let _ = gpu.present(p, drawn);
            }
        }
        for c in cmds.try_iter() {
            if c == b"K" {
                need_key = true;
                last_req = Instant::now() - Duration::from_secs(1);
            } else {
                settings.insert(c[0], c);
                settings_sent = None;
            }
        }
        if let Some(p) = peer {
            if settings_sent.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                for s in settings.values() {
                    let _ = sock.send_to(s, p);
                }
                settings_sent = Some(Instant::now());
            }
            if need_key && last_req.elapsed() > Duration::from_millis(200) {
                let _ = sock.send_to(b"K", p);
                last_req = Instant::now();
            }
        }
        if stat_t.elapsed() >= Duration::from_secs(1) {
            let secs = stat_t.elapsed().as_secs_f64();
            if frames > 0 {
                let codec = dec.as_ref().map_or("H.264", |d| crate::config::Codec::from_id(d.codec).label());
                status = format!(
                    "{:.0} fps · {mac} · decode {:.1} ms · present {:.1} ms · {:.1} Mbps · {codec}{}",
                    frames as f64 / secs,
                    dec_time.as_secs_f64() * 1000.0 / frames as f64,
                    present_time.as_secs_f64() * 1000.0 / frames as f64,
                    bytes as f64 * 8.0 / secs / 1e6,
                    if lost > 0 { format!(" · {lost} lost") } else { String::new() }
                );
            } else if peer.is_some() && !status.starts_with("decoder error") {
                status = "idle (screen unchanged)".into();
            }
            if std::env::var_os("SB_VERBOSE").is_some() {
                println!("{title}: {status}");
            }
            set_title(&status);
            (frames, bytes, dec_time, present_time, lost, stat_t) = (0, 0, Duration::ZERO, Duration::ZERO, 0, Instant::now());
        }
    }
}

unsafe fn set_fullscreen(hwnd: HWND, st: &State, on: bool) {
    unsafe {
        // The flag is set first: the Win32 calls below re-enter wndproc and may read it.
        st.fullscreen.set(on);
        if on {
            let mut rect = RECT::default();
            _ = GetWindowRect(hwnd, &mut rect);
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            st.saved.set((style, rect));
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
            let m = mi.rcMonitor;
            SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !(WS_OVERLAPPEDWINDOW.0 as isize)) | WS_POPUP.0 as isize);
            _ = SetWindowPos(hwnd, Some(HWND_TOP), m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_FRAMECHANGED | SWP_SHOWWINDOW);
        } else {
            let (style, r) = st.saved.get();
            SetWindowLongPtrW(hwnd, GWL_STYLE, style);
            _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED | SWP_SHOWWINDOW);
        }
    }
}

/// Native popup menu at the cursor (or the window corner if the cursor is elsewhere).
unsafe fn show_menu(hwnd: HWND, st: &State) {
    let s = st.s.borrow().clone();
    let id = unsafe {
        let item = |m: HMENU, on: bool, id: usize, text: &str| {
            let flags = MF_STRING | if on { MF_CHECKED } else { MF_UNCHECKED };
            _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
        };
        let sub = |m: HMENU, s: HMENU, label: &str| _ = AppendMenuW(m, MF_POPUP, s.0 as usize, &HSTRING::from(label));
        let m = CreatePopupMenu().unwrap();
        let b = CreatePopupMenu().unwrap();
        for v in [5u32, 10, 15, 20, 30, 50, 80] {
            item(b, s.mbps == v, (100 + v) as usize, &format!("{v} Mbps"));
        }
        sub(m, b, "Bitrate");
        let c = CreatePopupMenu().unwrap();
        use crate::config::Codec;
        item(c, s.codec == Codec::H264, 201, "H.264 (hardware encode on the Mac)");
        item(c, s.codec == Codec::Hevc, 202, "HEVC (hardware encode on the Mac)");
        item(c, s.codec == Codec::Av1, 203, "AV1 (software encode: uses Mac CPU)");
        item(c, s.codec == Codec::Vp9, 204, "VP9 (software encode: uses Mac CPU)");
        sub(m, c, "Codec");
        let f = CreatePopupMenu().unwrap();
        item(f, !s.fps120, 301, "60 fps");
        item(f, s.fps120, 302, "120 fps");
        sub(m, f, "Frame rate");
        let e = CreatePopupMenu().unwrap();
        item(e, !s.speed, 401, "Quality");
        item(e, s.speed, 402, "Speed");
        sub(m, e, "Encoder priority");
        let r = CreatePopupMenu().unwrap();
        item(r, s.low_latency_rc, 501, "Low-latency (Apple)");
        item(r, !s.low_latency_rc, 502, "Standard");
        sub(m, r, "Rate control");
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(m, st.fullscreen.get(), 1, "Fullscreen\tF11");
        item(m, false, 2, "Refresh picture");
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(m, false, 3, "Close\tAlt+F4");

        let mut wr = RECT::default();
        _ = GetWindowRect(hwnd, &mut wr);
        let mut p = POINT::default();
        _ = GetCursorPos(&mut p);
        if p.x < wr.left || p.x >= wr.right || p.y < wr.top || p.y >= wr.bottom {
            p = POINT { x: wr.left + 40, y: wr.top + 40 };
        }
        _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None).0 as u32;
        _ = DestroyMenu(m); // also destroys the submenus
        id
    };
    let msg = match id {
        1 => {
            let on = !st.fullscreen.get();
            unsafe { set_fullscreen(hwnd, st, on) };
            None
        }
        2 => Some("K".to_string()),
        3 => {
            unsafe { _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            None
        }
        101..=199 => {
            st.s.borrow_mut().mbps = id - 100;
            Some(format!("B{}", id - 100))
        }
        201..=204 => {
            let codec = crate::config::Codec::from_id((id - 201) as u8);
            st.s.borrow_mut().codec = codec;
            Some(codec.wire().into())
        }
        301 | 302 => {
            st.s.borrow_mut().fps120 = id == 302;
            Some(if id == 302 { "F120" } else { "F60" }.into())
        }
        401 | 402 => {
            st.s.borrow_mut().speed = id == 402;
            Some(if id == 402 { "S1" } else { "S0" }.into())
        }
        501 | 502 => {
            st.s.borrow_mut().low_latency_rc = id == 501;
            Some(if id == 501 { "L1" } else { "L0" }.into())
        }
        _ => None,
    };
    if let Some(m) = msg {
        let _ = st.cmd.send(m.into_bytes());
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if st.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp); // messages sent during CreateWindowExW
        }
        let st = &*st;
        match msg {
            WM_SIZE => {
                let (w, h) = (lp.0 as u64 & 0xffff, (lp.0 as u64 >> 16) & 0xffff);
                st.size.store(w << 32 | h, Ordering::Relaxed);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_KEYDOWN if wp.0 == VK_F11.0 as usize => {
                let on = !st.fullscreen.get();
                set_fullscreen(hwnd, st, on);
                LRESULT(0)
            }
            // F10 arrives as a system key; handling it here also stops Windows from entering menu mode.
            WM_SYSKEYDOWN if wp.0 == VK_F10.0 as usize => {
                show_menu(hwnd, st);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                show_menu(hwnd, st);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0); // ends this viewer's own message loop only
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
