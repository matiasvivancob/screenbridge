//! Finding Macs on the network and keeping one lightweight link to the chosen Mac. Protocol: docs/PROTOCOL.md.
//!
//! The link is a TCP connection with a ping every 2 s: no windows, no GPU, almost no memory. Each display is
//! turned on and off independently; turning one on opens its viewer here and asks the Mac to create the display.

use crate::config::{self, Config, DiscoveredMac};
use crate::viewer;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub type Macs = Arc<Mutex<Vec<DiscoveredMac>>>;

/// Listens for Mac announcements forever; keeps `macs` and discovered.json current.
pub fn discover(macs: Macs) {
    if std::env::var_os("SB_DISABLE_DISCOVERY").is_some() {
        return; // tests that must never find a real Mac
    }
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
                std::mem::replace(&mut m.address, address.clone()) != address
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

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not connecting (the user disconnected, or auto-connect is off).
    Off,
    /// Looking for the Mac / connecting.
    Searching,
    WaitingForApproval(String),
    Linked(String),
    /// The link dropped or the Mac isn't there; will try again.
    Retrying(String),
}

#[derive(Clone, Debug)]
pub struct DisplayState {
    pub id: usize,
    pub label: String,
    pub on: bool,
}

/// What the tray shows. Written by the link thread, read when the tray builds its menu.
#[derive(Clone, Debug)]
pub struct LinkState {
    pub status: Status,
    pub displays: Vec<DisplayState>,
    /// Set for things the user must act on or be told about; the tray shows it once.
    pub balloon: Option<(String, String)>,
}

pub enum Command {
    Connect,
    Disconnect,
    Toggle(usize, bool),
    SetAll(bool),
    Quit,
}

pub type Shared = Arc<Mutex<LinkState>>;

pub fn new_state() -> Shared {
    Arc::new(Mutex::new(LinkState { status: Status::Off, displays: Vec::new(), balloon: None }))
}

enum Outcome {
    Disconnected, // by the user, here or on the Mac: stay off until asked
    Quit,
    Denied,
    Lost(String),
}

