#!/usr/bin/env python3
"""Live acceptance harness for Buffer RPC M3 (T-04950).

Drives a running `zed` (launched with ZED_BUFFER_RPC_SOCK) over the UDS:
  test 7 — subscribe on conn A; conn B issues insertion/deletion/astral edits;
           apply each didChange (in array order) to A's held text and assert it
           EXACTLY equals a fresh buffer/text.
  test 8 — conn A subscribes and STOPS reading; conn B floods edits until the
           server drops+closes A (MAX_OUTBOUND_QUEUED); conn C ping still ok.
  anchor — create on a position, edit before it, resolve → MOVED position.
"""
import json
import os
import socket
import sys
import time

SOCK = os.environ.get("ZED_BUFFER_RPC_SOCK", "/tmp/bufrpc.sock")


class Conn:
    def __init__(self, name):
        self.name = name
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(SOCK)
        self.buf = b""
        self._id = 0

    def send(self, method, params=None):
        self._id += 1
        msg = {"jsonrpc": "2.0", "id": self._id, "method": method}
        if params is not None:
            msg["params"] = params
        body = json.dumps(msg).encode()
        self.s.sendall(b"Content-Length: %d\r\n\r\n%s" % (len(body), body))
        return self._id

    def _read_frame(self, timeout=5.0):
        self.s.settimeout(timeout)
        while True:
            sep = self.buf.find(b"\r\n\r\n")
            if sep != -1:
                header = self.buf[:sep].decode()
                length = None
                for line in header.split("\r\n"):
                    if line.lower().startswith("content-length:"):
                        length = int(line.split(":", 1)[1].strip())
                body_start = sep + 4
                if length is not None and len(self.buf) >= body_start + length:
                    body = self.buf[body_start:body_start + length]
                    self.buf = self.buf[body_start + length:]
                    return json.loads(body)
            try:
                chunk = self.s.recv(65536)
            except socket.timeout:
                return None
            if not chunk:
                return None
            self.buf += chunk

    def request(self, method, params=None, timeout=5.0):
        rid = self.send(method, params)
        while True:
            msg = self._read_frame(timeout)
            if msg is None:
                raise TimeoutError(f"{self.name}: no response to {method}")
            if msg.get("id") == rid:
                return msg

    def next_notification(self, method, timeout=5.0):
        while True:
            msg = self._read_frame(timeout)
            if msg is None:
                return None
            if msg.get("method") == method:
                return msg

    def close(self):
        self.s.close()


def apply_changes(text, changes):
    """Apply WireChanges (descending by start) in array order against `text`."""
    units = utf16_units(text)
    for ch in changes:
        s = utf16_to_byte(text, ch["range"]["start"])
        e = utf16_to_byte(text, ch["range"]["end"])
        text = text[:s] + ch["newText"] + text[e:]
    return text


def utf16_units(text):
    return text.encode("utf-16-le")


def utf16_to_byte(text, pos):
    """Convert a {line,character} (UTF-16) wire position to a Python str index."""
    line = pos["line"]
    character = pos["character"]
    # Find start of the target line.
    cur_line = 0
    idx = 0
    while cur_line < line:
        nl = text.index("\n", idx)
        idx = nl + 1
        cur_line += 1
    # Walk UTF-16 code units within the line.
    units = 0
    i = idx
    while units < character and i < len(text):
        cp = ord(text[i])
        units += 2 if cp > 0xFFFF else 1
        i += 1
    return i


def fresh_text(ctrl, ws, buffer_id):
    r = ctrl.request("buffer/text", {"bufferId": buffer_id})
    return r["result"]["text"]


