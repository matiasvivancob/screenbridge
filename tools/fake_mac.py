"""Fake Mac for testing the Windows app's background link without a real Mac. Prints PASS/FAIL per check.

Usage: python fake_mac.py            (the PC app must be configured with mac_address = 127.0.0.1, see scripts/test-link.ps1)
Speaks docs/PROTOCOL.md on 127.0.0.1:47800 and watches the PC's viewer windows with the Win32 API.
"""
import ctypes
import json
import socket
import struct
import sys
import time
from ctypes import wintypes

failures = 0


def check(name, ok, detail=""):
    global failures
    failures += 0 if ok else 1
    print(("PASS  " if ok else "FAIL  ") + name + (f"   [{detail}]" if detail and not ok else ""), flush=True)


def viewer_titles():
    """Titles of the PC app's viewer windows ("ScreenBridge · Display N ...")."""
    found = []
    user32 = ctypes.windll.user32

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def cb(hwnd, _):
        n = user32.GetWindowTextLengthW(hwnd)
        if n:
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(hwnd, buf, n + 1)
            if buf.value.startswith("ScreenBridge · Display"):
                found.append(buf.value)
        return True

    user32.EnumWindows(cb, 0)
    return found


def has_display(n):
    return any(t.startswith(f"ScreenBridge · Display {n}") for t in viewer_titles())


def wait_for(pred, seconds):
    end = time.time() + seconds
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.1)
    return pred()


class Conn:
    def __init__(self, sock):
        self.s = sock
        self.buf = b""
        self.s.settimeout(0.2)
        self.pong_enabled = True  # the real Mac answers every ping; turn off to simulate a Mac that went silent

    def send(self, o):
        self.s.sendall((json.dumps(o) + "\n").encode())

    def poll(self):
        """One message, or None. Raises ConnectionError when the PC closed the connection."""
        while b"\n" not in self.buf:
            try:
                d = self.s.recv(4096)
            except socket.timeout:
                return None
            except OSError as e:
                raise ConnectionError(str(e))
            if not d:
                raise ConnectionError("closed")
            self.buf += d
        line, self.buf = self.buf.split(b"\n", 1)
        m = json.loads(line)
        if m.get("type") == "ping" and self.pong_enabled:
            self.send({"type": "pong"})
        return m

    def wait_for_msg(self, pred, seconds):
        end = time.time() + seconds
        seen = []
        while time.time() < end:
            m = self.poll()
            if m is None:
                continue
            seen.append(m)
            if pred(m):
                return m, seen
        return None, seen

    def drain(self, seconds):
        end = time.time() + seconds
        out = []
        while time.time() < end:
            m = self.poll()
            if m is not None:
                out.append(m)
        return out

    def close(self):
        try:
            self.s.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.s.close()


def accept(srv, seconds):
    srv.settimeout(seconds)
    try:
        c, _ = srv.accept()
        return Conn(c)
    except socket.timeout:
        return None


def handshake(srv, seconds=25):
    c = accept(srv, seconds)
    if c is None:
        return None, None
    hello, _ = c.wait_for_msg(lambda m: m.get("type") == "hello", 3)
    if hello:
        c.send({"type": "welcome", "name": "FakeMac"})
    return c, hello


