#!/usr/bin/env python3
"""What Renoise's `client:send` does once a socket's buffers are full, on this system (macOS, Windows or Linux).

Run it against a real Renoise with Kumi's tool in it (`kumi --daw renoise bridge`) and a song open; a song with many
tracks and patterns gives bigger answers. Python 3.11+, nothing else:

    python3 daws/renoise/tests/send_probe.py [--kumi-home <folder>] [--seconds 20]

The tool sends only within the credit the host grants, and Kumi's host grants no more than its receive buffer holds,
so its own connection never meets a full socket. A host that grants more than it can hold and then stops reading
does: that is what this measures. Measured on macOS (Renoise 3.5.4): a send to a full socket writes what fits and
returns false, with no block and no queue, so the tool retires that host and the GUI stays responsive. A block would
freeze Renoise's GUI thread (every callback of every tool) until the host reads; a queue would grow Renoise's memory.

  A  a reader that keeps up: the answers' sizes and times, the baseline
  B  a host that grants the most credit, asks for a 1 KiB receive buffer (macOS grows it on loopback; the probe says what
     it got), asks for up to the credit's worth of answers, and never reads; meanwhile a second connection pings every
     100 ms, and the probe watches Renoise's memory and the tool's log for the stalled host's retirement
  C  that host then reads: what reached it (heartbeats aside), and whether its last answer is whole

The answers have to outgrow the sockets' buffers for B to show anything: with a small song they fit, and the probe
says so; open a song with many tracks and patterns (its snapshot is the biggest answer) and run it again.

It prints what it saw and a verdict (returns false / blocks / queues). Kumi's tool and Renoise are left running; the
stalled connection is closed by this side at the end.
"""
from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import math
import os
from pathlib import Path
import platform
import re
import secrets
import socket
import subprocess
import threading
import time

PROTOCOL = "kumi-daw/1"
CREDIT_MAX = 4 * 1024 * 1024  # the most the tool holds (payload/wire.lua)
QUEUED_ANSWERS = 64  # answers the tool queues for one connection before closing it (payload/server.lua)


def proof(secret: str, role: str, client_nonce: str, server_nonce: str) -> str:
    message = f"{PROTOCOL}\n{role}\n{client_nonce}\n{server_nonce}".encode()
    return hmac.new(secret.encode(), message, hashlib.sha256).hexdigest()


class Client:
    """One authenticated connection to the tool."""

    def __init__(self, port: int, secret: str, name: str, receive_buffer: int | None = None):
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        if receive_buffer:
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, receive_buffer)
        self.socket.settimeout(10)
        self.socket.connect(("127.0.0.1", port))
        self.held = self.socket.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
        self.buffer = b""
        self.next_id = 1
        nonce = secrets.token_hex(16)
        self.send({"type": "hello", "protocol": PROTOCOL, "nonce": nonce, "client": name})
        challenge = self.read()
        if challenge.get("type") != "challenge" or not hmac.compare_digest(challenge["proof"], proof(secret, "server", nonce, challenge["nonce"])):
            raise RuntimeError(f"the tool's challenge didn't prove the secret: {challenge}")
        self.send({"type": "auth", "proof": proof(secret, "client", nonce, challenge["nonce"])})
        self.welcome = self.read()
        if self.welcome.get("type") != "welcome":
            raise RuntimeError(f"not welcomed: {self.welcome}")

    def send(self, frame: dict) -> None:
        self.socket.sendall(json.dumps(frame, separators=(",", ":")).encode() + b"\n")

    def grant(self, size: int) -> None:
        self.send({"type": "credit", "bytes": size})

    def ask(self, method: str) -> int:
        request = self.next_id
        self.next_id += 1
        self.send({"type": "request", "id": request, "method": method, "params": {}})
        return request

    def read(self, want: int | None = None, timeout: float = 10) -> dict:
        end = time.monotonic() + timeout
        while True:
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                if line.strip():
                    frame = json.loads(line)
                    if want is None or (frame.get("type") == "response" and frame.get("id") == want):
                        return frame
            left = end - time.monotonic()
            if left <= 0:
                raise TimeoutError
            self.socket.settimeout(left)
            data = self.socket.recv(1 << 16)
            if not data:
                raise ConnectionError("the tool closed the connection")
            self.buffer += data

    def call(self, method: str, timeout: float = 10) -> tuple[dict, int, float]:
        """An answer, its size on the wire and how long it took."""
        start = time.perf_counter()
        answer = self.read(self.ask(method), timeout)
        return answer, len(json.dumps(answer)) + 1, (time.perf_counter() - start) * 1000


