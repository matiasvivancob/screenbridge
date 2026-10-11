# Using ScreenBridge

## Background link and per-display toggles

ScreenBridge keeps one tiny connection to the Mac in the background (about 2.5 MB of private memory, nothing on the GPU,
no windows). **Nothing is extended until you turn a display on.** Each display toggles on its own, from either computer,
and the link survives toggling, the Mac sleeping and waking, and short network drops (displays that were on come back).

## The tray icon (PC)

| Action | Result |
|---|---|
| Double-click | All displays on, or all off if any is on (not connected: connect) |
| Right-click / click | Menu: status, one checkable item per display, **Turn all displays on/off**, **Connect/Disconnect**, **Settings…**, **Start with Windows**, **Quit** |
| Hover | Current status |

A balloon appears only when you need to act (click Allow on the Mac) or something failed, with the reason.
Closing a viewer window (Alt+F4) turns that display off; the link stays up.

## The menu-bar icon (Mac)

Status line, one checkable item per display the PC offers (click to turn it on/off; the PC opens or closes its window),
**Disconnect from <PC>** (the PC stays disconnected until you connect it again), **Allowed PCs** (remove one to make it ask
again; removing the PC that is linked disconnects it), **Open at Login**, **Quit**.
The icon shows two displays while any display is on.

## In a viewer window

| Key / action | Result |
|---|---|
| **F11** | Fullscreen ↔ window |
| **F10** or right-click | Settings menu, applied live: bitrate, codec, frame rate, encoder priority, rate control, cursor overlay, refresh picture, close |
| **Alt+F4** | Close this viewer. That display turns off; the link to the Mac stays up. |

Viewers are ordinary windows (not always on top). Mouse and keyboard input in them is ignored.

**Draw the cursor on the PC (lower pointer lag)** is on by default. The Mac sends pointer position and shape separately
from the video, and the PC draws the pointer over the latest picture without waiting for another video frame. Toggle
it per viewer with F10; turning it off returns to the pointer captured in the Mac's video. This does not send mouse input.

The title bar shows live numbers, for example `60 fps · Mac encode 12.8 ms (max 19.8) · decode 0.3 ms · present 0.8 ms · 31.2 Mbps · H.264`.
`N lost` means that many frames were damaged and the stream re-synced.

## Settings (tray → Settings…)

| Setting | Notes |
|---|---|
| **Connect in the background** | Link to the Mac as soon as ScreenBridge starts. Off = connect from the tray menu when you want. |
| **Mac** | *Automatic* picks the first Mac found. Choose a specific one if you have several. *Manual address* is for a Mac that can't be discovered (another subnet, VPN): `192.168.1.104` or `host:47800`. |
| **Displays** | One row per virtual display: enabled (offered in the menus), size, which PC monitor shows it, start fullscreen, **On at connect** (turns on by itself whenever the link comes up). Up to 4. *Match monitor* sets the size to that monitor's native resolution, which gives the sharpest text. |
| **Codec** | H.264 works everywhere. HEVC gives better quality per Mbps but needs the HEVC Video Extensions on the PC. AV1 and VP9 are encoded on the Mac's CPU (about 2.7 and 3.7 cores per 1440p display), have no frame-rate ceiling from the single hardware encoder and finish desktop frames faster; they need the AV1 / VP9 Video Extensions on the PC. See [CODECS.md](CODECS.md). |
| **Bitrate** | 20–40 Mbps is plenty on wired gigabit for a 1440p desktop. |
| **Frame rate** | 60 is right for most monitors. 120 halves the capture wait and looks smoother on a 120/144 Hz monitor, at more GPU cost on the Mac. |
| **Prioritize encoding speed** | Trades some quality for lower encode time. |
| **Apple low-latency rate control** | Leave on unless a particular Mac encodes slowly with it. |
| **Draw the cursor on the PC (lowest pointer lag)** | On by default. Draw the Mac pointer separately from video for lower pointer lag; the starting value for each viewer. |

Saved settings are picked up within a couple of seconds: a display that is on is rebuilt if its size or monitor changed;
stream settings, including cursor overlay, apply to the next display you turn on. The F10 menu changes a running display immediately but isn't saved.

## Sharing the mouse and keyboard

ScreenBridge only moves pixels. Move the pointer onto the Mac's extra displays with the tool you already use
(Synergy, Deskflow, Barrier, a KVM).

With Synergy, the Mac is one screen in the layout no matter how many virtual displays it has. While extending,
you usually want the pointer to stay on the Mac: use Synergy's *lock cursor to screen* hotkeys, for example

```
keystroke(control+alt+F11) = switchToScreen(Mac), lockCursorToScreen(on)
keystroke(control+alt+F12) = lockCursorToScreen(off), switchToScreen(PC)
```

Use your own screen names, and check your Synergy version's documentation for the exact syntax.

## Files

| Where | What |
|---|---|
| `%APPDATA%\ScreenBridge\config.json` | PC settings and this PC's id |
| `%APPDATA%\ScreenBridge\discovered.json` | Macs heard on the network |
| Mac: `defaults read com.matiasvivancob.screenbridge allowedPCs` | PCs allowed on this Mac |
