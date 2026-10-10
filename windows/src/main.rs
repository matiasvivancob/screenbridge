//! ScreenBridge for Windows: shows a Mac's extra displays on this PC's monitors.
//!   screenbridge.exe                    tray app (normal use)
//!   screenbridge.exe --settings         settings window (started from the tray)
//!   screenbridge.exe --view PORT N      one bare viewer on monitor N, for testing with tools/test_sender.py

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window

mod config;
mod session;
mod settings;
mod tray;
mod video;
mod viewer;

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--settings") {
        settings::run().expect("settings window");
        return;
    }
    unsafe {
        // Physical pixels everywhere, so a 2560x1440 stream maps 1:1 onto a 2560x1440 monitor at any scaling.
        _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).expect("Media Foundation");
    }
    if let Some(i) = args.iter().position(|a| a == "--view") {
        // Built without a console window; borrow the one we were started from so SB_VERBOSE statistics print.
        unsafe { _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS) };
        let port = args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or(config::FIRST_VIDEO_PORT);
        let monitors = config::monitors();
        let m = args.get(i + 2).and_then(|n| n.parse::<usize>().ok()).and_then(|n| monitors.get(n)).or(monitors.first()).expect("no monitors");
        let cfg = config::Config::load();
        let settings = viewer::Settings {
            mbps: cfg.mbps,
            hevc: cfg.codec == config::Codec::Hevc,
            fps120: cfg.fps == 120,
            speed: cfg.speed,
            low_latency_rc: cfg.low_latency_rc,
        };
        let spec = viewer::Spec { id: 0, port, monitor: m.rect, fullscreen: !args.iter().any(|a| a == "--windowed"), title: format!("ScreenBridge :{port}"), settings };
        let (tx, rx) = std::sync::mpsc::channel();
        let _h = viewer::open(spec, tx).expect("UDP port in use");
        let _ = rx.recv();
        return;
    }
    tray::run();
}
