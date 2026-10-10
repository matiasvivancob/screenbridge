//! Standalone settings editor for the Windows app.

use crate::config::{self, Codec, Config, DiscoveredMac, DisplayCfg, Monitor};
use eframe::egui;
use std::time::{Duration, Instant};

const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);
const RESOLUTIONS: [(u32, u32); 7] = [
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (2560, 1600),
    (3440, 1440),
    (3840, 2160),
];

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 600.0])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        "ScreenBridge Settings",
        options,
        Box::new(|_| Ok(Box::new(Settings::new()))),
    )
}

struct Settings {
    config: Config,
    saved: Config,
    monitors: Vec<Monitor>,
    macs: Vec<DiscoveredMac>,
    next_discovery: Instant,
    error: Option<String>,
    saved_notice: bool,
    confirm_close: bool,
}

impl Settings {
    fn new() -> Self {
        // The app creator runs after eframe establishes Windows DPI awareness.
        // Config::load may also enumerate monitors when creating defaults.
        let config = Config::load();
        Self {
            saved: config.clone(),
            config,
            monitors: config::monitors(),
            macs: config::load_discovered(),
            next_discovery: Instant::now() + DISCOVERY_INTERVAL,
            error: None,
            saved_notice: false,
            confirm_close: false,
        }
    }

    fn save(&mut self) {
        let result = (|| {
            for (index, display) in self.config.displays.iter().enumerate() {
                if !(640..=7680).contains(&display.width)
                    || !(640..=7680).contains(&display.height)
                    || display.width % 2 != 0
                    || display.height % 2 != 0
                {
                    return Err(format!(
                        "Display {}: width and height must be even and between 640 and 7680 pixels.",
                        index + 1
                    ));
                }
            }
            if !self.config.displays.iter().any(|display| display.enabled) {
                return Err("Enable at least one display.".to_owned());
            }
            for (index, display) in self.config.displays.iter().enumerate() {
                if display.enabled
                    && self.config.displays[..index]
                        .iter()
                        .any(|other| other.enabled && other.monitor == display.monitor)
                {
                    return Err(format!(
                        "Display {}: two enabled displays cannot use the same monitor.",
                        index + 1
                    ));
                }
            }
            self.config
                .save()
                .map_err(|error| format!("Could not save settings: {error}"))
        })();

        match result {
            Ok(()) => {
                self.saved = self.config.clone();
                self.error = None;
                self.saved_notice = true;
                self.confirm_close = false;
            }
            Err(error) => {
                self.error = Some(error);
                self.saved_notice = false;
            }
        }
    }
}

impl eframe::App for Settings {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        if now >= self.next_discovery {
            self.macs = config::load_discovered();
            self.next_discovery = now + DISCOVERY_INTERVAL;
        }
        ctx.request_repaint_after(self.next_discovery.saturating_duration_since(now));

        if ctx.input(|input| input.viewport().close_requested())
            && self.config != self.saved
            && !self.confirm_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let before = self.config.clone();
        let mut save_clicked = false;
        let mut close_clicked = false;

