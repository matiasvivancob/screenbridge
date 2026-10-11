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

Earlier prototype work (command-line sender and viewer) is in the git history.
