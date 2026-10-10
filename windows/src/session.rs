//! Finding Macs on the network and running one extend session with one of them. Protocol: docs/PROTOCOL.md.

use crate::config::{self, Codec, Config, DiscoveredMac};
use crate::viewer;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub type Macs = Arc<Mutex<Vec<DiscoveredMac>>>;

/// Listens for Mac announcements forever; keeps `macs` and discovered.json current.
pub fn discover(macs: Macs) {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", config::DISCOVERY_PORT)) else { return };
    let mut buf = [0u8; 2048];
    let mut saved = Instant::now() - Duration::from_secs(60);
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let Ok(v) = serde_json::from_slice::<Value>(&buf[..n]) else { continue };
        if v["type"] != "announce" {
            continue;
        }
        let name = v["name"].as_str().unwrap_or("Mac").chars().take(64).collect::<String>();
        let address = format!("{}:{}", from.ip(), v["port"].as_u64().unwrap_or(config::CONTROL_PORT as u64));
        let mut list = macs.lock().unwrap();
        let changed = match list.iter_mut().find(|m| m.name == name) {
            Some(m) => {
                m.last_seen = config::unix_now();
                std::mem::replace(&mut m.address, address) != m.address
            }
            None => {
                list.push(DiscoveredMac { name, address, last_seen: config::unix_now() });
                true
            }
        };
        // The settings window shows "online" for Macs seen in the last 10 s; refresh the file often enough.
        if changed || saved.elapsed() > Duration::from_secs(4) {
            config::save_discovered(&list);
            saved = Instant::now();
        }
    }
}

/// What the tray shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Connecting,
    WaitingForApproval(String),
    Extending(String, usize),
    Failed(String),
}

pub enum Command {
    Stop,
}

/// Runs one session to completion. `report` is called on every status change.
pub fn run(macs: Macs, commands: mpsc::Receiver<Command>, report: impl Fn(Status)) {
    report(Status::Connecting);
    match session(macs, commands, &report) {
        Ok(()) => report(Status::Idle),
        Err(e) => report(Status::Failed(e)),
    }
}