def main():
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 47800))
    srv.listen(2)
    print("fake Mac listening on 127.0.0.1:47800", flush=True)

    c, hello = handshake(srv)
    check("PC connects and says hello", hello is not None and hello.get("version") == 1)
    if not hello:
        return
    check("hello carries a pc_id of 16+ chars", len(hello.get("pc_id", "")) >= 16)

    layout, seen = c.wait_for_msg(lambda m: m.get("type") == "layout", 3)
    check("PC sends its layout after welcome", layout is not None, str(seen))
    ids = [d["id"] for d in (layout or {}).get("displays", [])]
    check("layout lists both configured displays", ids == [0, 1], str(ids))

    quiet = [m for m in c.drain(2.0) if m.get("type") == "add"]
    check("no display turns on by itself (auto_on is off)", not quiet and not viewer_titles(), str(viewer_titles()))

    # The Mac menu asks the PC to turn display 0 on.
    c.send({"type": "toggle", "id": 0, "on": True})
    add, seen = c.wait_for_msg(lambda m: m.get("type") == "add", 4)
    check("toggle on -> PC asks the Mac to add display 0", bool(add) and add["display"]["id"] == 0, str(seen))
    check("add carries size, port and settings", bool(add) and add["display"]["port"] == 47810 and add["display"]["width"] == 1280 and add.get("settings", {}).get("codec") == "h264", str(add))
    check("viewer window for display 0 is open", wait_for(lambda: has_display(1), 3))
    c.send({"type": "state", "on": [0]})

    c.send({"type": "toggle", "id": 1, "on": True})
    add1, _ = c.wait_for_msg(lambda m: m.get("type") == "add" and m["display"]["id"] == 1, 4)
    check("display 1 turns on independently", bool(add1) and add1["display"]["port"] == 47811)
    check("both viewer windows are open", wait_for(lambda: has_display(1) and has_display(2), 3), str(viewer_titles()))
    c.send({"type": "state", "on": [0, 1]})

    c.send({"type": "toggle", "id": 0, "on": False})
    rem, _ = c.wait_for_msg(lambda m: m.get("type") == "remove", 4)
    check("toggle off -> PC sends remove for display 0", bool(rem) and rem["id"] == 0)
    check("display 0 window closed, display 1 still open", wait_for(lambda: not has_display(1) and has_display(2), 3), str(viewer_titles()))
    c.send({"type": "state", "on": [1]})

    # Stale state: the Mac says display 1 does not exist (capture failed). The PC must close its viewer.
    c.send({"type": "state", "on": []})
    check("a viewer the Mac doesn't have is closed", wait_for(lambda: not viewer_titles(), 6), str(viewer_titles()))
    # ...and turn it back on for the reconnect test.
    c.send({"type": "toggle", "id": 1, "on": True})
    c.wait_for_msg(lambda m: m.get("type") == "add", 4)
    c.send({"type": "state", "on": [1]})
    check("display 1 back on", wait_for(lambda: has_display(2), 3))

    # A message split by a pause longer than the PC's read timeout must arrive whole.
    c.s.sendall(b'{"type":"toggle","id":1,')
    time.sleep(0.6)
    c.s.sendall(b'"on":false}\n')
    rem1, _ = c.wait_for_msg(lambda m: m.get("type") == "remove" and m.get("id") == 1, 4)
    check("TCP message split across a read timeout is not lost", bool(rem1))
    c.send({"type": "state", "on": []})
    c.send({"type": "toggle", "id": 1, "on": True})
    c.wait_for_msg(lambda m: m.get("type") == "add", 4)
    c.send({"type": "state", "on": [1]})
    check("display 1 on again", wait_for(lambda: has_display(2), 3))

    # Hostile datagrams at the viewer's port: oversized, and a frame claiming 65535 chunks. Must be ignored.
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.sendto(b"\x00" * 60000, ("127.0.0.1", 47811))
    u.sendto(struct.pack(">IHHB", 1, 0, 65535, 1) + b"x" * 1200, ("127.0.0.1", 47811))
    u.sendto(struct.pack(">IHHB", 2, 0, 9000, 1) + b"x" * 100, ("127.0.0.1", 47811))
    time.sleep(0.5)
    check("hostile UDP datagrams are ignored, viewer survives", has_display(2))
    u.close()

    # Link loss without goodbye: the PC must reconnect by itself and restore display 1.
    c.close()
    t0 = time.time()
    c2, hello2 = handshake(srv, 20)
    check("PC reconnects after the link drops", hello2 is not None, f"after {time.time() - t0:.1f}s")
    if c2:
        check("reconnect took under 10 s", time.time() - t0 < 10, f"{time.time() - t0:.1f}s")
        add_r, seen = c2.wait_for_msg(lambda m: m.get("type") == "add", 6)
        check("display 1 is restored after reconnect", bool(add_r) and add_r["display"]["id"] == 1, str(seen))
        check("display 0 stays off", not has_display(1))
        c2.send({"type": "state", "on": [1]})

        # The Mac stops answering pings (asleep, cable pulled, no RST): the PC must notice and reconnect.
        c2.pong_enabled = False
        t1 = time.time()
        dropped = False
        try:
            while time.time() - t1 < 15:
                c2.poll()
        except ConnectionError:
            dropped = True
        check("PC drops a Mac that stopped answering", dropped, f"{time.time() - t1:.1f}s")
        c2.close()
        c3, hello3 = handshake(srv, 20)
        check("PC reconnects after a silent Mac", hello3 is not None)
        if c3:
            c2 = c3  # carry on with the 'bye' check on the new connection
            add_s, _ = c3.wait_for_msg(lambda m: m.get("type") == "add", 6)
            check("display 1 restored after the silent-Mac reconnect", bool(add_s))
            c3.send({"type": "state", "on": [1]})

        # The Mac user chooses Disconnect: the PC must stay disconnected.
        c2.send({"type": "bye"})
        gone = False
        try:
            c2.drain(2)
        except ConnectionError:
            gone = True
        check("viewers close when the Mac disconnects", wait_for(lambda: not viewer_titles(), 4), str(viewer_titles()))
        c4 = accept(srv, 9)
        check("PC does not reconnect after 'bye'", c4 is None)
        if c4:
            c4.close()
        c2.close()

    srv.close()
    print(f"\n{'ALL PASSED' if failures == 0 else str(failures) + ' FAILED'}", flush=True)


if __name__ == "__main__":
    main()
    sys.exit(1 if failures else 0)
