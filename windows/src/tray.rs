//! Notification-area app. It keeps one lightweight link to the Mac in the background; the menu turns each display
//! on or off. Double-click the icon: all displays on (or all off if any is on).

use crate::session::{self, Command, LinkState, Macs, Shared, Status};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

const ID_ALL: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_AUTOSTART: usize = 3;
const ID_QUIT: usize = 4;
const ID_LINK: usize = 5;
const ID_DISPLAY_BASE: usize = 100;

struct App {
    state: Shared,
    link: Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}

static APP: Mutex<Option<App>> = Mutex::new(None);

pub fn run() {
    unsafe {
        // One tray icon per user: a second launch (e.g. autostart + manual) just exits.
        let _guard = CreateMutexW(None, true, w!("Local\\ScreenBridgeTray"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let inst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("ScreenBridgeTray"), ..Default::default() };
        RegisterClassW(&wc);
        // A hidden top-level window, not message-only: only top-level windows receive the TaskbarCreated broadcast
        // that tells us Explorer restarted and the icon must be added again.
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, w!("ScreenBridgeTray"), w!("ScreenBridge"), WS_POPUP, 0, 0, 0, 0, None, None, Some(inst.into()), None).unwrap();
        add_icon(hwnd);

        let macs: Macs = Arc::new(Mutex::new(Vec::new()));
        let m = macs.clone();
        std::thread::spawn(move || session::discover(m));

        let state = session::new_state();
        let (tx, rx) = std::sync::mpsc::channel();
        let (s, h) = (state.clone(), hwnd.0 as isize);
        let thread = std::thread::spawn(move || {
            session::run(macs, rx, s, move || {
                _ = PostMessageW(Some(HWND(h as _)), WM_STATUS, WPARAM(0), LPARAM(0));
            })
        });
        *APP.lock().unwrap() = Some(App { state, link: tx, thread: Some(thread) });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}

fn copy(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

unsafe fn add_icon(hwnd: HWND) {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap();
        let mut d = icon_data(hwnd);
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = LoadIconW(Some(inst.into()), PCWSTR(1 as _)).unwrap_or_default();
        copy(&mut d.szTip, "ScreenBridge");
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
    }
}

fn status_text(s: &LinkState) -> String {
    let on = s.displays.iter().filter(|d| d.on).count();
    match &s.status {
        Status::Off => "Not connected to a Mac".into(),
        Status::Searching => "Looking for the Mac…".into(),
        Status::WaitingForApproval(mac) => format!("Click Allow on {mac}"),
        Status::Linked(mac) if on == 0 => format!("Connected to {mac} · no displays on"),
        Status::Linked(mac) => format!("Connected to {mac} · {on} display{} on", if on == 1 { "" } else { "s" }),
        Status::Retrying(why) => format!("{why}. Trying again…"),
    }
}

/// Link thread -> tray: new tooltip, and a balloon if the user needs to know something.
unsafe fn on_status(hwnd: HWND) {
    let (text, balloon) = {
        let g = APP.lock().unwrap();
        let mut s = g.as_ref().unwrap().state.lock().unwrap();
        (status_text(&s), s.balloon.take())
    };
    let mut d = icon_data(hwnd);
    d.uFlags = NIF_TIP;
    copy(&mut d.szTip, &format!("ScreenBridge: {text}"));
    if let Some((title, body)) = balloon {
        d.uFlags |= NIF_INFO;
        d.dwInfoFlags = NIIF_INFO;
        copy(&mut d.szInfoTitle, &title);
        copy(&mut d.szInfo, &body);
    }
    unsafe { _ = Shell_NotifyIconW(NIM_MODIFY, &d) };
}

fn send(c: Command) {
    if let Some(app) = APP.lock().unwrap().as_ref() {
        let _ = app.link.send(c);
    }
}

fn snapshot() -> LinkState {
    APP.lock().unwrap().as_ref().unwrap().state.lock().unwrap().clone()
}

fn linked(s: &LinkState) -> bool {
    matches!(s.status, Status::Linked(_))
}

/// Double-click: everything on, or everything off when anything is on. Not connected: connect.
fn quick_toggle() {
    let s = snapshot();
    if linked(&s) {
        send(Command::SetAll(!s.displays.iter().any(|d| d.on)));
    } else {
        send(Command::Connect);
    }
}

/// Stops the link thread (closing viewers and telling the Mac) before the process goes away.
fn shutdown() {
    let t = {
        let mut g = APP.lock().unwrap();
        let app = g.as_mut().unwrap();
        let _ = app.link.send(Command::Quit);
        app.thread.take()
    };
    if let Some(t) = t {
        let _ = t.join();
    }
}

fn autostart() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("ScreenBridge"), RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("ScreenBridge"), REG_SZ.0, Some(v.as_ptr() as _), (v.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("ScreenBridge"));
        }
    }
}

fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").spawn();
    }
}

unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        let s = snapshot();
        let m = CreatePopupMenu().unwrap();
        _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, &HSTRING::from(status_text(&s)));
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);

        if linked(&s) {
            if s.displays.is_empty() {
                _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, &HSTRING::from("No displays configured (open Settings)"));
            }
            for d in &s.displays {
                let flags = MF_STRING | if d.on { MF_CHECKED } else { MF_UNCHECKED };
                _ = AppendMenuW(m, flags, ID_DISPLAY_BASE + d.id, &HSTRING::from(&d.label));
            }
            if !s.displays.is_empty() {
                let any_on = s.displays.iter().any(|d| d.on);
                _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                _ = AppendMenuW(m, MF_STRING, ID_ALL, &HSTRING::from(if any_on { "Turn all displays off" } else { "Turn all displays on" }));
                _ = SetMenuDefaultItem(m, ID_ALL as u32, 0); // bold: what a double-click does
            }
            _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        }
        let connected = !matches!(s.status, Status::Off);
        _ = AppendMenuW(m, MF_STRING, ID_LINK, &HSTRING::from(if connected { "Disconnect from the Mac" } else { "Connect to the Mac" }));
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        _ = AppendMenuW(m, MF_STRING, ID_SETTINGS, w!("Settings…"));
        _ = AppendMenuW(m, MF_STRING | if autostart() { MF_CHECKED } else { MF_UNCHECKED }, ID_AUTOSTART, w!("Start with Windows"));
        _ = AppendMenuW(m, MF_STRING, ID_QUIT, w!("Quit ScreenBridge"));

        let mut p = POINT::default();
        _ = GetCursorPos(&mut p);
        _ = SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
        let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None).0 as usize;
        _ = DestroyMenu(m);
        match id {
            0 => {}
            ID_ALL => send(Command::SetAll(!s.displays.iter().any(|d| d.on))),
            ID_SETTINGS => open_settings(),
            ID_AUTOSTART => set_autostart(!autostart()),
            ID_LINK => send(if connected { Command::Disconnect } else { Command::Connect }),
            ID_QUIT => {
                shutdown();
                PostQuitMessage(0);
            }
            id if id >= ID_DISPLAY_BASE => {
                let want = s.displays.iter().find(|d| d.id == id - ID_DISPLAY_BASE).map(|d| !d.on).unwrap_or(true);
                send(Command::Toggle(id - ID_DISPLAY_BASE, want));
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        static TASKBAR_CREATED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let taskbar_created = *TASKBAR_CREATED.get_or_init(|| RegisterWindowMessageW(w!("TaskbarCreated")));
        match msg {
            WM_TRAY => {
                match lp.0 as u32 {
                    WM_LBUTTONDBLCLK => quick_toggle(),
                    WM_RBUTTONUP | WM_LBUTTONUP => show_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_STATUS => {
                on_status(hwnd);
                LRESULT(0)
            }
            WM_ENDSESSION => {
                shutdown(); // Windows shutting down: remove the Mac's displays cleanly
                LRESULT(0)
            }
            m if m == taskbar_created => {
                add_icon(hwnd); // Explorer restarted
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
