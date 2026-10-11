# ScreenBridge protocol (version 1)

Three channels, all on the local network. Nothing here is encrypted: use it on networks you trust.

| Channel | Transport | Direction | Port |
|---|---|---|---|
| Discovery | UDP broadcast | Mac → everyone | 47801 |
| Control | TCP, newline-delimited JSON | PC ⇄ Mac | 47800 (on the Mac) |
| Video + per-display settings | UDP | Mac → PC (and PC → Mac replies on the same socket) | PC listens on 47810 + display id |

## Discovery
Every 2 seconds the Mac app broadcasts to `255.255.255.255:47801`:
```json
{"type":"announce","name":"Matías's MacBook Pro","port":47800,"version":1}
```
The PC lists every Mac it heard from in the last 10 seconds. Users can also type an IP address (other subnets, VPNs).

## Control (TCP 47800 on the Mac)
One JSON object per line, UTF-8, at most 1 MiB per line. The PC connects once and **keeps the link up** with
no displays on; displays are then turned on and off one at a time. One linked PC at a time: a second PC
replaces the first only after the Mac has approved it.

### PC → Mac
| Message | Meaning |
|---|---|
| `{"type":"hello","pc_id":"<32 hex>","pc_name":"MATIAS-PC","version":1}` | first message; `pc_id` is this PC's random identity |
| `{"type":"layout","displays":[{"id":0,"name":"Display 1 (PC monitor 2)","width":2560,"height":1440}]}` | the displays the PC offers; sent after `welcome` and whenever settings change. Lets the Mac menu list them. |
| `{"type":"add","display":{"id":0,"width":2560,"height":1440,"port":47810},"settings":{"codec":"h264","mbps":30,"fps":60,"speed":false,"low_latency_rc":true}}` | turn display `id` on. The viewer is already listening on UDP `port`; the Mac streams to the TCP peer's address. `id` is 0–7. |
| `{"type":"remove","id":0}` | turn display `id` off |
| `{"type":"ping"}` | every 2 s |
| `{"type":"stop"}` | disconnect; the Mac removes everything |

### Mac → PC
| Message | Meaning |
|---|---|
| `{"type":"pending"}` | waiting for the user to click Allow on the Mac |
| `{"type":"welcome","name":"Matías's MacBook Pro"}` | approved; send `layout` |
| `{"type":"denied"}` | not allowed; the Mac then closes the connection |
| `{"type":"state","on":[0,1]}` | which displays exist on the Mac now, after every add/remove/failure. The PC closes a viewer whose display is missing (after a 3 s grace for an `add` in flight). |
| `{"type":"toggle","id":0,"on":true}` | the Mac user asked to turn display `id` on/off from the menu bar; the PC opens/closes the viewer and answers with `add`/`remove` |
| `{"type":"error","message":"..."}` | something failed (permission missing, capture or encoder failed); shown to the user |
| `{"type":"pong"}` | answer to every `ping` |
| `{"type":"bye"}` | the Mac user disconnected; the PC must not reconnect by itself |

### Rules
- The first `hello` from an unknown `pc_id` shows an Allow/Deny alert on the Mac. Allowed ids are remembered; removing one
  from the menu bar also disconnects that PC if it is linked. `pc_id` is a bearer secret: anyone who learns it on the LAN
  can impersonate that PC.
- A connection that has not said `hello` within 10 seconds is dropped, so it can't tie up the Mac.
- The Mac drops the PC if it hears nothing for 6 seconds; the PC drops the Mac after 8 seconds without any message.
  The PC then reconnects every few seconds and restores the displays that were on, unless the Mac said `bye` or the user disconnected.
- A display's identity on the Mac is its `id` (vendor/product/serial are derived from it), so macOS remembers where the user put it.

## Video (UDP)
One socket per display, Mac → `pc_ip:port`. Each datagram is a 9-byte header and a payload of at most 1200 bytes
(receivers ignore datagrams larger than 1509 bytes, and frames of more than 8192 chunks):

| Bytes | Field |
|---|---|
| 0–3 | frame id, u32 big-endian, +1 per frame |
| 4–5 | chunk index, u16 BE |
| 6–7 | chunk count, u16 BE (**0 = status text**, payload is UTF-8, e.g. `Mac encode 1.8 ms (max 12.0)`) |
| 8 | flags: bit0 keyframe, bit1 HEVC (else H.264) |

The payload is a chunk of one Annex-B access unit. Keyframes carry their parameter sets (SPS/PPS, plus VPS for HEVC).
The viewer accepts datagrams only from the Mac's address, abandons a frame that is not complete 100 ms after its first
chunk, and drops its whole backlog and resyncs on the next keyframe if handling a packet ever stalls for 80 ms.

The PC sends short ASCII messages back to the packet's source address (and repeats all of its current settings every 2 s,
because UDP has no acknowledgement):

| Message | Meaning |
|---|---|
| `K` | send a keyframe (decoder lost sync or the user chose Refresh) |
| `B<mbps>` | bitrate, 1–200 |
| `C264` / `C265` | codec |
| `F60` / `F120` | frame rate |
| `S0` / `S1` | prioritize encoding speed over quality |
| `L0` / `L1` | Apple low-latency rate control |
