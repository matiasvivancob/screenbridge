# Architecture

## Goal

Make a Windows PC's monitors behave like extra displays of a Mac, as cheaply and with as little latency as possible,
without touching how mouse and keyboard are shared.

## Data path

```
Mac
  CGVirtualDisplay ─▶ ScreenCaptureKit (NV12, 60/120 fps) ─▶ VideoToolbox (HW H.264/HEVC) ─▶ UDP chunks
                                                                                              │
PC                                                                                            ▼
  swapchain ◀─ D3D11 video processor ◀─ Media Foundation decoder (DXVA, GPU textures) ◀─ reassembly
  (flip model, tearing allowed)         NV12→BGRA + scale, no CPU copy
```

Per display, one UDP socket on each side and one thread on the PC that does receive → decode → present with no
queues in between: a frame is shown as soon as it is decoded.

## Mac (`mac/Sources`)

| File | Role |
|---|---|
| `main.swift` | `NSApplication` accessory app: status item, menu, approval alert, login item |
| `Agent.swift` | UDP announcements; TCP control server; `Session` per connected PC (approval, add/remove, watchdog); only an approved session becomes the active one |
| `Screen.swift` | One virtual display: create, capture, encode, packetize; applies the viewer's live settings |
| `CGVirtualDisplay.h` | Declarations of the private CoreGraphics classes, imported through the bridging header |

Decisions worth knowing:

- **Frame-counter timestamps.** `VTCompressionSessionEncodeFrame` gets evenly spaced PTS (`n/fps`), not capture times.
  With wall-clock PTS the low-latency encoder held most frames about 12 ms in a benchmark.
- **Encoder rebuilt on codec, fps, speed or rate-control change**; bitrate changes are applied in place.
- **Keyframe on demand.** The viewer asks (`K`) when it loses sync; a static screen produces no frames, so the last
  captured buffer is re-encoded to carry the keyframe.
- **Watchdog.** No message from the PC for 6 s ends the session and the virtual displays go away with the objects.
- **Virtual display identity** uses fixed vendor/product/serial per slot so macOS remembers the arrangement.

## PC (`windows/src`)

| File | Role |
|---|---|
| `main.rs` | Entry: tray (default), `--settings`, `--view PORT N` |
| `tray.rs` | Notification-area icon, menu, autostart, single instance, Explorer-restart recovery |
| `session.rs` | Mac discovery; the link thread: connect, hello, layout, per-display add/remove, ping/pong, reconnect and restore |
| `viewer.rs` | Window + render thread per display: reassembly, decode, present, F10/F11 handling, back-channel settings |
| `video.rs` | Media Foundation decoder wrapper and D3D11 video-processor/swapchain presenter |
| `settings.rs` | egui settings window (separate process, so the tray never loads a GUI toolkit) |
| `config.rs` | Config file, monitor enumeration, discovered Macs |

Decisions worth knowing:

- **GPU all the way.** The decoder outputs D3D11 textures; the video processor converts and scales them straight into the
  swapchain's back buffer. The CPU only touches compressed bytes.
- **The device is created on the adapter that drives the monitor**, so frames never cross GPUs.
- **One long-lived link, short-lived viewers.** Windows, the D3D device and the decoder exist only while a display is on;
  the idle cost is a socket and a 2-second ping.
- **Settings is a separate process** (`screenbridge.exe --settings`): the tray stays tiny and a UI crash can't stop a session.
- **Late/damaged frames are dropped, not waited for**: any gap marks the stream "needs keyframe" and requests one.
- **PC → Mac settings are replayed** whenever the sender's address changes, so a restarted Mac picks up the viewer's choices.

## Protocol

[PROTOCOL.md](PROTOCOL.md). Three channels: UDP discovery broadcast, TCP JSON control, UDP video with a text back channel.

## Security model

Convenience on a trusted LAN, not protection against a hostile one.

- A PC must be allowed once on the Mac (a dialog); the PC identifies itself with a random 128-bit id.
- The id and the video travel unencrypted. Someone on the LAN who sees the id can impersonate that PC; someone who can
  receive the video stream sees the Mac's extended displays.
- The Mac only captures its own virtual displays, never the built-in screen.

Ideas if this ever needs to leave a home network: derive a per-session key from a pairing code and encrypt the video
packets (the header is already fixed-size), and sign the control messages.

## Known limits and ideas

- Encoder latency (11–14 ms on an M1 Pro) dominates. Not reduced by codec, resolution, rate control, speed priority or a keep-warm frame.
  Not explained: synthetic frames encode in ~1.7 ms in a benchmark with the same settings.
- The cursor is baked into the video. Sending it as a separate overlay would remove the encode path from pointer feedback.
- No audio. No clipboard. No input (by design).
- Forward error correction: not needed on wired gigabit in testing; would help on Wi-Fi.
- Signing and notarization (Apple Developer ID, a Windows code-signing certificate) for friction-free installs.