        egui::Panel::bottom("settings_actions").show(ui, |ui| {
            ui.horizontal(|ui| {
                save_clicked = ui.button("Save").clicked();
                close_clicked = ui.button("Close").clicked();
            });
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::RED, error);
            }
            if self.saved_notice {
                ui.label("Saved. Changes apply the next time you extend.");
            }
            if self.confirm_close {
                ui.label("Unsaved changes. Click Close again to discard them.");
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Mac");
                let now = config::unix_now();
                let mac_label = |mac: &DiscoveredMac| {
                    let status = if now.saturating_sub(mac.last_seen) <= 10 {
                        "online"
                    } else {
                        "offline"
                    };
                    format!("{} ({status})", mac.name)
                };
                let selected_mac = if self.config.mac_name.is_empty() {
                    "Automatic (first Mac found)".to_owned()
                } else {
                    self.macs
                        .iter()
                        .find(|mac| mac.name == self.config.mac_name)
                        .map(&mac_label)
                        .unwrap_or_else(|| format!("{} (offline)", self.config.mac_name))
                };
                egui::ComboBox::from_id_salt("mac")
                    .selected_text(selected_mac)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.config.mac_name,
                            String::new(),
                            "Automatic (first Mac found)",
                        );
                        for mac in &self.macs {
                            ui.selectable_value(
                                &mut self.config.mac_name,
                                mac.name.clone(),
                                mac_label(mac),
                            );
                        }
                    });
                ui.label("Manual address (optional)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.config.mac_address)
                        .hint_text("e.g. 192.168.1.104"),
                );
                ui.label(
                    egui::RichText::new(
                        "Use this if the Mac is on another network or not found automatically.",
                    )
                    .small()
                    .weak(),
                );

                ui.separator();
                ui.heading("Displays");
                let mut remove = None;
                egui::ScrollArea::horizontal()
                    .id_salt("display_scroll")
                    .show(ui, |ui| {
                        egui::Grid::new("displays")
                            .striped(true)
                            .min_col_width(0.0)
                            .show(ui, |ui| {
                                ui.label("Enabled");
                                ui.label("Resolution");
                                ui.label("Width × height");
                                ui.label("Monitor");
                                ui.label("Fullscreen");
                                ui.label("");
                                ui.end_row();

                                for (index, display) in self.config.displays.iter_mut().enumerate()
                                {
                                    ui.checkbox(&mut display.enabled, "");
                                    let monitor = self
                                        .monitors
                                        .iter()
                                        .find(|monitor| monitor.device == display.monitor);
                                    let size = (display.width, display.height);
                                    let preset = RESOLUTIONS.contains(&size);
                                    let resolution = if preset {
                                        format!("{}x{}", size.0, size.1)
                                    } else {
                                        "Custom".to_owned()
                                    };
                                    egui::ComboBox::from_id_salt(("resolution", index))
                                        .width(110.0)
                                        .selected_text(resolution)
                                        .show_ui(ui, |ui| {
                                            for (width, height) in RESOLUTIONS {
                                                if ui
                                                    .selectable_label(
                                                        size == (width, height),
                                                        format!("{width}x{height}"),
                                                    )
                                                    .clicked()
                                                {
                                                    display.width = width;
                                                    display.height = height;
                                                }
                                            }
                                            if ui
                                                .add_enabled(
                                                    monitor.is_some(),
                                                    egui::Button::new("Match monitor"),
                                                )
                                                .clicked()
                                                && let Some(monitor) = monitor
                                            {
                                                display.width = monitor.width();
                                                display.height = monitor.height();
                                            }
                                            if !preset {
                                                let _ = ui.selectable_label(true, "Custom");
                                            }
                                        });
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            [52.0, ui.spacing().interact_size.y],
                                            egui::DragValue::new(&mut display.width),
                                        );
                                        ui.label("×");
                                        ui.add_sized(
                                            [52.0, ui.spacing().interact_size.y],
                                            egui::DragValue::new(&mut display.height),
                                        );
                                    });
                                    let monitor_label = self
                                        .monitors
                                        .iter()
                                        .enumerate()
                                        .find(|(_, monitor)| monitor.device == display.monitor)
                                        .map(|(index, monitor)| monitor.label(index))
                                        .unwrap_or_else(|| {
                                            format!("(disconnected) {}", display.monitor)
                                        });
                                    egui::ComboBox::from_id_salt(("monitor", index))
                                        .width(155.0)
                                        .wrap_mode(egui::TextWrapMode::Truncate)
                                        .selected_text(&monitor_label)
                                        .show_ui(ui, |ui| {
                                            for (index, monitor) in self.monitors.iter().enumerate()
                                            {
                                                ui.selectable_value(
                                                    &mut display.monitor,
                                                    monitor.device.clone(),
                                                    monitor.label(index),
                                                );
                                            }
                                        })
                                        .response
                                        .on_hover_text(monitor_label);
                                    ui.checkbox(&mut display.fullscreen, "");
                                    if ui.button("✕").on_hover_text("Remove display").clicked() {
                                        remove = Some(index);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                if let Some(index) = remove {
                    self.config.displays.remove(index);
                }
                let unused_monitor = self.monitors.iter().find(|monitor| {
                    !self
                        .config
                        .displays
                        .iter()
                        .any(|display| display.enabled && display.monitor == monitor.device)
                });
                if ui
                    .add_enabled(
                        self.config.displays.len() < 4 && unused_monitor.is_some(),
                        egui::Button::new("Add display"),
                    )
                    .on_hover_text(
                        "Add up to four displays, using the first unused connected monitor.",
                    )
                    .clicked()
                    && let Some(monitor) = unused_monitor
                {
                    self.config.displays.push(DisplayCfg {
                        enabled: true,
                        width: monitor.width(),
                        height: monitor.height(),
                        monitor: monitor.device.clone(),
                        fullscreen: true,
                    });
                }

                ui.separator();
                ui.heading("Stream");
                ui.horizontal(|ui| {
                    ui.label("Codec");
                    ui.radio_value(&mut self.config.codec, Codec::H264, "H.264")
                        .on_hover_text("Broad compatibility with hardware video decoders.");
                    ui.radio_value(&mut self.config.codec, Codec::Hevc, "HEVC")
                        .on_hover_text(
                            "More detail at the same bitrate; requires HEVC decoding support.",
                        );
                });
                ui.add(egui::Slider::new(&mut self.config.mbps, 5..=100).text("Bitrate (Mbps)"))
                    .on_hover_text(
                        "Higher bitrate preserves more detail but uses more network bandwidth.",
                    );
                ui.horizontal(|ui| {
                    ui.label("Frame rate");
                    ui.radio_value(&mut self.config.fps, 60, "60")
                        .on_hover_text(
                            "60 frames per second uses less bandwidth and encoding work.",
                        );
                    ui.radio_value(&mut self.config.fps, 120, "120")
                        .on_hover_text(
                            "120 frames per second gives smoother motion with more encoding work.",
                        );
                });
                ui.checkbox(&mut self.config.speed, "Prioritize encoding speed")
                    .on_hover_text(
                        "Favor faster encoding over compression efficiency and image quality.",
                    );
                ui.checkbox(
                    &mut self.config.low_latency_rc,
                    "Apple low-latency rate control",
                )
                .on_hover_text(
                    "Ask the Mac encoder to minimize buffering while controlling the bitrate.",
                );
                ui.label(
                    egui::RichText::new(
                        "These are the starting values; change them live with F10 in a viewer.",
                    )
                    .small()
                    .weak(),
                );
            });
        });

        if self.config != before {
            self.error = None;
            self.saved_notice = false;
            self.confirm_close = false;
        }
        // Process buttons after the fields so a focus-losing edit is saved too.
        if save_clicked {
            self.save();
            ctx.request_repaint();
        }
        if close_clicked {
            if self.config == self.saved || self.confirm_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.confirm_close = true;
                ctx.request_repaint();
            }
        }
    }
}
