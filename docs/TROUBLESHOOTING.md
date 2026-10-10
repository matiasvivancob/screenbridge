# Troubleshooting

The tray icon's hover text and the balloon notification always say what ScreenBridge is doing or why it stopped.

## "No Mac found"

The PC hasn't heard the Mac's announcement (UDP broadcast to port 47801, every 2 seconds).

- Is ScreenBridge running on the Mac? Look for its icon in the menu bar.
- Are both computers on the same network and subnet? A guest network or a VPN usually blocks broadcasts.
- Windows Firewall: the installer adds the rule for **private** networks. If Windows classified your network as *public*,
  set it to private (Settings → Network & internet → your connection → Network profile type).
- macOS asked to allow *local network access* and you said no: System Settings → Privacy & Security → Local Network → ScreenBridge on.
- Workaround: Settings → Mac → *Manual address*, type the Mac's IP.

## "Couldn't connect to <Mac>"

The Mac was found but TCP port 47800 didn't answer. Quit and reopen ScreenBridge on the Mac. If it still fails, another
program may be using port 47800; the Mac's menu shows "Port 47800 is in use".

## Nothing happens after "Click Allow on <Mac>"

The Mac shows an Allow dialog. It may be behind other windows or on another Space. Allow it, or remove the PC from
Mac menu → Allowed PCs and try again.

## "Screen Recording permission is missing"

System Settings → Privacy & Security → Screen Recording → switch **ScreenBridge** on, then **quit and reopen ScreenBridge on the Mac**
(macOS only applies the permission to a freshly started app). If ScreenBridge is listed but already on and it still fails after
an update, remove it from the list with **−** and add it again.

## Viewer windows stay black

- Wait a few seconds: the first frame is a full keyframe.
- F10 → **Refresh picture**.
- A black picture with a changing title (`idle (screen unchanged)`) means video is arriving but the display shows nothing new.
- Title says `decoder error`: HEVC without the HEVC Video Extensions. Switch the codec to H.264 (F10 → Codec).
- Title never changes from `waiting for the Mac…`: video isn't arriving. Check the firewall rule for UDP 47810+.

## Choppy, tearing, or "N lost" keeps increasing

- Use Ethernet on both computers. Wi-Fi drops UDP packets.
- Lower the bitrate (F10 → Bitrate). Very high bitrates create large keyframes that a congested link drops.
- Close other heavy network users while testing.
- 144 Hz monitors show a 60 fps stream with uneven pacing. Set the monitor to 120 Hz, or use 120 fps in Settings.

## Text looks soft

- Make the virtual display exactly the monitor's size (Settings → Displays → *Match monitor*), so there is no scaling.
- Raise the bitrate. Thin colored text is always slightly softer than native because video is 4:2:0.
- Turn Windows display scaling to 100% on that monitor if you want a 1:1 desktop; scaling is applied by macOS, not the PC.

## Mac displays don't go away

Quit ScreenBridge from the menu bar. The displays belong to the app and disappear with it.

## Starting over

- PC: quit from the tray, delete `%APPDATA%\ScreenBridge`, start again (a new id is generated, so the Mac asks again).
- Mac: `defaults delete com.matiasvivancob.screenbridge`, then reopen.

## Logs

Mac: Console.app → search "ScreenBridge", or `log show --last 5m --predicate 'process == "ScreenBridge"'`.
PC: run `set SB_VERBOSE=1 && "C:\Program Files\ScreenBridge\screenbridge.exe" --view 47810 0` from a terminal to print a viewer's statistics.
