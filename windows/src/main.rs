//! View-only receiver: one process = one UDP port = one window, starting fullscreen on one monitor.
//! Usage: viewer <port> <monitor#>     (run with no args to list monitors)
//! F11 fullscreen, F10 or right-click menu, Alt+F4 close. Never sends mouse/keyboard to the Mac.
//!
//! Packet: [frame_id u32 BE][chunk_idx u16 BE][chunk_count u16 BE][flags u8: bit0=keyframe bit1=HEVC][Annex-B bytes]
//! Back channel to the sender: "K" keyframe, "B<mbps>", "C264"/"C265", "F60"/"F120", "S0"/"S1" (encoder speed priority),
//! "L0"/"L1" (Apple low-latency rate control). Packets with chunk_count 0 carry sender status text.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window; stats are in the title bar

mod video;

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F10, VK_F11};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const HDR: usize = 9;

struct Frame {
    id: u32,
    flags: u8,
    chunks: Vec<Vec<u8>>, // empty = not received yet (payloads are never empty)
    got: usize,
    started: Instant,
}

/// Menu state; these defaults match the sender's.
struct Ui {
    fullscreen: bool,
    saved: (isize, RECT),
    mbps: u32,
    hevc: bool,
    fps120: bool,
    speed: bool,
    low_latency_rc: bool,
    cmd: mpsc::Sender<Vec<u8>>,
}

static UI: Mutex<Option<Ui>> = Mutex::new(None);
static SIZE: AtomicU64 = AtomicU64::new(0); // client size, w << 32 | h

fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

fn monitors() -> Vec<(HMONITOR, RECT, bool)> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool() {
            let v = unsafe { &mut *(out.0 as *mut Vec<(HMONITOR, RECT, bool)>) };
            v.push((m, mi.rcMonitor, mi.dwFlags & 1 != 0));
        }
        true.into()
    }
    let mut v = Vec::new();
    unsafe { _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize)) };
    v
}

