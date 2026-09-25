# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

"""Dials the softmodem from slmodemd's port and passes text both ways,
reading both serial ports."""

import os
import re
import sys
import termios
import time
import tty

TEXT_UP = b"Hello from slmodemd, 0123456789 abcdefghijklmnopqrstuvwxyz\r\n"
TEXT_DOWN = b"And back from the softmodem, ZYXWVUTSRQPONMLKJIHGFEDCBA\r\n"


class Port:
    def __init__(self, name, path):
        self.name = name
        self.fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        tty.setraw(self.fd)
        attributes = termios.tcgetattr(self.fd)
        attributes[2] |= termios.CLOCAL
        termios.tcsetattr(self.fd, termios.TCSANOW, attributes)
        self.seen = b""

    def expect(self, pattern, timeout):
        deadline = time.monotonic() + timeout
        while True:
            try:
                self.seen += os.read(self.fd, 4096)
            except (BlockingIOError, OSError):
                pass
            found = re.search(pattern, self.seen)
            if found:
                self.seen = self.seen[found.end():]
                print(f"{self.name}: {found.group(0).decode(errors='replace')}", flush=True)
                return found.group(0)
            if time.monotonic() > deadline:
                sys.exit(f"{self.name} never sent {pattern!r}, sent {self.seen!r}")
            time.sleep(0.05)

    def send(self, data):
        os.write(self.fd, data)


def exchange(caller, isp, round):
    up = TEXT_UP.replace(b"slmodemd", f"slmodemd, round {round}".encode())
    down = TEXT_DOWN.replace(b"softmodem", f"softmodem, round {round}".encode())
    caller.send(up)
    isp.expect(re.escape(up.strip()), 90)
    isp.send(down)
    caller.expect(re.escape(down.strip()), 90)


def main():
    caller_path, isp_path, theirs = sys.argv[1:4]
    # Before a second exchange: seconds to wait for noise, or "ato1" to retrain from the softmodem.
    later = sys.argv[4] if len(sys.argv) > 4 else None
    caller = Port("slmodemd", caller_path)
    isp = Port("softmodem", isp_path)
    caller.send(f"ATE0X3{theirs}\r".encode())
    caller.expect(rb"OK", 30)
    caller.send(b"ATDT0300\r")
    result = caller.expect(rb"CONNECT[^\r\n]*|NO CARRIER|BUSY|ERROR", 300)
    if not result.startswith(b"CONNECT"):
        sys.exit(f"slmodemd gave {result!r}")
    isp.expect(rb"CONNECT[^\r\n]*", 60)
    time.sleep(2)
    exchange(caller, isp, 1)
    if later == "ato1":
        time.sleep(1.5)
        isp.send(b"+++")
        isp.expect(rb"OK", 10)
        isp.send(b"ATO1\r")
        isp.expect(rb"CONNECT[^\r\n]*", 30)
        time.sleep(15)
        exchange(caller, isp, 2)
    elif later is not None:
        time.sleep(float(later))
        exchange(caller, isp, 2)
    time.sleep(1.5)
    isp.send(b"+++")
    isp.expect(rb"OK", 10)
    isp.send(b"ATH\r")
    isp.expect(rb"OK", 10)
    caller.expect(rb"NO CARRIER", 10)


if __name__ == "__main__":
    main()
