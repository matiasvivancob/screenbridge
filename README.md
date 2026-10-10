# screenbridge

The Mac creates virtual displays and streams each one (H.264 or HEVC over UDP) to a view-only window on the PC.
Input stays with Synergy: the viewer never sends mouse or keyboard to the Mac.

## Daily use (PC)
- `windows\start.cmd` starts everything: the Mac sender (opened in the Mac's Terminal over SSH) and both viewers.
- `windows\stop.cmd` closes the viewers and removes the Mac's virtual displays.
- In a viewer: **F11** fullscreen/window, **F10** or **right-click** menu (bitrate, codec, frame rate,
  encoder priority, rate control, refresh, close), **Alt+F4** close. Live stats are in the window title.
- Viewer windows are normal windows (not always on top).
- While extended, lock the Synergy cursor to the Mac (hotkey `switchToScreen(Mac), lockCursorToScreen(on)`).

## Mac
```sh
sh mac/build.sh
./mac/screenbridge <pc-ip> 2560x1440@5001 1920x1080@5002     # optional: --mbps 30
```
`mac/run.command` holds the PC's IP. The first run needs Screen Recording permission for Terminal.

## How it works
- Mac: CGVirtualDisplay (private API) -> ScreenCaptureKit -> VideoToolbox (low-latency rate control) -> UDP.
- PC: Media Foundation hardware decode -> D3D11 video processor -> flip-model swapchain (tearing allowed). Frames stay on the GPU.
- Measured: PC decode ~0.3 ms and present ~1 ms per frame. The Mac's hardware encoder takes ~11-14 ms per real frame on the M1 Pro,
  whatever the codec, rate control, resolution or speed setting. That is now the largest part of the latency.

## Rebuild the viewer
`cd viewer && cargo build --release` (output goes to `%TEMP%\sbt`, see `viewer/.cargo/config.toml`), then copy `viewer.exe` to `windows\`.
`tools/test_sender.py <host> <port> [WxH] [loss%] [h264|hevc]` streams an ffmpeg test pattern to a viewer without the Mac.
