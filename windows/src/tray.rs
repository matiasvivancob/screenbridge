//! Notification-area app: Extend/Stop, Settings, Start with Windows, Quit. Double-click the icon to toggle.

use crate::session::{self, Command, Macs, Status};
use std::sync::{Arc, Mutex, mpsc};
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

struct App {
    hwnd: isize,
    macs: Macs,
    status: Status,
    session: Option<(mpsc::Sender<Command>, std::thread::JoinHandle<()>)>,
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
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("ScreenBridgeTray"), w!("ScreenBridge"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None).unwrap();

        let macs: Macs = Arc::new(Mutex::new(Vec::new()));
        let m = macs.clone();
        std::thread::spawn(move || session::discover(m));
        *APP.lock().unwrap() = Some(App { hwnd: hwnd.0 as isize, macs, status: Status::Idle, session: None });
        add_icon(hwnd);

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

fn status_text(s: &Status) -> String {
    match s {
        Status::Idle => "Not extending".into(),
        Status::Connecting => "Connecting…".into(),
        Status::WaitingForApproval(mac) => format!("Click Allow on {mac}"),
        Status::Extending(mac, n) => format!("Extending {mac} · {n} display{}", if *n == 1 { "" } else { "s" }),
        Status::Failed(e) => e.clone(),
    }
}

/// Called from the session thread.
fn report(s: Status) {
    let hwnd = {
        let mut g = APP.lock().unwrap();
        let app = g.as_mut().unwrap();
        app.status = s;
        app.hwnd
    };
    unsafe { _ = PostMessageW(Some(HWND(hwnd as _)), WM_STATUS, WPARAM(0), LPARAM(0)) };
}

unsafe fn on_status(hwnd: HWND) {
    let status = APP.lock().unwrap().as_ref().unwrap().status.clone();
    let mut d = icon_data(hwnd);
    d.uFlags = NIF_TIP;
    copy(&mut d.szTip, &format!("ScreenBridge: {}", status_text(&status)));
    // Balloons only for things the user must act on.
    let balloon = match &status {
        Status::WaitingForApproval(_) => Some(("Approve on the Mac", NIIF_INFO)),
        Status::Failed(_) => Some(("ScreenBridge couldn't extend", NIIF_WARNING)),
        _ => None,
    };
    if let Some((title, flags)) = balloon {
        d.uFlags |= NIF_INFO;
        d.dwInfoFlags = flags;
        copy(&mut d.szInfoTitle, title);
        copy(&mut d.szInfo, &status_text(&status));
    }
    unsafe { _ = Shell_NotifyIconW(NIM_MODIFY, &d) };
}

fn extending() -> bool {
    APP.lock().unwrap().as_ref().unwrap().session.as_ref().is_some_and(|(_, h)| !h.is_finished())
}

fn toggle() {
    let mut g = APP.lock().unwrap();
    let app = g.as_mut().unwrap();
    if let Some((tx, h)) = app.session.take() {
        if !h.is_finished() {
            let _ = tx.send(Command::Stop);
            return;
        }
    }
    let (tx, rx) = mpsc::channel();
    let macs = app.macs.clone();
    let h = std::thread::spawn(move || session::run(macs, rx, report));
    app.session = Some((tx, h));
}

fn stop_and_wait() {
    let s = APP.lock().unwrap().as_mut().unwrap().session.take();
    if let Some((tx, h)) = s {
        let _ = tx.send(Command::Stop);
        let _ = h.join(); // closes the viewers and tells the Mac to remove its displays
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
        let status = status_text(&APP.lock().unwrap().as_ref().unwrap().status);
        let busy = extending();
        let m = CreatePopupMenu().unwrap();
        _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, &HSTRING::from(status));
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        _ = AppendMenuW(m, MF_STRING, 1, if busy { w!("Stop extending") } else { w!("Extend displays") });
        _ = SetMenuDefaultItem(m, 1, 0); // bold: what a double-click does
        _ = AppendMenuW(m, MF_STRING, 2, w!("Settings…"));
        _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        _ = AppendMenuW(m, MF_STRING | if autostart() { MF_CHECKED } else { MF_UNCHECKED }, 3, w!("Start with Windows"));
        _ = AppendMenuW(m, MF_STRING, 4, w!("Quit ScreenBridge"));
        let mut p = POINT::default();
        _ = GetCursorPos(&mut p);
        _ = SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
        let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None).0;
        _ = DestroyMenu(m);
        match id {
            1 => toggle(),
            2 => open_settings(),
            3 => set_autostart(!autostart()),
            4 => {
                stop_and_wait();
                PostQuitMessage(0);
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
                    WM_LBUTTONDBLCLK => toggle(),
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
                stop_and_wait(); // Windows shutting down: remove the Mac's displays cleanly
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