def bridge(kumi_home: Path) -> tuple[str, int, int]:
    """The secret, and the port and pid of the running tool's discovery file (the newest whose Renoise runs: files of
    Renoises that ended stay until Kumi prunes them)."""
    folder = kumi_home / "bridges" / "renoise"
    secret = (folder / "bridge.secret").read_text(encoding="ascii").strip()
    running = set(renoise_pids())
    found = []
    for path in sorted((folder / "discovery").glob("*.json")):
        discovery = json.loads(path.read_text(encoding="utf-8"))
        if int(discovery.get("pid") or 0) in running:
            found.append((path.stat().st_mtime, discovery))
    if not found:
        raise SystemExit(f"no running Renoise is serving Kumi's tool: no discovery file of one in {folder / 'discovery'}")
    discovery = max(found, key=lambda item: item[0])[1]
    return secret, int(discovery["port"]), int(discovery["pid"])


def renoise_pids() -> list[int]:
    if platform.system() == "Windows":
        out = subprocess.run(["tasklist", "/FI", "IMAGENAME eq Renoise.exe", "/FO", "CSV", "/NH"], capture_output=True, text=True).stdout
        return [int(row.split('","')[1]) for row in out.splitlines() if row.startswith('"Renoise')]
    out = subprocess.run(["pgrep", "-x", "Renoise" if platform.system() == "Darwin" else "renoise"], capture_output=True, text=True).stdout
    return [int(pid) for pid in out.split()]


def memory_mb(pid: int) -> float | None:
    """The process's resident memory in MB, or None when it can't be read."""
    try:
        if platform.system() == "Windows":
            row = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"], capture_output=True, text=True).stdout
            kilobytes = re.search(r'"([\d.,\s ]+) K"', row)
            return int(re.sub(r"\D", "", kilobytes[1])) / 1024 if kilobytes else None
        if Path(f"/proc/{pid}/status").exists():
            return int(re.search(r"VmRSS:\s+(\d+)", Path(f"/proc/{pid}/status").read_text())[1]) / 1024
        return int(subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout) / 1024
    except (ValueError, TypeError, OSError):
        return None


def log_lines(kumi_home: Path, port: int) -> list[str]:
    """The tool's log lines about the connection from this local port."""
    lines = []
    for path in sorted((kumi_home / "bridges" / "renoise" / "logs").glob("*")):
        lines += [line for line in path.read_text(encoding="utf-8", errors="replace").splitlines() if f":{port}" in line]
    return lines


