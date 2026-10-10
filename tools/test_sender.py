"""Test-only sender: streams an ffmpeg test pattern in the screenbridge packet format.
Usage: python test_sender.py <host> <port> [WxH] [loss%] [h264|hevc]
"""
import random, socket, struct, subprocess, sys

host, port = sys.argv[1], int(sys.argv[2])
size = sys.argv[3] if len(sys.argv) > 3 else "2560x1440"
loss = float(sys.argv[4]) / 100 if len(sys.argv) > 4 else 0.0
hevc = len(sys.argv) > 5 and sys.argv[5] == "hevc"
CHUNK = 1200
AUD = b"\x00\x00\x01\x46\x01" if hevc else b"\x00\x00\x01\x09"  # 3-byte start codes match both forms
enc = (["libx265", "-x265-params", "log-level=error:bframes=0:rc-lookahead=0:keyint=600"] if hevc
       else ["libx264", "-profile:v", "baseline", "-g", "600"])

ff = subprocess.Popen(
    ["ffmpeg", "-loglevel", "error", "-re", "-f", "lavfi", "-i", f"testsrc2=size={size}:rate=60",
     "-c:v", *enc, "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", "20M",
     "-bsf:v", ("hevc" if hevc else "h264") + "_metadata=aud=insert", "-f", "hevc" if hevc else "h264", "-"],
    stdout=subprocess.PIPE)
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 << 20)
sock.setblocking(False)
frame_id, buf, keyreqs = random.getrandbits(32), b"", 0

def send(au):
    global frame_id, keyreqs
    nal_types = [(au[i + 3] >> 1) & 0x3F if hevc else au[i + 3] & 0x1F
                 for i in range(len(au) - 3) if au[i:i + 3] == b"\x00\x00\x01"]
    key = any(16 <= t <= 21 for t in nal_types) if hevc else 5 in nal_types
    chunks = [au[i:i + CHUNK] for i in range(0, len(au), CHUNK)]
    for idx, c in enumerate(chunks):
        if random.random() >= loss:
            sock.sendto(struct.pack(">IHHB", frame_id, idx, len(chunks), key | hevc << 1) + c, (host, port))
    frame_id = (frame_id + 1) & 0xFFFFFFFF
    try:
        while (m := sock.recv(16)):
            keyreqs += m == b"K"
            print(f"viewer sent {m!r} (test sender ignores it; keyframes every 600 frames)", flush=True)
    except (BlockingIOError, ConnectionResetError):
        pass

while chunk := ff.stdout.read(65536):
    buf += chunk
    while (start := buf.find(AUD)) != -1 and (end := buf.find(AUD, start + 1)) != -1:
        send(buf[start:end])
        buf = buf[end:]
