# Installing ScreenBridge

You need both parts: the Mac app and the Windows app. Both computers must be on the same network.
Ethernet on both is strongly recommended; Wi-Fi works but adds delay and stutter.

## Requirements

| | |
|---|---|
| Mac | macOS 13 or later. Developed and tested on macOS 27.2 with an M1 Pro; other versions are untested. |
| PC | Windows 10 version 2004 or later, or Windows 11. A GPU that can decode H.264 in hardware (any GPU from the last decade). HEVC, AV1 and VP9 additionally need Microsoft's *HEVC Video Extensions*, *AV1 Video Extension* and *VP9 Video Extensions* from the Microsoft Store (H.264 needs nothing). AV1 also needs a GPU with AV1 decode. |
| Network | Same subnet. UDP ports 47801 (discovery) and 47810+ (video), TCP 47800 (control). |

## 1. Mac

1. Open `ScreenBridge-<version>.dmg` and drag **ScreenBridge** onto **Applications**.
2. Open ScreenBridge from Applications. The first time, macOS may say it can't verify the app, because it isn't notarized:
   - **macOS 15 and later:** open System Settings → Privacy & Security, scroll to the message about ScreenBridge and click **Open Anyway**.
   - **macOS 13–14:** right-click ScreenBridge → **Open** → **Open**.
   - Or, in Terminal: `xattr -dr com.apple.quarantine /Applications/ScreenBridge.app`
3. macOS asks for **Screen Recording** permission. Click **Open System Settings**, switch ScreenBridge on, then quit and reopen ScreenBridge.
   (This is how the Mac is allowed to capture its virtual displays. ScreenBridge only captures its own virtual displays.)
4. Optional: menu bar icon → **Open at Login**.

A small display icon appears in the menu bar. It has no Dock icon.

## 2. Windows

1. Run `ScreenBridge-Setup-<version>.exe`. If SmartScreen warns about an unknown publisher (the installer isn't signed),
   click **More info → Run anyway**.
2. Accept the administrator prompt. It is only used to allow ScreenBridge through Windows Firewall on private networks.
3. Leave **Start ScreenBridge when I sign in to Windows** ticked if you want the tray app at every sign-in.

ScreenBridge appears in the notification area (tray). If you only see a chevron, click it and drag the icon out.

## Portable (no installer)

If you'd rather not install anything, or you're still changing things:

- **Windows:** unzip `ScreenBridge-<version>-portable.zip` anywhere and run `screenbridge.exe`.
  Settings are kept in a `data` folder next to it (because of the `portable.txt` file); delete that file to use `%APPDATA%` instead.
  The exe needs no other files or runtimes. The first run triggers a Windows Firewall prompt: allow it on **private** networks
  (the installer would have added that rule for you). *Start with Windows* still works from the tray menu and points at wherever the exe is.
- **Mac:** `ScreenBridge.app` is already portable: it runs from anywhere (Downloads, a USB stick).
  `ScreenBridge-<version>-mac.zip` is the same app without the disk image. *Open at Login* works best from the Applications folder.

Portable and installed copies can coexist; they keep separate settings and each is its own PC identity on the Mac.

## 3. First connection

1. On the PC, double-click the tray icon (or right-click → **Extend displays**).
2. On the Mac, a dialog asks to allow the PC. Click **Allow**. The PC is remembered; remove it any time from the menu bar → **Allowed PCs**.
3. Fullscreen windows open on the PC's monitors and the Mac gets extra displays.
4. On the Mac, open System Settings → Displays and drag the new displays to match how your monitors sit.

## Updating

Run the new installer / replace the app in Applications. Settings are kept
(`%APPDATA%\ScreenBridge\config.json` on the PC).

## Uninstalling

- **Windows:** Settings → Apps → ScreenBridge → Uninstall (also removes the firewall rule and autostart entry).
  `%APPDATA%\ScreenBridge` holds your settings; delete it for a completely clean removal.
- **Mac:** quit ScreenBridge from the menu bar and drag it from Applications to the Bin. If you enabled *Open at Login*, turn that off first.