class Pinger:
    """Pings every 100 ms on a connection of its own: how long Renoise's GUI thread takes to answer."""

    def __init__(self, port: int, secret: str):
        self.client = Client(port, secret, "send-probe-pinger")
        self.client.grant(CREDIT_MAX)
        self.times: list[tuple[float, float]] = []
        self.answered = time.monotonic()
        self.running = True
        threading.Thread(target=self.run, daemon=True).start()

    def run(self) -> None:
        while self.running:
            try:
                _, _, ms = self.client.call("ping", timeout=60)
                self.times.append((time.monotonic(), ms))
                self.answered = time.monotonic()
                self.client.grant(4096)
            except Exception as error:  # noqa: BLE001 (the probe reports whatever stopped it)
                print(f"  pinger stopped: {type(error).__name__}: {error}", flush=True)
                return
            time.sleep(0.1)

    def since(self, start: float) -> str:
        ms = sorted(ms for at, ms in self.times if at >= start)
        if not ms:
            return f"no ping answered (the last {time.monotonic() - self.answered:.1f} s ago)"
        return f"{len(ms)} pings, median {ms[len(ms) // 2]:.1f} ms, max {ms[-1]:.1f} ms"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--kumi-home", type=Path, default=Path(os.environ.get("KUMI_HOME") or Path.home() / ".kumi"))
    parser.add_argument("--seconds", type=float, default=20, help="how long B watches the stalled host")
    args = parser.parse_args()
    secret, port, pid = bridge(args.kumi_home)
    print(f"{platform.system()} {platform.release()} ({platform.machine()}), Python {platform.python_version()}; "
          f"the tool on port {port}, Renoise pid {pid or 'unknown'}, memory {memory_mb(pid) if pid else None} MB", flush=True)

    # A: a reader that keeps up.
    reader = Client(port, secret, "send-probe-reader")
    reader.grant(CREDIT_MAX)
    print(f"A  welcome {reader.welcome.get('identity', {}).get('dawVersion')}, credit floor {reader.welcome.get('creditBytes')}; "
          f"this host's receive buffer {reader.held} bytes", flush=True)
    sizes = {}
    for method in ("status", "snapshot", "focus"):
        answer, size, ms = reader.call(method, timeout=30)
        sizes[method] = size
        state = "error " + str(answer.get("error", {}).get("code")) if answer.get("error") else "ok"
        print(f"A  {method}: {size} bytes in {ms:.1f} ms ({state})", flush=True)
        reader.grant(size)
    reader.socket.close()
    method = max(sizes, key=sizes.get)

    # B: a host that grants the most credit, holds 1 KiB, asks and never reads.
    pinger = Pinger(port, secret)
    time.sleep(1)
    stalled = Client(port, secret, "send-probe-stalled", receive_buffer=1024)
    local = stalled.socket.getsockname()[1]
    stalled.grant(CREDIT_MAX)
    asked = min(QUEUED_ANSWERS, max(8, math.ceil(CREDIT_MAX / max(sizes[method], 1))))
    asked_bytes = asked * sizes[method]
    for _ in range(asked):
        stalled.ask(method)
    print(f"B  the stalled host (port {local}, receive buffer {stalled.held} bytes) asked for {asked} {method} answers "
          f"(about {asked_bytes // 1024} KiB) and reads nothing", flush=True)
    start = time.monotonic()
    froze, before = None, memory_mb(pid) if pid else None
    for second in range(int(args.seconds)):
        time.sleep(1)
        retired = [line[-120:] for line in log_lines(args.kumi_home, local) if "retir" in line or "send failed" in line]
        memory = memory_mb(pid) if pid else None
        print(f"B  {second + 1:2d} s: {pinger.since(time.monotonic() - 1)}; memory {memory} MB; the tool's log: {retired[-1:] or 'nothing yet'}", flush=True)
        if time.monotonic() - pinger.answered > 5:
            froze = time.monotonic() - pinger.answered
            break
    after = memory_mb(pid) if pid else None
    retired = [line[-160:] for line in log_lines(args.kumi_home, local) if "retir" in line or "send failed" in line]

    # C: the stalled host reads what reached it: every answer, or until none has come for 3 s (heartbeats keep arriving
    # each second, so they don't count), at most 30 s.
    got, answers, last, end = b"", 0, time.monotonic(), time.monotonic() + 30
    stalled.socket.settimeout(0.5)
    while answers < asked and time.monotonic() < end and time.monotonic() - last < 3:
        try:
            data = stalled.socket.recv(1 << 20)
        except socket.timeout:
            continue
        except OSError:
            break
        if not data:
            break
        got += data
        counted = got.count(b'\n{"type":"response"') + got.startswith(b'{"type":"response"')
        if counted > answers:
            answers, last = counted, time.monotonic()
    torn = bool(got) and not got.endswith(b"\n")
    print(f"C  the stalled host then read {len(got)} bytes: {answers} of {asked} answers, the stream {'torn' if torn else 'whole'}", flush=True)
    stalled.socket.close()
    print(f"   pings while it stalled: {pinger.since(start)}", flush=True)
    pinger.running = False

    if froze:
        verdict = f"BLOCKS: Renoise answered no ping for {froze:.1f} s while a host held a full socket (the GUI thread waits in send)"
    elif any("send failed" in line for line in retired):
        verdict = "RETURNS FALSE: the tool retired the stalled host (send failed) and Renoise kept answering, as on macOS"
    elif retired:
        verdict = "UNDECIDED: the tool retired the stalled host for another reason (its log, below); run it again"
    elif before is not None and after is not None and after - before > 32:
        verdict = f"QUEUES: no block and no failed send, and Renoise's memory grew {after - before:.0f} MB"
    elif answers == asked and asked_bytes < 4 * stalled.held:
        verdict = (f"UNDECIDED: the {asked_bytes // 1024} KiB asked for fit in the sockets' buffers (this host's alone holds "
                   f"{stalled.held // 1024} KiB), so nothing filled: open a song with more tracks and patterns and run it again")
    else:
        verdict = "UNDECIDED: no block, no failed send, no memory growth; open a bigger song and run it again"
    print(f"\nVerdict: {verdict}\nThe tool's log about the stalled host:\n  " + "\n  ".join(retired or ["(nothing)"]), flush=True)


if __name__ == "__main__":
    main()