/// The link thread. `notify` is called after every state change.
pub fn run(macs: Macs, cmds: Receiver<Command>, state: Shared, notify: impl Fn()) {
    let mut connect = Config::load().auto_connect;
    // Displays the user had on, restored after a reconnect (Mac woke from sleep, network blip).
    let mut remembered: HashSet<usize> = HashSet::new();
    loop {
        if !connect {
            set(&state, &notify, Status::Off, true);
            match cmds.recv_timeout(Duration::from_millis(500)) {
                Ok(Command::Connect) => connect = true,
                Ok(Command::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                _ => {}
            }
            continue;
        }
        set(&state, &notify, Status::Searching, false);
        match link_once(&macs, &cmds, &state, &notify, &mut remembered) {
            Outcome::Quit => return,
            Outcome::Disconnected | Outcome::Denied => connect = false,
            Outcome::Lost(reason) => {
                set(&state, &notify, Status::Retrying(reason), true);
                let until = Instant::now() + Duration::from_secs(4);
                while Instant::now() < until {
                    match cmds.recv_timeout(Duration::from_millis(200)) {
                        Ok(Command::Disconnect) => {
                            connect = false;
                            break;
                        }
                        Ok(Command::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Ok(Command::Connect) => break,
                        _ => {}
                    }
                }
            }
        }
    }
}

fn set(state: &Shared, notify: &impl Fn(), status: Status, clear_displays: bool) {
    let mut s = state.lock().unwrap();
    if s.status != status || (clear_displays && !s.displays.is_empty()) {
        s.status = status;
        if clear_displays {
            s.displays.clear();
        }
        drop(s);
        notify();
    }
}

fn balloon(state: &Shared, notify: &impl Fn(), title: &str, text: &str) {
    state.lock().unwrap().balloon = Some((title.into(), text.into()));
    notify();
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// One connection to the Mac, from connect to disconnect.
fn link_once(macs: &Macs, cmds: &Receiver<Command>, state: &Shared, notify: &impl Fn(), remembered: &mut HashSet<usize>) -> Outcome {
    let mut cfg = Config::load();
    let cfg_path = config::dir().join("config.json");
    let mut cfg_mtime = modified(&cfg_path);

    let (addr, mac_name) = match find_mac(&cfg, macs, cmds) {
        Ok(m) => m,
        Err(Some(o)) => return o,
        Err(None) => {
            return Outcome::Lost(if cfg.mac_name.is_empty() {
                "No Mac found on the network".into()
            } else {
                format!("{} isn't answering", cfg.mac_name)
            });
        }
    };
    let stream = match addr.to_socket_addrs().ok().and_then(|mut a| a.next()).and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(4)).ok()) {
        Some(s) => s,
        None => return Outcome::Lost(format!("Couldn't connect to {mac_name} ({addr}). Is ScreenBridge running on it?")),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_nodelay(true);
    let Ok(mut out) = stream.try_clone() else { return Outcome::Lost("socket error".into()) };
    let peer_ip: Option<std::net::IpAddr> = addr.rsplit_once(':').and_then(|(h, _)| h.parse().ok());
    let mut lines = BufReader::new(stream);
    let mut pending: Vec<u8> = Vec::new(); // bytes of a line still being received
    let lost = || Outcome::Lost(format!("Lost the connection to {mac_name}"));

    let mut send = |v: Value| -> bool { writeln!(out, "{v}").is_ok() };
    // One line, or None on timeout; Err when the connection is gone.
    let read = |lines: &mut BufReader<TcpStream>, pending: &mut Vec<u8>| -> Result<Option<Value>, ()> {
        // read_until keeps what it has read in `pending` when the 200 ms timeout interrupts a line.
        match lines.read_until(b'\n', pending) {
            Ok(0) => Err(()),
            Ok(_) if pending.ends_with(b"\n") => {
                let v = serde_json::from_slice(pending).ok();
                pending.clear();
                Ok(v)
            }
            Ok(_) => Ok(None),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if pending.len() > (1 << 20) { Err(()) } else { Ok(None) } // a line this long is not our protocol
            }
            Err(_) => Err(()),
        }
    };

    let pc_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    if !send(json!({"type": "hello", "pc_id": cfg.pc_id, "pc_name": pc_name, "version": 1})) {
        return lost();
    }
    let asked = Instant::now();
    loop {
        match cmds.try_recv() {
            Ok(Command::Disconnect) => return Outcome::Disconnected,
            Ok(Command::Quit) => return Outcome::Quit,
            _ => {}
        }
        match read(&mut lines, &mut pending) {
            Err(()) => return lost(),
            Ok(Some(v)) if v["type"] == "welcome" => break,
            Ok(Some(v)) if v["type"] == "pending" => {
                set(state, notify, Status::WaitingForApproval(mac_name.clone()), false);
                balloon(state, notify, "Approve on the Mac", &format!("Click Allow on {mac_name}."));
            }
            Ok(Some(v)) if v["type"] == "denied" => {
                balloon(state, notify, "ScreenBridge", &format!("{mac_name} didn't allow this PC. Choose Connect in the tray menu to ask again."));
                return Outcome::Denied;
            }
            _ if asked.elapsed() > Duration::from_secs(120) => {
                balloon(state, notify, "ScreenBridge", "Nobody answered the approval dialog on the Mac.");
                return Outcome::Denied;
            }
            _ => {}
        }
    }

    // ---- linked ----
    let mut viewers: HashMap<usize, (usize, viewer::Handle)> = HashMap::new(); // display id -> (token, window)
    let mut pending_add: HashMap<usize, Instant> = HashMap::new();
    let (closed_tx, closed_rx) = mpsc::channel::<usize>();
    let mut next_token = 1usize;

    let publish = |cfg: &Config, viewers: &HashMap<usize, (usize, viewer::Handle)>| {
        let monitors = config::monitors();
        let mut s = state.lock().unwrap();
        s.status = Status::Linked(mac_name.clone());
        s.displays = cfg
            .displays
            .iter()
            .enumerate()
            .filter(|(_, d)| d.enabled)
            .map(|(id, d)| DisplayState {
                id,
                label: format!("Display {} · {}×{} → monitor {}", id + 1, d.width, d.height, monitor_number(&monitors, &d.monitor)),
                on: viewers.contains_key(&id),
            })
            .collect();
        drop(s);
        notify();
    };
    let layout_msg = |cfg: &Config| {
        let monitors = config::monitors();
        let displays: Vec<Value> = cfg
            .displays
            .iter()
            .enumerate()
            .filter(|(_, d)| d.enabled)
            .map(|(id, d)| json!({"id": id, "name": format!("Display {} (PC monitor {})", id + 1, monitor_number(&monitors, &d.monitor)), "width": d.width & !1, "height": d.height & !1}))
            .collect();
        json!({"type": "layout", "displays": displays})
    };
    let settings_json = |cfg: &Config| {
        json!({"codec": cfg.codec.json(),
               "mbps": cfg.mbps, "fps": cfg.fps, "speed": cfg.speed, "low_latency_rc": cfg.low_latency_rc})
    };

    // Opens the viewer, then asks the Mac for the display (so the viewer's port is listening before video arrives).
    let turn_on = |id: usize, cfg: &Config, viewers: &mut HashMap<usize, (usize, viewer::Handle)>, pending: &mut HashMap<usize, Instant>,
                   next_token: &mut usize, send: &mut dyn FnMut(Value) -> bool| {
        let Some(d) = cfg.displays.get(id).filter(|d| d.enabled) else { return };
        if viewers.contains_key(&id) {
            return;
        }
        let monitors = config::monitors();
        let Some(primary) = monitors.iter().find(|m| m.primary).or(monitors.first()) else { return };
        let monitor = monitors.iter().find(|m| m.device == d.monitor).unwrap_or(primary);
        let port = config::FIRST_VIDEO_PORT + id as u16;
        let token = *next_token;
        *next_token += 1;
        let make_spec = || viewer::Spec {
            id: token,
            port,
            peer: peer_ip,
            monitor: monitor.rect,
            fullscreen: d.fullscreen,
            title: format!("ScreenBridge · Display {}", id + 1),
            settings: viewer::Settings {
                mbps: cfg.mbps,
                codec: cfg.codec,
                fps120: cfg.fps == 120,
                speed: cfg.speed,
                low_latency_rc: cfg.low_latency_rc,
            },
        };
        // A viewer closed a moment ago may still hold the port; give it a second to let go.
        let mut opened = None;
        for _ in 0..10 {
            match viewer::open(make_spec(), closed_tx.clone()) {
                Ok(h) => {
                    opened = Some(h);
                    break;
                }
                Err(e) if e.kind() == ErrorKind::AddrInUse => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        match opened {
            Some(h) => {
                viewers.insert(id, (token, h));
                pending.insert(id, Instant::now());
                let settings = settings_json(cfg);
                send(json!({"type": "add", "display": {"id": id, "width": d.width & !1, "height": d.height & !1, "port": port}, "settings": settings}));
            }
            None => balloon(state, notify, "ScreenBridge", &format!("Couldn't open display {} (UDP port {port} is in use).", id + 1)),
        }
    };
    let turn_off = |id: usize, viewers: &mut HashMap<usize, (usize, viewer::Handle)>, pending: &mut HashMap<usize, Instant>, send: &mut dyn FnMut(Value) -> bool| {
        if let Some((_, h)) = viewers.remove(&id) {
            h.close();
            pending.remove(&id);
            send(json!({"type": "remove", "id": id}));
        }
    };

    if !send(layout_msg(&cfg)) {
        return lost();
    }
    publish(&cfg, &viewers);
    // Restore what was on before a reconnect, plus the displays marked "on at connect".
    let startup: Vec<usize> = cfg
        .displays
        .iter()
        .enumerate()
        .filter(|(id, d)| d.enabled && (d.auto_on || remembered.contains(id)))
        .map(|(id, _)| id)
        .collect();
    for id in startup {
        turn_on(id, &cfg, &mut viewers, &mut pending_add, &mut next_token, &mut send);
    }
    *remembered = viewers.keys().copied().collect();
    publish(&cfg, &viewers);

    let close_all = |viewers: &mut HashMap<usize, (usize, viewer::Handle)>| {
        for (_, (_, h)) in viewers.drain() {
            h.close();
        }
    };
    let mut last_ping = Instant::now();
    let mut last_heard = Instant::now(); // any message from the Mac (it answers every ping)
    let mut last_cfg_check = Instant::now();
    let outcome = loop {
        let mut dirty = false;

        match cmds.try_recv() {
            Ok(Command::Disconnect) => {
                send(json!({"type": "stop"}));
                break Outcome::Disconnected;
            }
            Ok(Command::Quit) => {
                send(json!({"type": "stop"}));
                break Outcome::Quit;
            }
            Ok(Command::Toggle(id, on)) => {
                if on {
                    turn_on(id, &cfg, &mut viewers, &mut pending_add, &mut next_token, &mut send);
                } else {
                    turn_off(id, &mut viewers, &mut pending_add, &mut send);
                }
                dirty = true;
            }
            Ok(Command::SetAll(on)) => {
                let ids: Vec<usize> = cfg.displays.iter().enumerate().filter(|(_, d)| d.enabled).map(|(i, _)| i).collect();
                for id in ids {
                    if on {
                        turn_on(id, &cfg, &mut viewers, &mut pending_add, &mut next_token, &mut send);
                    } else {
                        turn_off(id, &mut viewers, &mut pending_add, &mut send);
                    }
                }
                dirty = true;
            }
            _ => {}
        }

        // A viewer window the user closed (Alt+F4) means that display is off.
        for token in closed_rx.try_iter() {
            if let Some(id) = viewers.iter().find(|(_, (t, _))| *t == token).map(|(id, _)| *id) {
                viewers.remove(&id);
                pending_add.remove(&id);
                send(json!({"type": "remove", "id": id}));
                dirty = true;
            }
        }

        // Settings saved in the settings window: re-read, tell the Mac the new layout, rebuild changed displays.
        if last_cfg_check.elapsed() > Duration::from_secs(2) {
            last_cfg_check = Instant::now();
            let m = modified(&cfg_path);
            if m != cfg_mtime {
                cfg_mtime = m;
                let new = Config::load();
                let changed: Vec<usize> = viewers
                    .keys()
                    .copied()
                    .filter(|id| new.displays.get(*id).filter(|d| d.enabled).map(|d| (d.width, d.height, &d.monitor, d.fullscreen)) != cfg.displays.get(*id).map(|d| (d.width, d.height, &d.monitor, d.fullscreen)))
                    .collect();
                cfg = new;
                send(layout_msg(&cfg));
                for id in changed {
                    turn_off(id, &mut viewers, &mut pending_add, &mut send);
                    if cfg.displays.get(id).is_some_and(|d| d.enabled) {
                        turn_on(id, &cfg, &mut viewers, &mut pending_add, &mut next_token, &mut send);
                    }
                }
                dirty = true;
            }
        }

        if last_ping.elapsed() > Duration::from_secs(2) {
            if !send(json!({"type": "ping"})) {
                break lost();
            }
            last_ping = Instant::now();
        }

        if last_heard.elapsed() > Duration::from_secs(8) {
            break Outcome::Lost(format!("Lost contact with {mac_name}"));
        }
        match read(&mut lines, &mut pending) {
            Err(()) => break lost(),
            Ok(None) => {}
            Ok(Some(v)) => {
                last_heard = Instant::now();
                match v["type"].as_str() {
                Some("bye") => break Outcome::Disconnected, // the Mac chose to disconnect: stay off until asked
                Some("toggle") => {
                    if let (Some(id), Some(on)) = (v["id"].as_u64(), v["on"].as_bool()) {
                        let id = id as usize;
                        if on {
                            turn_on(id, &cfg, &mut viewers, &mut pending_add, &mut next_token, &mut send);
                        } else {
                            turn_off(id, &mut viewers, &mut pending_add, &mut send);
                        }
                        dirty = true;
                    }
                }
                Some("error") => {
                    balloon(state, notify, "ScreenBridge couldn't turn a display on", v["message"].as_str().unwrap_or("Error on the Mac"));
                }
                Some("state") => {
                    // The Mac says which displays exist. A viewer whose display the Mac doesn't have (capture failed,
                    // request rejected) is closed, unless its request is still in flight.
                    let on: HashSet<usize> = v["on"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as usize).collect()).unwrap_or_default();
                    let stale: Vec<usize> = viewers
                        .keys()
                        .copied()
                        .filter(|id| !on.contains(id) && pending_add.get(id).is_none_or(|t| t.elapsed() > Duration::from_secs(3)))
                        .collect();
                    for id in stale {
                        if let Some((_, h)) = viewers.remove(&id) {
                            h.close();
                        }
                        pending_add.remove(&id);
                        dirty = true;
                    }
                    pending_add.retain(|id, _| !on.contains(id));
                }
                _ => {}
                }
            }
        }

        if dirty {
            *remembered = viewers.keys().copied().collect();
            publish(&cfg, &viewers);
        }
    };
    close_all(&mut viewers);
    if !matches!(outcome, Outcome::Quit | Outcome::Disconnected) {
        // Keep `remembered` for the reconnect, but the tray shows everything off meanwhile.
        state.lock().unwrap().displays.clear();
    } else {
        remembered.clear();
    }
    outcome
}

fn monitor_number(monitors: &[config::Monitor], device: &str) -> usize {
    monitors.iter().position(|m| m.device == device).map(|i| i + 1).unwrap_or(1)
}

/// The configured Mac: a manual address, the preferred name, or the first one announcing itself.
/// `Err(None)` = not found in time; `Err(Some(outcome))` = the user changed their mind while waiting.
fn find_mac(cfg: &Config, macs: &Macs, cmds: &Receiver<Command>) -> Result<(String, String), Option<Outcome>> {
    if !cfg.mac_address.trim().is_empty() {
        let a = cfg.mac_address.trim();
        let addr = if a.contains(':') { a.to_string() } else { format!("{a}:{}", config::CONTROL_PORT) };
        return Ok((addr, a.to_string()));
    }
    let deadline = Instant::now() + Duration::from_secs(5); // announcements come every 2 s
    loop {
        match cmds.try_recv() {
            Ok(Command::Disconnect) => return Err(Some(Outcome::Disconnected)),
            Ok(Command::Quit) => return Err(Some(Outcome::Quit)),
            _ => {}
        }
        let now = config::unix_now();
        if let Some(m) = macs
            .lock()
            .unwrap()
            .iter()
            .filter(|m| now.saturating_sub(m.last_seen) <= 10)
            .find(|m| cfg.mac_name.is_empty() || m.name == cfg.mac_name)
        {
            return Ok((m.address.clone(), m.name.clone()));
        }
        if Instant::now() > deadline {
            return Err(None);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
