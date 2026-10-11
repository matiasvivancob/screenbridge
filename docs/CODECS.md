# Video codecs

ScreenBridge streams each display with one of four codecs. You can switch live from the viewer's F10 menu or set the
starting codec in Settings. The Mac encodes, the PC decodes in hardware (Media Foundation / DXVA) in every case.

| Codec | Encoded on the Mac by | Mac cost (1440p60) | Needs on the PC | Use it when |
|---|---|---|---|---|
| **H.264** (default) | Media engine (VideoToolbox) | ~0.5 core | nothing | it works everywhere; start here |
| **HEVC** | Media engine (VideoToolbox) | ~0.5 core | *HEVC Video Extensions* (Store) | you want better quality at the same bitrate |
| **AV1** | CPU, SVT-AV1 low-delay CBR | ~2.7 cores | *AV1 Video Extension* (Store) + a GPU with AV1 decode (RTX 30/40, RX 6000+, Arc) | heavy motion on two displays; best quality per Mbps |
| **VP9** | CPU, libvpx realtime | ~3.7 cores | *VP9 Video Extensions* (Store) | AV1 isn't available on your PC |

AV1 and VP9 are optional parts of the Mac app: they need the SVT-AV1 and libvpx libraries, which the official build
bundles for Apple Silicon (they are loaded at run time, so the app starts normally even where they can't load, and
falls back to H.264). A Mac build made without Homebrew's `svt-av1` and `libvpx` offers H.264 and HEVC only.

## Why the software codecs are interesting

The Mac's media engine is a single fixed-function block. On an M1 Pro it encodes about **72 frames per second at 1440p**
and about 99 at 1080p, no matter how it is configured, and the two displays share it. With heavy motion on both
displays that is not enough for 60 fps each. Frames the hardware encoder holds also take 11–14 ms.
The CPU codecs have no such ceiling and, on desktop content, finish a frame faster:

Measured on a MacBook Pro M1 Pro (6 performance + 2 efficiency cores), 8-bit 4:2:0, low-latency settings:

| 1440p60, synthetic **high-motion** pattern | Throughput | CPU |
|---|---|---|
| H.264, media engine | 72 fps | 0.5 core |
| SVT-AV1 preset 12, low-delay CBR (`rtc`) | 130 fps (2.2× real time) | 2.7 cores |
| libvpx VP9, realtime, cpu-used 8 | 169 fps (2.8× real time) | 3.7 cores |

| 1440p, **desktop-like** content (static background, text, one moving element) | Per-frame encode p50 | p95 | Keyframe |
|---|---|---|---|
| AV1 | 2.9 ms | 12.6 ms | 324 KB |
| VP9 | 4.5 ms | 5.6 ms | 143 KB |
| H.264, media engine (measured in the live pipeline) | ~12 ms | | |

Decode on the PC (RTX 3090 and RTX 4070 Ti SUPER, Media Foundation, GPU textures): **0.1–0.3 ms per frame** for all four.

The CPU cost is real: both displays in AV1 use roughly 4 of the M1 Pro's 6 performance cores. On a MacBook that
also means more heat and battery use. If the Mac is busy with something else (builds, renders), stay on H.264.

## What was evaluated and not built

- **PyroWave** (Themaister, MIT): an intra-only GPU wavelet codec built for local streaming, roughly 200+ Mbps per
  1440p60 stream, with encode/decode far below a millisecond reported by its author. It would remove the Mac's encoder
  latency almost entirely. Not built because: the PC side needs a Vulkan compute decoder (not Media Foundation) and
  presentation through Vulkan or D3D11 interop; the Mac side needs its Metal port; two 1440p60 streams are about
  520–660 Mbps of a gigabit link; and nothing has been measured on this hardware. It is the most promising next step
  if latency matters more than anything else, and it is a multi-day integration with real risk, not a setting.
- **ProRes (Proxy)**: hardware-encoded on the M1 Pro, but no hardware decode on the PC, and roughly 250 Mbps.
- **MJPEG / JPEG XS / raw**: bandwidth or CPU cost without a clear benefit over the codecs above.
- **Software x264/x265 on the Mac**: slower per quality than AV1/VP9 at these settings, no advantage over the media engine.

## Wire format

The codec is a 3-bit field in the video packet header (see [PROTOCOL.md](PROTOCOL.md)): 0 H.264, 1 HEVC, 2 AV1, 3 VP9.
H.264/HEVC are Annex-B; AV1 is one temporal unit (low-overhead OBUs including the temporal delimiter) per frame; VP9 is one
frame (or superframe) per packet. Keyframes are requested on demand; none of the codecs uses periodic keyframes.

## Reproducing the numbers

```sh
sh tools/bench_encoders.sh                                   # ffmpeg throughput on this Mac
clang -O2 -DSB_HAVE_SOFTENC -I/opt/homebrew/include mac/Sources/softenc.c tools/softenc_test.c -o /tmp/softenc_test
/tmp/softenc_test                                            # per-frame latency through the app's own encoder shim
```

`tools/replay_ivf.py` replays the streams the second command writes into a viewer
(`screenbridge.exe --view 47899 0 --windowed`), which checks the Mac encoders' output against the PC's decoders.
