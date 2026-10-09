#!/usr/bin/env python3
"""A virtual keyboard for the e2e VM: presses key chords on the real seat.

The compositor (mutter, or Xorg through libinput/evdev) sees an ordinary
keyboard, so global shortcuts, focus and input methods behave as they do for a
person at the keys; neither X11 nor Wayland needs a special injector.

Run as root (needs /dev/uinput). Reads one chord per line from a FIFO, e.g.
`super+j` or `Escape`, and presses and releases it. Prints `ready` once the
device exists and the compositor has had time to adopt it.

Usage: uinput-keys.py FIFO
"""

import fcntl
import os
import struct
import sys
import time

# linux/input-event-codes.h
KEYS = {
    "escape": 1,
    "enter": 28,
    "ctrl": 29,
    "shift": 42,
    "alt": 56,
    "space": 57,
    "super": 125,
    **{c: code for c, code in zip("qwertyuiop", range(16, 26))},
    **{c: code for c, code in zip("asdfghjkl", range(30, 39))},
    **{c: code for c, code in zip("zxcvbnm", range(44, 51))},
}
EV_SYN, EV_KEY = 0, 1
BUS_USB = 3

# _IOW('U', n, int) and friends from linux/uinput.h
UI_SET_EVBIT = 0x40045564
UI_SET_KEYBIT = 0x40045565
UI_DEV_CREATE = 0x5501
UI_DEV_DESTROY = 0x5502
UI_DEV_SETUP = 0x405C5503


def emit(fd, etype, code, value):
    os.write(fd, struct.pack("llHHi", 0, 0, etype, code, value))


def press(fd, chord):
    codes = [KEYS[k.lower()] for k in chord.split("+")]
    for code in codes:
        emit(fd, EV_KEY, code, 1)
        emit(fd, EV_SYN, 0, 0)
        time.sleep(0.02)
    for code in reversed(codes):
        emit(fd, EV_KEY, code, 0)
        emit(fd, EV_SYN, 0, 0)
        time.sleep(0.02)


def main(fifo):
    fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    fcntl.ioctl(fd, UI_SET_EVBIT, EV_KEY)
    for code in set(KEYS.values()):
        fcntl.ioctl(fd, UI_SET_KEYBIT, code)
    name = b"myna-e2e-keyboard"
    setup = struct.pack("HHHH80sI", BUS_USB, 0x1234, 0x5678, 1, name, 0)
    fcntl.ioctl(fd, UI_DEV_SETUP, setup)
    fcntl.ioctl(fd, UI_DEV_CREATE)
    try:
        # udev, libinput and the compositor adopt a new device asynchronously.
        time.sleep(1.5)
        print("ready", flush=True)
        while True:
            with open(fifo) as f:
                for line in f:
                    if line.strip():
                        press(fd, line.strip())
    finally:
        fcntl.ioctl(fd, UI_DEV_DESTROY)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
