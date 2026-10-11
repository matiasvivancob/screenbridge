# Changelog

## 0.1.0

First packaged release.

- Mac menu-bar app (`ScreenBridge.app`, universal, ad-hoc signed) and Windows tray app with installer.
- Automatic discovery of Macs on the LAN; one-time approval of a PC on the Mac.
- Configurable displays: count, size, monitor mapping, codec, bitrate, frame rate (Settings window).
- View-only fullscreen viewers with F11, F10 live menu and live statistics in the title bar.
- Hardware decode and Direct3D 11 presentation on the PC.
- Build, release and installer test scripts.
- Portable builds: `ScreenBridge-<version>-portable.zip` (Windows, settings in a `data` folder beside the exe) and
  `ScreenBridge-<version>-mac.zip`.
- The Windows exe links the C runtime statically, so it runs on a clean Windows without the Visual C++ Redistributable.

## Unreleased

- Background link: the PC stays connected to the Mac with no displays on; each display toggles individually from the
  tray menu or the Mac's menu bar. Reconnects after sleep/network loss and restores the displays that were on.
- Settings: connect automatically, and "On at connect" per display.
- Robustness (from an independent code review): deadlock between the Mac's main and encoder queues, SIGPIPE, socket
  teardown races, cancellable startup, encoder failures reported, bounded and source-filtered UDP reassembly,
  stall recovery, GPU reset recovery, no lost bytes on TCP read timeouts, pong deadline, no eviction of the active PC
  before approval, revoke disconnects, handshake timeout, tray recovery after Explorer restarts.
- Installer only stops its own copy; release script refuses stale artifacts; tests fail loudly.

Earlier prototype work (command-line sender and viewer) is in the git history.
