# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

"""Dials the softmodem from slmodemd's port, or with --answer slmodemd from
the softmodem's, and passes text both ways, reading both serial ports."""

import argparse
import os
import re
import select
import sys
import termios
import time
import tty

TEXT_UP = b"Hello from slmodemd, 0123456789 abcdefghijklmnopqrstuvwxyz\r\n"
TEXT_DOWN = b"And back from the softmodem, ZYXWVUTSRQPONMLKJIHGFEDCBA\r\n"


class Port:
    # slmodemd exits just after NO CARRIER, and its pty's hangup drops unread text.
    ports = []

    def __init__(self, name, path):
        self.name = name
        self.fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        tty.setraw(self.fd)
        attributes = termios.tcgetattr(self.fd)
        attributes[2] |= termios.CLOCAL
        termios.tcsetattr(self.fd, termios.TCSANOW, attributes)
        self.seen = b""
        self.open = True
        Port.ports.append(self)

    def read(self):
        try:
            data = os.read(self.fd, 4096)
        except BlockingIOError:
            return
        except OSError:
            data = b""
        self.seen += data
        self.open = bool(data)

    def expect(self, pattern, timeout):
        deadline = time.monotonic() + timeout
        while True:
            found = re.search(pattern, self.seen)
            if found:
                self.seen = self.seen[found.end():]
                print(f"{self.name}: {found.group(0).decode(errors='replace')}", flush=True)
                return found.group(0)
            left = deadline - time.monotonic()
            if left <= 0:
                sys.exit(f"{self.name} never sent {pattern!r}, sent {self.seen!r}")
            ready, _, _ = select.select([p.fd for p in Port.ports if p.open], [], [], left)
            for port in Port.ports:
                if port.fd in ready:
                    port.read()

    def send(self, data):
        os.write(self.fd, data)


def exchange(slmodemd, softmodem, round):
    up = TEXT_UP.replace(b"slmodemd", f"slmodemd, round {round}".encode())
    down = TEXT_DOWN.replace(b"softmodem", f"softmodem, round {round}".encode())
    slmodemd.send(up)
    softmodem.expect(re.escape(up.strip()), 90)
    softmodem.send(down)
    slmodemd.expect(re.escape(down.strip()), 90)


def connected(port, timeout):
    result = port.expect(rb"CONNECT[^\r\n]*|NO CARRIER|BUSY|ERROR", timeout)
    if not result.startswith(b"CONNECT"):
        sys.exit(f"{port.name} gave {result!r}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("slmodemd")
    parser.add_argument("softmodem")
    parser.add_argument("theirs")
    parser.add_argument("later", nargs="?", help='seconds to wait for noise, or "ato1" to retrain from the softmodem')
    parser.add_argument("--answer", action="store_true", help="slmodemd answers")
    args = parser.parse_args()
    slmodemd = Port("slmodemd", args.slmodemd)
    softmodem = Port("softmodem", args.softmodem)
    slmodemd.send(f"ATE0X3{args.theirs}\r".encode())
    slmodemd.expect(rb"OK", 30)
    if args.answer:
        slmodemd.send(b"ATA\r")
        softmodem.send(b"ATDT0300\r")
        connected(softmodem, 300)
        connected(slmodemd, 60)
    else:
        slmodemd.send(b"ATDT0300\r")
        connected(slmodemd, 300)
        connected(softmodem, 60)
    time.sleep(2)
    exchange(slmodemd, softmodem, 1)
    if args.later == "ato1":
        time.sleep(1.5)
        softmodem.send(b"+++")
        softmodem.expect(rb"OK", 10)
        softmodem.send(b"ATO1\r")
        softmodem.expect(rb"CONNECT[^\r\n]*", 30)
        time.sleep(15)
        exchange(slmodemd, softmodem, 2)
    elif args.later is not None:
        time.sleep(float(args.later))
        exchange(slmodemd, softmodem, 2)
    time.sleep(1.5)
    softmodem.send(b"+++")
    softmodem.expect(rb"OK", 10)
    softmodem.send(b"ATH\r")
    softmodem.expect(rb"OK", 10)
    slmodemd.expect(rb"NO CARRIER", 10)


if __name__ == "__main__":
    main()
