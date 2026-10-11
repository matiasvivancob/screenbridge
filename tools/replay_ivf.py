"""Replays an IVF file (written by tools/softenc_test.c on the Mac) into a viewer in the screenbridge packet format.
Usage: python replay_ivf.py <file.ivf> <host> <port> <av1|vp9> [seconds]
Keyframes come from <file>.keys. Loops the file, re-marking frame 0 as the keyframe each time."""
import socket, struct, sys, time

path, host, port, codec = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
seconds = float(sys.argv[5]) if len(sys.argv) > 5 else 10
cid = {"h264": 0, "hevc": 1, "av1": 2, "vp9": 3}[codec]
data = open(path, "rb").read()
keys = {int(x) for x in open(path + ".keys").read().split()}
frames, pos = [], 32
while pos + 12 <= len(data):
    n = struct.unpack_from("<I", data, pos)[0]
    frames.append(data[pos + 12:pos + 12 + n])
    pos += 12 + n
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
frame_id, t0, sent = 1000, time.time(), 0
while time.time() - t0 < seconds:
    for i, fr in enumerate(frames):
        key = i in keys
        chunks = [fr[o:o + 1200] for o in range(0, len(fr), 1200)]
        for idx, c in enumerate(chunks):
            if idx % 32 == 31:
                time.sleep(0.0003)
            sock.sendto(struct.pack(">IHHB", frame_id, idx, len(chunks), int(key) | cid << 1) + c, (host, port))
        frame_id = (frame_id + 1) & 0xFFFFFFFF
        sent += 1
        wait = t0 + sent / 60.0 - time.time()
        if wait > 0:
            time.sleep(wait)
        if time.time() - t0 >= seconds:
            break
print(f"sent {sent} frames")
