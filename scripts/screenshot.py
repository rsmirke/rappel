#!/usr/bin/env python3
"""Screenshot Rappel on a private Xvfb display, optionally driving it with keys.

For batch mode (CLAUDE.md): screenshots go to docs/briefs/screenshots/, which
is git-ignored. Input is sent only with XTest to the private Xvfb display,
never through AT-SPI or the session bus, so nothing can reach the desktop.

Usage (run with `python3 -I`):
  scripts/screenshot.py --out NAME --size 1000x720 --scheme dark \
      [--data DIR | --fill "30 --due 5 --tags"] [--deck DECK_ID] \
      [--steps "wait:4 shot:list key:ctrl+n wait:1 type:Hello shot:editor"]

Steps, separated by spaces (use %20 for a space inside type:):
  wait:SECONDS      let the app run
  shot:SUFFIX       save <out>-<SUFFIX>.png
  key:COMBO         e.g. Tab, Return, ctrl+n, ctrl+shift+Return, F10, Escape
  type:TEXT         type characters (ASCII)
  click:X,Y         left click at window coordinates
  resize:WxH        not supported (the screen is the window); run again instead
Without --steps: wait 6 s, then one shot <out>.png.
The whole run is killed after 110 s (CLAUDE.md: under 2 minutes).
"""
import argparse
import ctypes
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHOTS = os.path.join(REPO, "docs", "briefs", "screenshots")
DEADLINE = time.monotonic() + 110

NAMED = {
    " ": "space", "\n": "Return", "\t": "Tab", "!": "exclam", '"': "quotedbl",
    "#": "numbersign", "$": "dollar", "%": "percent", "&": "ampersand",
    "'": "apostrophe", "(": "parenleft", ")": "parenright", "*": "asterisk",
    "+": "plus", ",": "comma", "-": "minus", ".": "period", "/": "slash",
    ":": "colon", ";": "semicolon", "<": "less", "=": "equal", ">": "greater",
    "?": "question", "@": "at", "[": "bracketleft", "\\": "backslash",
    "]": "bracketright", "^": "asciicircum", "_": "underscore", "`": "grave",
    "{": "braceleft", "|": "bar", "}": "braceright", "~": "asciitilde",
}
SHIFTED = set('!"#$%&()*+:<>?@^_{|}~') | set("ABCDEFGHIJKLMNOPQRSTUVWXYZ")
MODS = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L", "super": "Super_L"}


class X:
    def __init__(self, display):
        self.x11 = ctypes.CDLL("libX11.so.6")
        self.xtst = ctypes.CDLL("libXtst.so.6")
        self.x11.XOpenDisplay.restype = ctypes.c_void_p
        self.x11.XStringToKeysym.restype = ctypes.c_ulong
        self.x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        self.xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
        self.x11.XFlush.argtypes = [ctypes.c_void_p]
        self.dpy = self.x11.XOpenDisplay(display.encode())
        if not self.dpy:
            sys.exit(f"cannot open {display}")

    def code(self, name):
        sym = self.x11.XStringToKeysym(name.encode())
        if not sym:
            sys.exit(f"unknown key {name!r}")
        code = self.x11.XKeysymToKeycode(self.dpy, sym)
        if not code:
            sys.exit(f"no keycode for {name!r}")
        return code

    def press(self, names):
        codes = [self.code(n) for n in names]
        for c in codes:
            self.xtst.XTestFakeKeyEvent(self.dpy, c, 1, 0)
        for c in reversed(codes):
            self.xtst.XTestFakeKeyEvent(self.dpy, c, 0, 0)
        self.x11.XFlush(self.dpy)
        time.sleep(0.08)

    def combo(self, text):
        parts = text.split("+")
        self.press([MODS.get(p.lower(), p) for p in parts[:-1]] + [parts[-1]])

    def type(self, text):
        for ch in text:
            name = NAMED.get(ch, ch)
            self.press(["Shift_L", name] if ch in SHIFTED else [name])

    def click(self, x, y):
        self.xtst.XTestFakeMotionEvent(self.dpy, -1, x, y, 0)
        self.xtst.XTestFakeButtonEvent(self.dpy, 1, 1, 0)
        self.xtst.XTestFakeButtonEvent(self.dpy, 1, 0, 0)
        self.x11.XFlush(self.dpy)
        time.sleep(0.1)