fn session(macs: Macs, commands: mpsc::Receiver<Command>, report: &impl Fn(Status)) -> Result<(), String> {
    let cfg = Config::load();
    let (addr, mac_name) = find_mac(&cfg, &macs)?;
    let stream = addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(4)).ok())
        .ok_or_else(|| format!("Couldn't connect to {mac_name} ({addr}). Is ScreenBridge running on the Mac?"))?;
    stream.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut out = stream.try_clone().map_err(|e| e.to_string())?;
    let mut send = |v: Value| -> Result<(), String> {
        writeln!(out, "{v}").map_err(|_| format!("Lost the connection to {mac_name}"))
    };
    let mut lines = BufReader::new(stream);
    let mut read = || -> Result<Option<Value>, String> {
        let mut line = String::new();
        match lines.read_line(&mut line) {
            Ok(0) => Err(format!("{mac_name} closed the connection")),
            Ok(_) => Ok(serde_json::from_str(&line).ok()),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(format!("Lost the connection to {mac_name}: {e}")),
        }
    };

    let pc_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    send(json!({"type": "hello", "pc_id": cfg.pc_id, "pc_name": pc_name, "version": 1}))?;
    let asked = Instant::now();
    loop {
        if let Ok(Command::Stop) = commands.try_recv() {
            return Ok(());
        }
        match read()? {
            Some(v) if v["type"] == "welcome" => break,
            Some(v) if v["type"] == "pending" => report(Status::WaitingForApproval(mac_name.clone())),
            Some(v) if v["type"] == "denied" => return Err(format!("{mac_name} didn't allow this PC.")),
            _ if asked.elapsed() > Duration::from_secs(120) => return Err("Nobody answered on the Mac.".into()),
            _ => {}
        }
    }

    // Open the viewers first so their ports are listening before video arrives.
    let monitors = config::monitors();
    let primary = monitors.iter().find(|m| m.primary).or(monitors.first()).ok_or("No monitors found")?.clone();
    let settings = viewer::Settings {
        mbps: cfg.mbps,
        hevc: cfg.codec == Codec::Hevc,
        fps120: cfg.fps == 120,
        speed: cfg.speed,
        low_latency_rc: cfg.low_latency_rc,
    };
    let (closed_tx, closed_rx) = mpsc::channel();
    let mut viewers = Vec::new();
    let mut displays = Vec::new();
    for (id, d) in cfg.displays.iter().filter(|d| d.enabled).enumerate() {
        let monitor = monitors.iter().find(|m| m.device == d.monitor).unwrap_or(&primary);
        let port = config::FIRST_VIDEO_PORT + id as u16;
        let spec = viewer::Spec {
            id,
            port,
            monitor: monitor.rect,
            fullscreen: d.fullscreen,
            title: format!("ScreenBridge · Display {}", id + 1),
            settings: settings.clone(),
        };
        match viewer::open(spec, closed_tx.clone()) {
            Ok(h) => viewers.push((id, h)),
            Err(e) => return Err(format!("Couldn't open display {} (UDP port {port}): {e}", id + 1)),
        }
        displays.push(json!({"id": id, "width": d.width & !1, "height": d.height & !1, "port": port}));
    }
    if viewers.is_empty() {
        return Err("No displays are enabled. Open Settings to add one.".into());
    }
    let close_all = |viewers: &[(usize, viewer::Handle)]| viewers.iter().for_each(|(_, h)| h.close());
    let st = json!({
        "codec": if settings.hevc { "hevc" } else { "h264" },
        "mbps": cfg.mbps, "fps": cfg.fps, "speed": cfg.speed, "low_latency_rc": cfg.low_latency_rc,
    });
    if let Err(e) = send(json!({"type": "extend", "displays": displays, "settings": st})) {
        close_all(&viewers);
        return Err(e);
    }
    report(Status::Extending(mac_name.clone(), viewers.len()));

    let mut last_ping = Instant::now();
    let result = loop {
        if let Ok(Command::Stop) = commands.try_recv() {
            let _ = send(json!({"type": "stop"}));
            break Ok(());
        }
        for id in closed_rx.try_iter() {
            viewers.retain(|(v, _)| *v != id);
            let _ = send(json!({"type": "remove", "id": id}));
            report(Status::Extending(mac_name.clone(), viewers.len()));
        }
        if viewers.is_empty() {
            let _ = send(json!({"type": "stop"}));
            break Ok(());
        }
        if last_ping.elapsed() > Duration::from_secs(2) {
            if let Err(e) = send(json!({"type": "ping"})) {
                break Err(e);
            }
            last_ping = Instant::now();
        }
        match read() {
            Ok(Some(v)) if v["type"] == "error" => break Err(v["message"].as_str().unwrap_or("Error on the Mac").to_string()),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    close_all(&viewers);
    result
}

/// The configured Mac: a manual address, the preferred name, or the first one announcing itself.
fn find_mac(cfg: &Config, macs: &Macs) -> Result<(String, String), String> {
    if !cfg.mac_address.trim().is_empty() {
        let a = cfg.mac_address.trim();
        let addr = if a.contains(':') { a.to_string() } else { format!("{a}:{}", config::CONTROL_PORT) };
        return Ok((addr, a.to_string()));
    }
    let deadline = Instant::now() + Duration::from_secs(5); // announcements come every 2 s
    loop {
        let now = config::unix_now();
        if let Some(m) = macs
            .lock()
            .unwrap()
            .iter()
            .filter(|m| now - m.last_seen <= 10)
            .find(|m| cfg.mac_name.is_empty() || m.name == cfg.mac_name)
        {
            return Ok((m.address.clone(), m.name.clone()));
        }
        if Instant::now() > deadline {
            return Err(if cfg.mac_name.is_empty() {
                "No Mac found. Open ScreenBridge on the Mac and make sure both computers are on the same network.".into()
            } else {
                format!("{} isn't answering. Open ScreenBridge on it, or pick another Mac in Settings.", cfg.mac_name)
            });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
