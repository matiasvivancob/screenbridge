# Using ScreenBridge

## The tray icon (PC)

| Action | Result |
|---|---|
| Double-click | Extend, or stop if already extending |
| Right-click / click | Menu: status, **Extend displays / Stop extending**, **Settings…**, **Start with Windows**, **Quit** |
| Hover | Current status |

A balloon appears when you need to act (click Allow on the Mac) or when extending fails, with the reason.

## The menu-bar icon (Mac)

Status line, **Stop Extending**, **Allowed PCs** (remove a PC to make it ask again), **Open at Login**, **Quit**.
The icon shows two displays while a PC is connected.

## In a viewer window

| Key / action | Result |
|---|---|
| **F11** | Fullscreen ↔ window |
| **F10** or right-click | Settings menu, applied live: bitrate, codec, frame rate, encoder priority, rate control, refresh picture, close |
| **Alt+F4** | Close this viewer. Closing the last one stops the session. |

Viewers are ordinary windows (not always on top). Mouse and keyboard input in them is ignored.

The title bar shows live numbers, for example `60 fps · Mac encode 12.8 ms (max 19.8) · decode 0.3 ms · present 0.8 ms · 31.2 Mbps · H.264`.
`N lost` means that many frames were damaged and the stream re-synced.

## Settings (tray → Settings…)

| Setting | Notes |
|---|---|
| **Mac** | *Automatic* picks the first Mac found. Choose a specific one if you have several. *Manual address* is for a Mac that can't be discovered (another subnet, VPN): `192.168.1.104` or `host:47800`. |
| **Displays** | One row per virtual display: on/off, size, which PC monitor shows it, start fullscreen. Up to 4. *Match monitor* sets the size to that monitor's native resolution, which gives the sharpest text. |
| **Codec** | H.264 works everywhere. HEVC gives better quality per Mbps but needs the HEVC Video Extensions on the PC. |
| **Bitrate** | 20–40 Mbps is plenty on wired gigabit for a 1440p desktop. |
| **Frame rate** | 60 is right for most monitors. 120 halves the capture wait and looks smoother on a 120/144 Hz monitor, at more GPU cost on the Mac. |
| **Prioritize encoding speed** | Trades some quality for lower encode time. |
| **Apple low-latency rate control** | Leave on unless a particular Mac encodes slowly with it. |

Saved settings apply the next time you extend. The F10 menu changes the running session immediately but isn't saved.

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
