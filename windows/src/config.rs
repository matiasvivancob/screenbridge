//! Settings shared by the tray app and the settings window, stored as JSON in %APPDATA%\ScreenBridge.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::core::BOOL;

pub const CONTROL_PORT: u16 = 47800;
pub const DISCOVERY_PORT: u16 = 47801;
pub const FIRST_VIDEO_PORT: u16 = 47810;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Random id this PC presents to the Mac; the Mac remembers which ids the user allowed.
    pub pc_id: String,
    /// Preferred Mac by announced name. Empty = the first Mac found.
    pub mac_name: String,
    /// Manual "host" or "host:port", used instead of discovery when set (other subnets, VPNs).
    pub mac_address: String,
    /// Link to the Mac in the background as soon as ScreenBridge starts (no displays are turned on by this).
    pub auto_connect: bool,
    pub displays: Vec<DisplayCfg>,
    pub codec: Codec,
    pub mbps: u32,
    pub fps: u32,
    pub speed: bool,
    pub low_latency_rc: bool,
    pub cursor_overlay: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct DisplayCfg {
    pub enabled: bool,
    /// Virtual display size on the Mac, in pixels.
    pub width: u32,
    pub height: u32,
    /// Windows monitor device name (e.g. `\\.\DISPLAY1`) the viewer opens on.
    pub monitor: String,
    pub fullscreen: bool,
    /// Turn this display on whenever the link to the Mac comes up.
    pub auto_on: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
    Av1,
    Vp9,
}

impl Codec {
    /// From the id in the video packet header (flags bits 1-3).
    pub fn from_id(id: u8) -> Codec {
        match id {
            1 => Codec::Hevc,
            2 => Codec::Av1,
            3 => Codec::Vp9,
            _ => Codec::H264,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
            Codec::Vp9 => "VP9",
        }
    }

    /// Name in the TCP `add` message.
    pub fn json(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
            Codec::Vp9 => "vp9",
        }
    }

    /// Message on the UDP back channel.
    pub fn wire(self) -> &'static str {
        match self {
            Codec::H264 => "C264",
            Codec::Hevc => "C265",
            Codec::Av1 => "CAV1",
            Codec::Vp9 => "CVP9",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            pc_id: random_id(),
            mac_name: String::new(),
            mac_address: String::new(),
            auto_connect: true,
            // One virtual display per PC monitor, at that monitor's resolution.
            displays: monitors()
                .into_iter()
                .map(|m| DisplayCfg { enabled: true, width: m.width(), height: m.height(), monitor: m.device, fullscreen: true, auto_on: false })
                .collect(),
            codec: Codec::H264,
            mbps: 30,
            fps: 60,
            speed: false,
            low_latency_rc: true,
            cursor_overlay: true,
        }
    }
}

impl Default for DisplayCfg {
    fn default() -> Self {
        DisplayCfg { enabled: true, width: 1920, height: 1080, monitor: String::new(), fullscreen: true, auto_on: false }
    }
}

/// Where settings live. Portable mode: a `portable.txt` next to the exe keeps everything in `data\` beside it
/// (so the folder can be moved or run from a USB stick); otherwise %APPDATA%\ScreenBridge.
pub fn dir() -> PathBuf {
    let portable = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).filter(|p| p.join("portable.txt").exists());
    let d = match portable {
        Some(p) => p.join("data"),
        None => PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("ScreenBridge"),
    };
    let _ = std::fs::create_dir_all(&d);
    d
}

impl Config {
    /// Loads the config, creating (and saving) defaults on first run.
    pub fn load() -> Config {
        let path = dir().join("config.json");
        // trim_start_matches: a UTF-8 BOM (older Notepad, some scripts) must not make us discard the user's settings.
        match std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Config>(s.trim_start_matches('\u{feff}')).ok()) {
            Some(mut c) => {
                if c.pc_id.is_empty() {
                    c.pc_id = random_id();
                    let _ = c.save();
                }
                c
            }
            None => {
                let c = Config::default();
                let _ = c.save();
                c
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let tmp = dir().join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap())?;
        std::fs::rename(tmp, dir().join("config.json")) // atomic, so a crash never leaves half a file
    }
}

/// Macs heard on the network, written by the tray app and read by the settings window.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DiscoveredMac {
    pub name: String,
    pub address: String, // "ip:port"
    pub last_seen: u64,  // unix seconds
}

pub fn load_discovered() -> Vec<DiscoveredMac> {
    std::fs::read_to_string(dir().join("discovered.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_discovered(macs: &[DiscoveredMac]) {
    let _ = std::fs::write(dir().join("discovered.json"), serde_json::to_string_pretty(macs).unwrap());
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct Monitor {
    pub device: String,
    pub rect: RECT,
    pub primary: bool,
}

impl Monitor {
    pub fn width(&self) -> u32 {
        (self.rect.right - self.rect.left) as u32
    }
    pub fn height(&self) -> u32 {
        (self.rect.bottom - self.rect.top) as u32
    }
    pub fn label(&self, index: usize) -> String {
        format!("{}: {}x{}{}", index + 1, self.width(), self.height(), if self.primary { " (primary)" } else { "" })
    }
}

/// PC monitors in physical pixels (call after the process is per-monitor DPI aware).
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(m, &mut mi as *mut _ as *mut MONITORINFO) }.as_bool() {
            let v = unsafe { &mut *(out.0 as *mut Vec<Monitor>) };
            let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
            v.push(Monitor { device: String::from_utf16_lossy(&mi.szDevice[..n]), rect: mi.monitorInfo.rcMonitor, primary: mi.monitorInfo.dwFlags & 1 != 0 });
        }
        true.into()
    }
    let mut v: Vec<Monitor> = Vec::new();
    unsafe { _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize)) };
    v.sort_by_key(|m| (m.rect.left, m.rect.top)); // left-to-right, like the physical desk
    v
}

fn random_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded from the OS RNG; two draws give 128 random bits.
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(a);
    format!("{:016x}{:016x}", a, h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_overlay_defaults_on_in_existing_configs() {
        let old: Config = serde_json::from_str("{}").unwrap();
        assert!(old.cursor_overlay);
        let off: Config = serde_json::from_str(r#"{"cursor_overlay":false}"#).unwrap();
        assert!(!off.cursor_overlay);
    }
}
