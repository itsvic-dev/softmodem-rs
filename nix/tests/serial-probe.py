"""Uses a serial port the way QEMU's host serial backend does, and checks
the modem lines it reads with TIOCMGET."""

import fcntl
import os
import select
import struct
import sys
import termios
import time

PORT = "/dev/ttySM0"


def open_port():
    fd = os.open(PORT, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    attributes = termios.tcgetattr(fd)
    attributes[2] |= termios.CLOCAL
    termios.tcsetattr(fd, termios.TCSANOW, attributes)
    return fd


def lines(fd):
    return struct.unpack("I", fcntl.ioctl(fd, termios.TIOCMGET, b"\0" * 4))[0]


def carrier(fd):
    return bool(lines(fd) & termios.TIOCM_CAR)


def ring(fd):
    return bool(lines(fd) & termios.TIOCM_RNG)


def expect(fd, text, timeout=60):
    seen = b""
    deadline = time.monotonic() + timeout
    while text.encode() not in seen:
        left = deadline - time.monotonic()
        if left <= 0:
            sys.exit(f"never saw {text!r}, saw {seen!r}")
        readable, _, _ = select.select([fd], [], [], left)
        if readable:
            try:
                seen += os.read(fd, 256)
            except BlockingIOError:
                pass
    print(f"saw {text!r}")


def hang_up(fd):
    time.sleep(1.2)
    os.write(fd, b"+++")
    time.sleep(1.2)
    expect(fd, "OK")
    os.write(fd, b"ATH\r")
    expect(fd, "OK")
    time.sleep(0.5)
    assert not carrier(fd), "DCD stayed on after hanging up"
    print("DCD off after hang-up")


def dial():
    fd = open_port()
    assert not carrier(fd), "DCD on before a call"
    os.write(fd, b"ATE0\r")
    expect(fd, "OK")
    os.write(fd, b"ATDT0300\r")
    expect(fd, "CONNECT")
    assert carrier(fd), "no DCD after CONNECT"
    print("DCD on after CONNECT")
    hang_up(fd)


def answer():
    fd = open_port()
    deadline = time.monotonic() + 30
    while not ring(fd):
        assert time.monotonic() < deadline, "RI never rose"
        time.sleep(0.05)
    print("RI on while ringing")
    expect(fd, "RING")
    os.write(fd, b"ATA\r")
    expect(fd, "CONNECT")
    assert carrier(fd), "no DCD after answering"
    assert not ring(fd), "RI still on after answering"
    print("DCD on and RI off after answering")
    hang_up(fd)


{"dial": dial, "answer": answer}[sys.argv[1]]()