def free_display():
    for n in range(90, 200):
        if not os.path.exists(f"/tmp/.X{n}-lock") and not os.path.exists(f"/tmp/.X11-unix/X{n}"):
            return f":{n}"
    sys.exit("no free display number")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--out", required=True, help="file name stem under docs/briefs/screenshots/")
    p.add_argument("--size", default="1000x720")
    p.add_argument("--scheme", choices=["light", "dark"], default="light")
    p.add_argument("--data", help="an existing scratch data dir (copied, never changed)")
    p.add_argument("--fill", default="30 --due 5", help="fill_scratch_db arguments")
    p.add_argument("--deck", default="", help="deck id to open at start")
    p.add_argument("--steps", default="wait:6 shot:")
    p.add_argument("--env", action="append", default=[], help="extra KEY=VALUE for the app")
    a = p.parse_args()
    width, height = (int(v) for v in a.size.lower().split("x"))
    os.makedirs(SHOTS, exist_ok=True)

    work = tempfile.mkdtemp(prefix="rappel-shot-")
    data = os.path.join(work, "data")
    if a.data:
        shutil.copytree(a.data, data)
    else:
        os.makedirs(data, mode=0o700)
        subprocess.run(["cargo", "run", "-q", "-p", "rappel-core", "--example", "fill_scratch_db", "--", data]
                       + a.fill.split(), cwd=REPO, check=True, stdout=subprocess.DEVNULL)
    with open(os.path.join(data, "window-state.ini"), "w") as f:
        f.write(f"[window]\nwidth={width}\nheight={height}\nmaximized=false\ndeck={a.deck}\n")
    subprocess.run(["cargo", "build", "-q", "-p", "rappel"], cwd=REPO, check=True)

    display = free_display()
    if display == os.environ.get("DISPLAY"):
        sys.exit("refusing: that is the real display")
    xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", f"{width}x{height}x24", "-nolisten", "tcp"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "AT_SPI_BUS_ADDRESS")}
    env.update(DISPLAY=display, GDK_BACKEND="x11", GSK_RENDERER="cairo", NO_AT_BRIDGE="1",
               DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent/rappel-harness",
               ADW_DEBUG_COLOR_SCHEME=f"prefer-{a.scheme}", RAPPEL_DATA_DIR=data, GTK_A11Y="none")
    for kv in a.env:
        k, _, v = kv.partition("=")
        env[k] = v
    log = open(os.path.join(SHOTS, a.out + ".log"), "w")
    app = subprocess.Popen([os.path.join(REPO, "target", "debug", "rappel")], env=env, stdout=log, stderr=log)
    x = X(display)
    saved = []
    try:
        for step in a.steps.split():
            if time.monotonic() > DEADLINE:
                print("deadline reached", file=sys.stderr)
                break
            kind, _, arg = step.partition(":")
            arg = arg.replace("%20", " ")
            if kind == "wait":
                time.sleep(float(arg))
            elif kind == "shot":
                path = os.path.join(SHOTS, f"{a.out}-{arg}.png" if arg else f"{a.out}.png")
                subprocess.run(["import", "-display", display, "-window", "root", path], check=True)
                saved.append(path)
            elif kind == "key":
                x.combo(arg)
            elif kind == "type":
                x.type(arg)
            elif kind == "click":
                cx, cy = (int(v) for v in arg.split(","))
                x.click(cx, cy)
            else:
                sys.exit(f"unknown step {step!r}")
            if app.poll() is not None:
                print(f"the app exited with {app.returncode}; see {log.name}", file=sys.stderr)
                break
    finally:
        app.send_signal(signal.SIGTERM)
        try:
            app.wait(5)
        except subprocess.TimeoutExpired:
            app.kill()
        xvfb.terminate()
        xvfb.wait()
        shutil.rmtree(work, ignore_errors=True)
    for path in saved:
        print(path)


if __name__ == "__main__":
    main()
