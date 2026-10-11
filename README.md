# ScreenBridge

Use the monitors of a Windows PC as extra displays for a Mac, over your local network.

The Mac creates virtual displays and streams them (H.264, HEVC, AV1 or VP9) to full-screen, view-only windows on the PC.
Your mouse and keyboard keep working through whatever you already use to share them between the two computers
(Synergy, Deskflow, a KVM...): ScreenBridge never sends input.

```
  MacBook                                         Windows PC
  ┌───────────────────────────┐   wired LAN      ┌───────────────────────────┐
  │ ScreenBridge (menu bar)   │ ◀── control ──── │ ScreenBridge (tray)       │
  │ virtual display 1 ───────────── video ─────▶ │ viewer → monitor 1        │
  │ virtual display 2 ───────────── video ─────▶ │ viewer → monitor 2        │
  └───────────────────────────┘                  └───────────────────────────┘
```

## Install

| | |
|---|---|
| **Windows 10 (2004+) / 11** | Run `ScreenBridge-Setup-<version>.exe`, or unzip `ScreenBridge-<version>-portable.zip` and run `screenbridge.exe` (no install, no runtimes) |
| **macOS 13+**, Apple Silicon or Intel | Open `ScreenBridge-<version>.dmg`, drag ScreenBridge to Applications, open it (or run the `.app` from `ScreenBridge-<version>-mac.zip` anywhere) |

Step by step, including the first-run approvals on both machines: [docs/INSTALL.md](docs/INSTALL.md).

## Use

1. Start ScreenBridge on the Mac (it lives in the menu bar) and on the PC (it lives in the tray).
   They find each other and link up in the background; nothing is extended yet.
2. The first time, click **Allow** on the Mac.
3. Turn displays on: on the PC, double-click the tray icon (all on/off) or pick one in its menu; or pick one in the
   Mac's menu bar.

The Mac's new displays appear on the PC's monitors. Arrange them in macOS System Settings → Displays.
Turn a display off from either menu and the Mac removes it. The link itself costs about 2.5 MB of memory and no GPU.

In each viewer window: **F11** fullscreen/window, **F10** or right-click for the live settings menu
(bitrate, codec, frame rate...), **Alt+F4** close. Window title shows live fps, latency and bitrate.

**Settings…** in the tray menu picks the Mac, how many displays, their resolution, which monitor each one uses, and the stream quality.

More: [docs/CODECS.md](docs/CODECS.md) · [docs/USAGE.md](docs/USAGE.md) · [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) · [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · [docs/PROTOCOL.md](docs/PROTOCOL.md)

## Performance

Measured on a MacBook Pro M1 Pro and an RTX-class PC over gigabit Ethernet (not a guarantee for other hardware):

| Stage | Time per frame |
|---|---|
| Mac encode, H.264/HEVC (media engine) | ~11–14 ms (2560×1440) |
| Mac encode, AV1 / VP9 (CPU, desktop content) | ~3 ms / ~4.5 ms |
| PC hardware decode (any codec) | 0.1–0.3 ms |
| PC present (flip-model swapchain) | ~1 ms |

The Mac's encoder is the largest part of the latency; AV1 and VP9 trade Mac CPU for lower frame times and no
single-encoder ceiling ([details](docs/CODECS.md)). Network time on wired gigabit is negligible. Use Ethernet on both ends.

## Limits

- **Private API.** Virtual displays use `CGVirtualDisplay`, an undocumented macOS API (the same one DeskPad and BetterDisplay use).
  A macOS update could break it, and it rules out the Mac App Store.
- **No encryption, no per-stream authentication.** Use it on networks you trust. A PC must be approved once on the Mac.
- **Chroma 4:2:0.** Like every hardware-encoded stream: thin colored text is slightly softer than native.
- **Cursor is part of the video**, so it has the same latency as the rest of the picture.
- **Not signed or notarized** (see INSTALL.md for opening it).

## Build from source

```
scripts\release.ps1                         # Windows: installer in dist\  (needs Rust and Inno Setup 6)
sh mac/build.sh                             # Mac: mac/build/ScreenBridge.app + .dmg (needs Xcode Command Line Tools)
set SB_MAC_HOST=user@mac && scripts\release.ps1   # both, building the Mac side over SSH
```

## Repository

| | |
|---|---|
| `mac/` | Swift menu-bar app: virtual displays, ScreenCaptureKit, VideoToolbox, control server |
| `windows/` | Rust tray app, viewers (Media Foundation + Direct3D 11), egui settings window |
| `installer/` | Inno Setup script |
| `scripts/` | build, release and installer test scripts |
| `tools/test_sender.py` | streams a test pattern to a viewer, no Mac needed (`screenbridge.exe --view PORT N`) |
| `docs/` | install, usage, troubleshooting, architecture, protocol |