fn receive(port: u16, hwnd: isize, cmds: mpsc::Receiver<Vec<u8>>) {
    let hwnd = HWND(hwnd as _);
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let size = || {
        let s = SIZE.load(Ordering::Relaxed);
        ((s >> 32) as u32, s as u32)
    };
    let mut gpu = video::Gpu::new(hwnd, monitor, size()).expect("D3D11 init");
    let mut dec: Option<video::Decoder> = None;
    let mut last: Option<video::Picture> = None;
    let mut drawn = size();

    let sock = UdpSocket::bind(("0.0.0.0", port)).expect("bind UDP port");
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
    let mut settings_sent_to: Option<SocketAddr> = None;
    let mut last_req = Instant::now() - Duration::from_secs(1);
    let (mut frames, mut bytes, mut dec_time, mut stat_t) = (0u32, 0usize, Duration::ZERO, Instant::now());
    let (mut present_time, mut lost) = (Duration::ZERO, 0u32);
    let mut status = String::from("waiting for the Mac…");
    let start = Instant::now();
    let mut mac = String::new();
    println!("listening on UDP {port}");

    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if n > HDR => {
                peer = Some(from);
                bytes += n;
                let id = u32::from_be_bytes(buf[0..4].try_into().unwrap());
                let idx = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                let count = u16::from_be_bytes([buf[6], buf[7]]) as usize;
                if count == 0 {
                    mac = String::from_utf8_lossy(&buf[HDR..n]).into_owned(); // sender status text
                    continue;
                }
                if idx >= count {
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
                    let (key, hevc) = (f.flags & 1 != 0, f.flags & 2 != 0);
                    if key {
                        need_key = false;
                        if dec.as_ref().is_none_or(|d| d.hevc != hevc) {
                            dec = match video::Decoder::new(&gpu.device, hevc) {
                                Ok(d) => Some(d),
                                Err(e) => {
                                    status = format!("decoder error: {e}");
                                    eprintln!("{status}");
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
                                if let Err(e) = gpu.present(&pic, drawn) {
                                    eprintln!("present: {e}");
                                }
                                present_time += t.elapsed();
                                last = Some(pic);
                                frames += 1;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                eprintln!("decode error: {e}");
                                need_key = true;
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                // The last packet of a frame was lost and nothing newer is coming (static screen).
                if cur.as_ref().is_some_and(|f| f.started.elapsed() > Duration::from_millis(60)) {
                    cur = None;
                    need_key = true;
                }
            }
            Err(_) => {} // e.g. ICMP-induced WSAECONNRESET after the sender went away
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
                settings_sent_to = None;
            }
        }
        if let Some(p) = peer {
            if settings_sent_to != Some(p) {
                for s in settings.values() {
                    let _ = sock.send_to(s, p);
                }
                settings_sent_to = Some(p);
            }
            if need_key && last_req.elapsed() > Duration::from_millis(200) {
                let _ = sock.send_to(b"K", p);
                last_req = Instant::now();
            }
        }
        if stat_t.elapsed() >= Duration::from_secs(1) {
            let secs = stat_t.elapsed().as_secs_f64();
            if frames > 0 {
                let codec = if dec.as_ref().is_some_and(|d| d.hevc) { "HEVC" } else { "H.264" };
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
            if std::env::var_os("SB_VERBOSE").is_some() || stat_t.duration_since(start).as_secs() % 5 == 0 {
                println!("{status}");
            }
            unsafe { _ = SetWindowTextW(hwnd, &HSTRING::from(format!("screenbridge :{port} — {status}   [F10 menu · F11 fullscreen]"))) };
            (frames, bytes, dec_time, present_time, lost, stat_t) = (0, 0, Duration::ZERO, Duration::ZERO, 0, Instant::now());
        }
    }
}

unsafe fn set_fullscreen(hwnd: HWND, ui: &mut Ui, on: bool) {
    unsafe {
        if on {
            _ = GetWindowRect(hwnd, &mut ui.saved.1);
            ui.saved.0 = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
            let m = mi.rcMonitor;
            SetWindowLongPtrW(hwnd, GWL_STYLE, (ui.saved.0 & !(WS_OVERLAPPEDWINDOW.0 as isize)) | WS_POPUP.0 as isize);
            _ = SetWindowPos(hwnd, Some(HWND_TOP), m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_FRAMECHANGED | SWP_SHOWWINDOW);
        } else {
            SetWindowLongPtrW(hwnd, GWL_STYLE, ui.saved.0);
            let r = ui.saved.1;
            _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED | SWP_SHOWWINDOW);
        }
        ui.fullscreen = on;
    }
}

/// Native popup menu at the cursor (or the window corner if the cursor is elsewhere).
unsafe fn show_menu(hwnd: HWND) {
    let (fs, mbps, hevc, fps120, speed, llrc) = {
        let g = UI.lock().unwrap();
        let u = g.as_ref().unwrap();
        (u.fullscreen, u.mbps, u.hevc, u.fps120, u.speed, u.low_latency_rc)
    };
    let id = unsafe {
        let item = |m: HMENU, on: bool, id: usize, s: &str| {
            let flags = MF_STRING | if on { MF_CHECKED } else { MF_UNCHECKED };
            _ = AppendMenuW(m, flags, id, &HSTRING::from(s));
        };
        let sub = |m: HMENU, s: HMENU, label: &str| _ = AppendMenuW(m, MF_POPUP, s.0 as usize, &HSTRING::from(label));
        let m = CreatePopupMenu().unwrap();
        let b = CreatePopupMenu().unwrap();
        for v in [5u32, 10, 15, 20, 30, 50, 80] {
            item(b, mbps == v, (100 + v) as usize, &format!("{v} Mbps"));
        }
        sub(m, b, "Bitrate");
        let c = CreatePopupMenu().unwrap();
        item(c, !hevc, 201, "H.264");
        item(c, hevc, 202, "HEVC (H.265)");
        sub(m, c, "Codec");
        let f = CreatePopupMenu().unwrap();
        item(f, !fps120, 301, "60 fps");
        item(f, fps120, 302, "120 fps (lower latency, more GPU)");
        sub(m, f, "Frame rate");
        let e = CreatePopupMenu().unwrap();
        item(e, !speed, 401, "Quality");
        item(e, speed, 402, "Speed (lower latency)");
        sub(m, e, "Encoder priority");
        let r = CreatePopupMenu().unwrap();
        item(r, llrc, 501, "Low-latency (Apple)");
        item(r, !llrc, 502, "Standard (often faster on M1)");
        sub(m, r, "Rate control");
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(m, fs, 1, "Fullscreen\tF11");
        item(m, false, 2, "Refresh picture");
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(m, false, 3, "Close viewer\tAlt+F4");

        let mut r = RECT::default();
        _ = GetWindowRect(hwnd, &mut r);
        let mut p = POINT::default();
        _ = GetCursorPos(&mut p);
        if p.x < r.left || p.x >= r.right || p.y < r.top || p.y >= r.bottom {
            p = POINT { x: r.left + 40, y: r.top + 40 };
        }
        _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None).0 as u32;
        _ = DestroyMenu(m); // also destroys the submenus
        id
    };
    let mut g = UI.lock().unwrap();
    let u = g.as_mut().unwrap();
    let msg = match id {
        1 => {
            let on = !u.fullscreen;
            unsafe { set_fullscreen(hwnd, u, on) };
            None
        }
        2 => Some("K".to_string()),
        3 => {
            unsafe { _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            None
        }
        101..=199 => {
            u.mbps = id - 100;
            Some(format!("B{}", u.mbps))
        }
        201 | 202 => {
            u.hevc = id == 202;
            Some(if u.hevc { "C265" } else { "C264" }.into())
        }
        301 | 302 => {
            u.fps120 = id == 302;
            Some(if u.fps120 { "F120" } else { "F60" }.into())
        }
        401 | 402 => {
            u.speed = id == 402;
            Some(if u.speed { "S1" } else { "S0" }.into())
        }
        501 | 502 => {
            u.low_latency_rc = id == 501;
            Some(if u.low_latency_rc { "L1" } else { "L0" }.into())
        }
        _ => None,
    };
    if let Some(m) = msg {
        let _ = u.cmd.send(m.into_bytes());
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                let (w, h) = (lp.0 as u64 & 0xffff, (lp.0 as u64 >> 16) & 0xffff);
                SIZE.store(w << 32 | h, Ordering::Relaxed);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_KEYDOWN if wp.0 == VK_F11.0 as usize => {
                let mut g = UI.lock().unwrap();
                let u = g.as_mut().unwrap();
                let on = !u.fullscreen;
                set_fullscreen(hwnd, u, on);
                LRESULT(0)
            }
            // F10 arrives as a system key; handling it here also stops Windows from entering menu mode.
            WM_SYSKEYDOWN if wp.0 == VK_F10.0 as usize => {
                show_menu(hwnd);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                show_menu(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn main() -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    let mons = monitors();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        // Built without a console window; borrow the one we were started from so this text shows.
        unsafe { _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS) };
        eprintln!("usage: viewer <udp-port> <monitor#> [--mbps N] [--codec h264|hevc] [--fps 60|120] [--speed] [--standard-rc] [--windowed]\nmonitors:");
        for (i, (_, r, p)) in mons.iter().enumerate() {
            eprintln!("  {i}: {}x{} at ({},{}){}", r.right - r.left, r.bottom - r.top, r.left, r.top, if *p { " primary" } else { "" });
        }
        std::process::exit(1);
    }
    let port: u16 = args[1].parse().expect("port");
    let (_, m, _) = *mons.get(args[2].parse::<usize>().expect("monitor#")).expect("no such monitor");
    let (mw, mh) = (m.right - m.left, m.bottom - m.top);

    // Optional starting settings; sent to the Mac like menu choices.
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str);
    let mbps: u32 = opt("--mbps").map(|v| v.parse().expect("--mbps")).unwrap_or(30);
    let hevc = opt("--codec") == Some("hevc");
    let fps120 = opt("--fps") == Some("120");
    let speed = args.iter().any(|a| a == "--speed");
    let low_latency_rc = !args.iter().any(|a| a == "--standard-rc");
    let (cmd_tx, cmd_rx) = mpsc::channel();
    for c in [format!("B{mbps}"), if hevc { "C265" } else { "C264" }.into(), if fps120 { "F120" } else { "F60" }.into(), if speed { "S1" } else { "S0" }.into(), if low_latency_rc { "L1" } else { "L0" }.into()] {
        cmd_tx.send(c.into_bytes()).unwrap();
    }
    *UI.lock().unwrap() = Some(Ui { fullscreen: false, saved: (0, RECT::default()), mbps, hevc, fps120, speed, low_latency_rc, cmd: cmd_tx });

    unsafe {
        let inst = GetModuleHandleW(None)?;
        let class = w!("screenbridge");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Created as a normal window (60% of the monitor, centered) so F11 has somewhere to return to.
        let (ww, wh) = (mw * 3 / 5, mh * 3 / 5);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &HSTRING::from(format!("screenbridge :{port}")),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            m.left + (mw - ww) / 2,
            m.top + (mh - wh) / 2,
            ww,
            wh,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        if !args.iter().any(|a| a == "--windowed") {
            set_fullscreen(hwnd, UI.lock().unwrap().as_mut().unwrap(), true);
        }
        let h = hwnd.0 as isize;
        std::thread::spawn(move || receive(port, h, cmd_rx));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    std::process::exit(0); // also ends the receive thread
}
