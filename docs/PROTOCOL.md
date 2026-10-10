# ScreenBridge protocol (version 1)

Three channels, all on the local network. Nothing here is encrypted: use it on networks you trust.

| Channel | Transport | Direction | Port |
|---|---|---|---|
| Discovery | UDP broadcast | Mac → everyone | 47801 |
| Control | TCP, newline-delimited JSON | PC → Mac | 47800 |
| Video + per-display settings | UDP | Mac → PC (and PC → Mac replies on the same socket) | PC picks, default 47810, 47811, ... |

## Discovery
Every 2 seconds the Mac app broadcasts to `255.255.255.255:47801`:
```json
{"type":"announce","name":"Matías's MacBook Pro","port":47800,"version":1}
```
The PC lists every Mac it heard from in the last 10 seconds. Users can also type an IP address (for other subnets or VPNs).

## Control (TCP 47800 on the Mac)
One JSON object per line, UTF-8. One session at a time: a new session replaces the old one.

PC → Mac:
```json
{"type":"hello","pc_id":"<random uuid, stored on the PC>","pc_name":"MATIAS-PC","version":1}
{"type":"extend","displays":[{"id":0,"width":2560,"height":1440,"port":47810}],
 "settings":{"codec":"h264","mbps":30,"fps":60,"speed":false,"low_latency_rc":true}}
{"type":"remove","id":0}
{"type":"stop"}
{"type":"ping"}
```
Mac → PC:
```json
{"type":"pending"}                      // waiting for the user to click Allow on the Mac
{"type":"welcome","name":"Matías's MacBook Pro"}
{"type":"denied"}                       // then the Mac closes the connection
{"type":"extended","count":2}
{"type":"error","message":"Screen Recording permission is missing"}
```
- The first `hello` from an unknown `pc_id` shows an Allow/Deny alert on the Mac. Allowed ids are remembered and can be revoked from the menu bar.
  `pc_id` is a bearer secret: anyone who learns it on the LAN can impersonate that PC.
- Video goes to the TCP peer's IP address at each display's `port`.
- The PC sends `ping` every 2 seconds. If the Mac hears nothing for 6 seconds, or the connection closes, it removes all virtual displays.

## Video (UDP)
One socket per display, Mac → `pc_ip:port`. Each packet is a 9-byte header and then a payload:

| Bytes | Field |
|---|---|
| 0–3 | frame id, u32 big-endian, +1 per frame |
| 4–5 | chunk index, u16 BE |
| 6–7 | chunk count, u16 BE (**0 = status text**, payload is UTF-8, e.g. `Mac encode 1.8 ms (max 12.0)`) |
| 8 | flags: bit0 keyframe, bit1 HEVC (else H.264) |

The payload is a chunk (≤1200 bytes) of one Annex-B access unit. Keyframes carry their parameter sets (SPS/PPS, plus VPS for HEVC).

The PC sends short ASCII messages back to the packet's source address:

| Message | Meaning |
|---|---|
| `K` | send a keyframe (decoder lost sync or the user chose Refresh) |
| `B<mbps>` | bitrate, 1–200 |
| `C264` / `C265` | codec |
| `F60` / `F120` | frame rate |
| `S0` / `S1` | prioritize encoding speed over quality |
| `L0` / `L1` | Apple low-latency rate control |