def main():
    results = []
    a = Conn("A")
    b = Conn("B")
    ctrl = Conn("CTRL")

    for c in (a, b, ctrl):
        r = c.request("initialize", {"clientName": "live-test"})
        assert "result" in r, r

    ws = ctrl.request("workspace/list")["result"]
    print("workspaces:", json.dumps(ws))
    assert ws, "no workspaces"
    workspace_id = ws[0]["workspaceId"]

    path = os.environ["BUFRPC_FILE"]
    opened = ctrl.request("buffer/open", {"workspace": workspace_id, "path": path})
    print("buffer/open:", json.dumps(opened))
    buffer_id = opened["result"]["bufferId"]

    # ── test 7 ───────────────────────────────────────────────────────────────
    sub = a.request("buffer/subscribe", {"bufferId": buffer_id})
    held = sub["result"]["text"]
    print(f"\n[test7] baseline version={sub['result']['version']} text={held!r}")

    def do_edit(label, edits):
        nonlocal held
        b.request("buffer/edit", {"bufferId": buffer_id, "edits": edits})
        note = a.next_notification("buffer/didChange", timeout=5.0)
        assert note is not None, f"{label}: no didChange"
        changes = note["params"]["changes"]
        before = held
        held = apply_changes(held, changes)
        server = fresh_text(ctrl, workspace_id, buffer_id)
        ok = held == server
        print(f"[test7:{label}] isLocal={note['params']['isLocal']} "
              f"changes={json.dumps(changes)}")
        print(f"   before={before!r}\n   after ={held!r}\n   server={server!r} EXACT={ok}")
        results.append((f"test7:{label}", ok))
        return ok

    # (i) insertion at byte 6
    do_edit("insertion", [{"range": {"start": {"line": 0, "character": 0, "offset": 6},
                                     "end": {"line": 0, "character": 0, "offset": 6}},
                           "newText": "beautiful "}])
    # (ii) deletion: delete "world" — recompute its byte offsets from current held text
    w = held.index("world")
    do_edit("deletion", [{"range": {"start": {"line": 0, "character": 0, "offset": w},
                                    "end": {"line": 0, "character": 0, "offset": w + 5}},
                          "newText": ""}])
    # (iii) astral: insert an emoji, then replace it (astral in newText, then in range)
    end = len(held.encode())
    do_edit("astral-insert", [{"range": {"start": {"line": 0, "character": 0, "offset": end},
                                         "end": {"line": 0, "character": 0, "offset": end}},
                               "newText": " 😀!"}])
    # replace the emoji with X — the range must span its 2 UTF-16 units
    emoji_byte = held.encode().index("😀".encode())
    do_edit("astral-replace", [{"range": {"start": {"line": 0, "character": 0, "offset": emoji_byte},
                                          "end": {"line": 0, "character": 0, "offset": emoji_byte + 4}},
                                "newText": "X"}])

    # ── anchor ───────────────────────────────────────────────────────────────
    cur = fresh_text(ctrl, workspace_id, buffer_id)
    pos_byte = cur.index("!")  # anchor just before the '!'
    ac = ctrl.request("anchor/create", {"bufferId": buffer_id,
                                        "position": {"line": 0, "character": 0, "offset": pos_byte},
                                        "bias": "right"})
    token = ac["result"]["anchor"]
    resolved_before = ctrl.request("anchor/resolve", {"bufferId": buffer_id, "anchor": token})
    # Insert text BEFORE the anchor; it must shift forward.
    ctrl.request("buffer/edit", {"bufferId": buffer_id,
                                 "edits": [{"range": {"start": {"line": 0, "character": 0, "offset": 0},
                                                      "end": {"line": 0, "character": 0, "offset": 0}},
                                            "newText": "PREFIX "}]})
    resolved_after = ctrl.request("anchor/resolve", {"bufferId": buffer_id, "anchor": token})
    before_off = resolved_before["result"]["position"]["offset"]
    after_off = resolved_after["result"]["position"]["offset"]
    moved = (after_off == before_off + len("PREFIX ")) and resolved_after["result"]["valid"]
    print(f"\n[anchor] before_offset={before_off} after_offset={after_off} "
          f"valid={resolved_after['result']['valid']} MOVED_OK={moved}")
    results.append(("anchor:moved", moved))

    # drain A's backlog of didChange from the anchor edits so it's clean
    time.sleep(0.2)

    a.close()
    b.close()
    ctrl.close()

    # ── test 8 ───────────────────────────────────────────────────────────────
    print("\n[test8] stalled-subscriber drop")
    sa = Conn("STALL-A")
    sc = Conn("PING-C")
    sa.request("initialize")
    sc.request("initialize")
    sa.request("buffer/subscribe", {"bufferId": buffer_id})
    fb = Conn("FLOOD-B")
    fb.request("initialize")
    # Flood edits WITHOUT reading sa. Each insert is large so sa's outbound
    # socket buffer + the MAX_OUTBOUND_QUEUED(256) channel fill quickly. B reads
    # its OWN responses every iteration (request, not send) so B never self-stalls.
    big = "z" * 8192
    flooded = 0
    dropped = False
    for i in range(1500):
        try:
            fb.request("buffer/edit", {"bufferId": buffer_id,
                                       "edits": [{"range": {"start": {"line": 0, "character": 0, "offset": 0},
                                                            "end": {"line": 0, "character": 0, "offset": 0}},
                                                 "newText": big}]}, timeout=5.0)
            flooded += 1
        except (BrokenPipeError, OSError, TimeoutError):
            break
    time.sleep(0.5)
    # Is sa closed? Drain its buffered notifications; the server shut down the
    # socket on overflow, so recv eventually returns b"" (EOF) or resets.
    sa.s.settimeout(2.0)
    dropped = False
    deadline = time.time() + 8.0
    while time.time() < deadline:
        try:
            data = sa.s.recv(65536)
        except socket.timeout:
            break
        except (ConnectionResetError, OSError) as e:
            dropped = True
            print(f"   stalled subscriber reset (=dropped): {e}")
            break
        if data == b"":
            dropped = True  # clean EOF: server closed the subscriber
            break
    # conn C ping still works → editor responsive
    ping = sc.request("ping", timeout=5.0)
    ping_ok = ping.get("result", {}).get("ok") is True
    print(f"   flooded={flooded} stalled_subscriber_dropped={dropped} ping_C_ok={ping_ok}")
    results.append(("test8:dropped", dropped))
    results.append(("test8:ping_responsive", ping_ok))

    sa.close()
    sc.close()
    fb.close()

    print("\n==== SUMMARY ====")
    allok = True
    for name, ok in results:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
        allok = allok and ok
    print("ALL PASS" if allok else "SOME FAILED")
    sys.exit(0 if allok else 1)


if __name__ == "__main__":
    main()
