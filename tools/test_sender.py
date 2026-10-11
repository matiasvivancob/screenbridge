"""Test-only sender: streams an ffmpeg test pattern in the screenbridge packet format (no Mac needed).
Usage: python test_sender.py <host> <port> [WxH] [loss%] [h264|hevc|av1|vp9]
View it with:  screenbridge.exe --view <port> <monitor#> --windowed      (SB_VERBOSE=1 prints statistics)
"""
import random, socket, struct, subprocess, sys, time

host, port = sys.argv[1], int(sys.argv[2])
size = sys.argv[3] if len(sys.argv) > 3 else "2560x1440"
loss = float(sys.argv[4]) / 100 if len(sys.argv) > 4 else 0.0
codec = sys.argv[5] if len(sys.argv) > 5 else "h264"
CODEC_ID = {"h264": 0, "hevc": 1, "av1": 2, "vp9": 3}[codec]
CHUNK = 1200
KEY_EVERY = 60  # the encoder is told -g 60; frame 0 and every 60th frame are keyframes (the test sender cannot honour K requests)

ENC = {
    "h264": ["-c:v", "libx264", "-profile:v", "baseline", "-g", str(KEY_EVERY), "-preset", "ultrafast", "-tune", "zerolatency",
             "-bsf:v", "h264_metadata=aud=insert", "-f", "h264"],
    "hevc": ["-c:v", "libx265", "-x265-params", f"log-level=error:bframes=0:rc-lookahead=0:keyint={KEY_EVERY}",
             "-preset", "ultrafast", "-tune", "zerolatency", "-bsf:v", "hevc_metadata=aud=insert", "-f", "hevc"],
    "av1": ["-c:v", "libsvtav1", "-preset", "12", "-g", str(KEY_EVERY), "-f", "ivf"],
    "vp9": ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-lag-in-frames", "0", "-g", str(KEY_EVERY), "-f", "ivf"],
}[codec]

ff = subprocess.Popen(
    ["ffmpeg", "-loglevel", "error", "-re", "-f", "lavfi", "-i", f"testsrc2=size={size}:rate=60", "-b:v", "20M", *ENC, "-"],
    stdout=subprocess.PIPE)
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 << 20)
sock.setblocking(False)
frame_id, keyreqs = random.getrandbits(32), 0


def send(au, key):
    global frame_id
    chunks = [au[i:i + CHUNK] for i in range(0, len(au), CHUNK)]
    for idx, c in enumerate(chunks):
        if idx % 32 == 31:
            time.sleep(0.0003)  # pace big keyframes like the real sender does instead of bursting megabytes at once
        if random.random() >= loss:
            sock.sendto(struct.pack(">IHHB", frame_id, idx, len(chunks), int(key) | CODEC_ID << 1) + c, (host, port))
    frame_id = (frame_id + 1) & 0xFFFFFFFF
    try:
        while (m := sock.recv(16)):
            print(f"viewer sent {m!r} (test sender ignores it; keyframes every {KEY_EVERY} frames)", flush=True)
    except (BlockingIOError, ConnectionResetError):
        pass


def read_exact(n):
    b = b""
    while len(b) < n:
        c = ff.stdout.read(n - len(b))
        if not c:
            return None
        b += c
    return b


if codec in ("av1", "vp9"):
    # IVF: 32-byte header, then [u32 size][u64 pts][payload] per frame; each frame is one temporal unit / VP9 frame.
    if read_exact(32) is None:
        sys.exit("ffmpeg produced nothing (is libsvtav1 / libvpx available?)")
    n = 0
    while (h := read_exact(12)) is not None:
        payload = read_exact(struct.unpack("<I", h[:4])[0])
        if payload is None:
            break
        send(payload, n % KEY_EVERY == 0)
        n += 1
else:
    # Annex-B with access-unit delimiters: split on the AUD that starts every access unit.
    AUD = b"\x00\x00\x01\x46\x01" if codec == "hevc" else b"\x00\x00\x01\x09"
    buf, n = b"", 0
    while chunk := ff.stdout.read(65536):
        buf += chunk
        while (start := buf.find(AUD)) != -1 and (end := buf.find(AUD, start + 1)) != -1:
            au = buf[start:end]
            # a leading zero byte belongs to the 4-byte start code of the next unit
            send(au, n % KEY_EVERY == 0)
            buf = buf[end:]
            n += 1
