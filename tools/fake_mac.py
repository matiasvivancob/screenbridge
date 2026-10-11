"""Fake Mac for testing the Windows app's background link without a real Mac. Prints PASS/FAIL per check.

Usage: python fake_mac.py            (the PC app must be configured with mac_address = 127.0.0.1, see scripts/test-link.ps1)
Speaks docs/PROTOCOL.md on 127.0.0.1:47800 and watches the PC's viewer windows with the Win32 API.
"""
import ctypes
import base64
import json
import os
import socket
import struct
import subprocess
import sys
import time
from ctypes import wintypes

try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)
except Exception:
    pass

failures = 0
cursor_seq = 0


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


def cursor_overlay_checks(c):
    """Exercise cursor side-channel packets against display 0's real viewer."""
    global cursor_seq
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    sender_path = os.path.join(root, "tools", "test_sender.py")
    sender = None
    u = None
    pixel_ok = True
    pixel_skip_printed = False

    user32 = ctypes.windll.user32
    gdi32 = ctypes.windll.gdi32
    user32.GetWindowTextLengthW.argtypes = (wintypes.HWND,)
    user32.GetWindowTextLengthW.restype = ctypes.c_int
    user32.GetWindowTextW.argtypes = (wintypes.HWND, wintypes.LPWSTR, ctypes.c_int)
    user32.SetWindowPos.argtypes = (wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT)
    user32.SetWindowPos.restype = wintypes.BOOL
    user32.GetClientRect.argtypes = (wintypes.HWND, ctypes.POINTER(wintypes.RECT))
    user32.GetClientRect.restype = wintypes.BOOL
    user32.ClientToScreen.argtypes = (wintypes.HWND, ctypes.POINTER(wintypes.POINT))
    user32.ClientToScreen.restype = wintypes.BOOL
    user32.GetDC.argtypes = (wintypes.HWND,)
    user32.GetDC.restype = wintypes.HDC
    user32.ReleaseDC.argtypes = (wintypes.HWND, wintypes.HDC)
    gdi32.GetPixel.argtypes = (wintypes.HDC, ctypes.c_int, ctypes.c_int)
    gdi32.GetPixel.restype = ctypes.c_uint32

    def skip_pixels():
        nonlocal pixel_ok, pixel_skip_printed
        pixel_ok = False
        if not pixel_skip_printed:
            print("SKIP: could not sample screen pixels", flush=True)
            pixel_skip_printed = True

    def find_viewer():
        found = []
        prefix = "ScreenBridge \u00b7 Display 1"

        @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        def cb(hwnd, _):
            n = user32.GetWindowTextLengthW(hwnd)
            if n:
                buf = ctypes.create_unicode_buffer(n + 1)
                user32.GetWindowTextW(hwnd, buf, n + 1)
                if buf.value.startswith(prefix):
                    found.append(hwnd)
            return True

        user32.EnumWindows(cb, 0)
        return found[0] if found else None

    def viewer_alive():
        return find_viewer() is not None

    hwnd = None

    def pixel_at_stream(sx, sy):
        nonlocal pixel_ok
        if not pixel_ok or hwnd is None:
            return None
        rect = wintypes.RECT()
        origin = wintypes.POINT(0, 0)
        if not user32.GetClientRect(hwnd, ctypes.byref(rect)) or not user32.ClientToScreen(hwnd, ctypes.byref(origin)):
            skip_pixels()
            return None
        cw, ch = rect.right - rect.left, rect.bottom - rect.top
        if cw <= 0 or ch <= 0:
            skip_pixels()
            return None
        scale = min(cw / 1280.0, ch / 720.0)
        dw, dh = int(1280 * scale), int(720 * scale)
        dx, dy = (cw - dw) // 2, (ch - dh) // 2
        screen_x = origin.x + dx + sx * scale
        screen_y = origin.y + dy + sy * scale
        hdc = user32.GetDC(wintypes.HWND(0))
        if not hdc:
            skip_pixels()
            return None
        try:
            color = gdi32.GetPixel(hdc, int(screen_x), int(screen_y))
        finally:
            user32.ReleaseDC(wintypes.HWND(0), hdc)
        if color == 0xFFFFFFFF:
            skip_pixels()
            return None
        return color

    def near(color, rgb, tol=24):
        if color is None:
            return False
        if color == 0:
            skip_pixels()
            return False
        actual = (color & 0xFF, (color >> 8) & 0xFF, (color >> 16) & 0xFF)
        return all(abs(actual[i] - rgb[i]) <= tol for i in range(3))

    def check_pixels(name, ok):
        if pixel_ok:
            check(name, ok)
        else:
            check(name + " (pixel sampling skipped; viewer still alive)", viewer_alive())

    def send_shape(shape_id, rgba):
        bgra = bytes(rgba) * (24 * 24)
        c.send({"type": "cursor_shape", "id": shape_id, "w": 24, "h": 24, "hx": 0, "hy": 0,
                "bgra": base64.b64encode(bgra).decode("ascii")})

    def send_state(x, y, shape, flags=3, seq=None):
        global cursor_seq
        if seq is None:
            cursor_seq = (cursor_seq + 1) & 0xFFFF
            seq = cursor_seq
        packet = struct.pack(">IHHB", 0, 0, 0, 0) + bytes((0x02, flags)) + struct.pack(">HhhH", seq, x, y, shape)
        u.sendto(packet, ("127.0.0.1", 47810))

    def show_cursor(x, y, shape, seconds, flags=3):
        end = time.monotonic() + seconds
        while True:
            send_state(x, y, shape, flags)
            remaining = end - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(0.016, remaining))

    try:
        sender = subprocess.Popen(
            [sys.executable, sender_path, "127.0.0.1", "47810", "1280x720", "0", "h264"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        prefix = "ScreenBridge \u00b7 Display 1"
        video_ready = wait_for(lambda: any(t.startswith(prefix) and " fps" in t for t in viewer_titles()), 8)
        check("cursor test video reaches display 0 viewer", video_ready, str(viewer_titles()))

        hwnd = find_viewer()
        if hwnd is not None:
            # HWND_TOPMOST with SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE keeps the test window visible without stealing focus.
            topmost = user32.SetWindowPos(hwnd, wintypes.HWND(-1), 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010)
            if not topmost:
                skip_pixels()
        else:
            skip_pixels()

        u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        u.bind(("127.0.0.1", 0))
        u.setblocking(False)

        send_shape(5, (255, 0, 255, 255))
        send_shape(6, (0, 255, 0, 255))

        # Verify GDI sampling while the video is unobstructed, and avoid a naturally matching test-pattern pixel.
        first_cursor = (200, 150)
        first_center = (212, 162)
        baseline = pixel_at_stream(*first_center)
        if pixel_ok and baseline == 0:
            skip_pixels()
        if pixel_ok and (near(baseline, (255, 0, 255)) or near(baseline, (0, 255, 0))):
            for candidate in ((252, 212), (332, 262), (452, 352), (732, 532)):
                sample = pixel_at_stream(*candidate)
                if sample is not None and not near(sample, (255, 0, 255)) and not near(sample, (0, 255, 0)):
                    first_center = candidate
                    first_cursor = (candidate[0] - 12, candidate[1] - 12)
                    baseline = sample
                    break
            else:
                skip_pixels()

        send_state(first_cursor[0], first_cursor[1], 5)
        received = []
        deadline = time.monotonic() + 3
        saw_p1 = False
        while time.monotonic() < deadline:
            try:
                message, _ = u.recvfrom(1024)
                received.append(message)
                if message == b"P1":
                    saw_p1 = True
                    break
            except BlockingIOError:
                time.sleep(0.01)
        check("viewer asks for the cursor overlay (P1)", saw_p1, repr(received))

        show_cursor(first_cursor[0], first_cursor[1], 5, 0.6)
        color = pixel_at_stream(*first_center)
        label = "cursor at (200,150)" if first_cursor == (200, 150) else f"cursor at alternate position {first_cursor}"
        check_pixels(f"{label}, shape 5 is magenta at its center", near(color, (255, 0, 255)))

        show_cursor(600, 400, 5, 0.6)
        moved = pixel_at_stream(612, 412)
        old = pixel_at_stream(*first_center)
        check_pixels("cursor moves to (600,400) and leaves its old center", near(moved, (255, 0, 255)) and not near(old, (255, 0, 255)))

        show_cursor(600, 400, 5, 0.5, flags=1)
        hidden_off_display = pixel_at_stream(612, 412)
        check_pixels("cursor is hidden when flags=1 (not on this display)", not near(hidden_off_display, (255, 0, 255)))

        show_cursor(600, 400, 5, 0.5, flags=2)
        hidden_invisible = pixel_at_stream(612, 412)
        check_pixels("cursor is hidden when flags=2 (not visible)", not near(hidden_invisible, (255, 0, 255)))

        show_cursor(600, 400, 5, 0.5)
        time.sleep(1.6)
        stale = pixel_at_stream(612, 412)
        check_pixels("cursor hides after 1 second without state packets", not near(stale, (255, 0, 255)))

        send_shape(6, (0, 255, 0, 255))
        show_cursor(400, 300, 6, 0.5)
        changed = pixel_at_stream(412, 312)
        check_pixels("cursor shape change to id 6 renders green", near(changed, (0, 255, 0)))

        show_cursor(400, 300, 6, 0.2)
        older_seq = (cursor_seq - 5) & 0xFFFF
        send_state(900, 500, 6, seq=older_seq)
        old_position_green = False
        center_green = False
        # Leave enough time for a bad older state to reach a presentation before refreshing the current position.
        for _ in range(3):
            old_position_green |= near(pixel_at_stream(912, 512), (0, 255, 0))
            center_green |= near(pixel_at_stream(412, 312), (0, 255, 0))
            time.sleep(0.012)
        end = time.monotonic() + 0.5
        while time.monotonic() < end:
            send_state(400, 300, 6)
            old_position_green |= near(pixel_at_stream(912, 512), (0, 255, 0))
            center_green |= near(pixel_at_stream(412, 312), (0, 255, 0))
            time.sleep(0.016)
        check_pixels("out-of-order state is ignored; cursor stays at (400,300)", not old_position_green and center_green)

        # Malformed side-channel packets must be ignored without taking down the viewer.
        u.sendto(struct.pack(">IHHB", 0, 0, 0, 0) + b"\x02", ("127.0.0.1", 47810))
        u.sendto(struct.pack(">IHHB", 0, 0, 0, 0) + b"\x7f", ("127.0.0.1", 47810))
        c.send({"type": "cursor_shape", "id": 9, "w": 100000, "h": 100000, "hx": 0, "hy": 0, "bgra": "AAAA"})
        c.send({"type": "cursor_shape", "id": 10, "w": 24, "h": 24, "hx": 0, "hy": 0, "bgra": "not-base64!"})
        time.sleep(0.2)
        alive_after_hostile = viewer_alive()
        send_shape(5, (255, 0, 255, 255))
        show_cursor(200, 150, 5, 0.5)
        after_hostile = pixel_at_stream(212, 162)
        check_pixels("hostile cursor input leaves viewer alive and valid cursor rendering works",
                     alive_after_hostile and near(after_hostile, (255, 0, 255)))

        # A missing shape id should leave the most recently resolved image in place.
        show_cursor(400, 300, 6, 0.2)
        show_cursor(400, 300, 77, 0.5)
        missing_shape = pixel_at_stream(412, 312)
        check_pixels("unknown shape id 77 keeps drawing the previous shape", near(missing_shape, (0, 255, 0)))
    finally:
        if u is not None:
            for _ in range(3):
                try:
                    send_state(0, 0, 5, flags=0)
                except OSError:
                    break
                time.sleep(0.02)
            u.close()
        if sender is not None and sender.poll() is None:
            # test_sender.py owns ffmpeg; terminate that process tree so neither test process is left behind.
            subprocess.run(["taskkill", "/PID", str(sender.pid), "/T", "/F"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                sender.wait(timeout=5)
            except subprocess.TimeoutExpired:
                sender.kill()
                sender.wait()


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

    cursor_overlay_checks(c)

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
