#!/usr/bin/env -S python3 -I
"""Screenshot Rappel on a private Xvfb display, optionally driving it with keys.

For batch mode (CLAUDE.md): screenshots go to docs/briefs/screenshots/, which
is git-ignored. Input is sent only with XTest to the private Xvfb display,
never through AT-SPI or the session bus, so nothing can reach the desktop:
Xvfb picks its own free display (-displayfd) and requires a fresh cookie
(-auth), so a connection can only reach that server.

Needs: Xvfb, xauth, ImageMagick's `import`, libX11 and libXtst.

Usage:
  scripts/screenshot.py --out NAME --size 1000x720 --scheme dark \
      [--data DIR | --fill "30 --due 5 --tags"] [--deck DECK_ID] \
      [--steps "wait:4 shot:list key:ctrl+n wait:1 type:Hello shot:editor"] \
      [--env LC_ALL=fr_FR.UTF-8] [--decoration-layout :close]

Steps, separated by spaces (use %20 for a space inside type:):
  wait:SECONDS      let the app run
  shot:SUFFIX       save <out>-<SUFFIX>.png
  key:COMBO         e.g. Tab, Return, ctrl+n, ctrl+shift+Return, F10, Escape
  type:TEXT         type characters (ASCII)
  click:X,Y         left click at window coordinates
  resize:W,H        resize the app's window (as an on-screen keyboard does
                    on a phone); its top-left corner stays where it is
Without --steps: wait 6 s, then one shot <out>.png.
Steps stop after 110 s, and the whole run is killed after 115 s
(CLAUDE.md: under 2 minutes).
"""
import argparse
import ctypes
import os
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHOTS = os.path.join(REPO, "docs", "briefs", "screenshots")
STEP_SECONDS = 110
RUN_SECONDS = 115
# The app never gets these from --env: they keep it on the private display,
# off the session bus and out of real data.
PROTECTED = {"DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "GDK_BACKEND", "DBUS_SESSION_BUS_ADDRESS",
             "AT_SPI_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS", "NO_AT_BRIDGE", "GTK_A11Y", "RAPPEL_DATA_DIR", "HOME",
             "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"}
REAL_DATA = [os.path.join(base, name)
             for base in {os.path.expanduser("~/.local/share"), os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")}
             for name in ("rappel", "rappel-dev", "cards")]

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


class Stop(Exception):
    pass


def on_signal(signum, _frame):
    raise Stop(f"signal {signum}")


class XWindowAttributes(ctypes.Structure):
    _fields_ = [("x", ctypes.c_int), ("y", ctypes.c_int), ("width", ctypes.c_int), ("height", ctypes.c_int),
                ("border_width", ctypes.c_int), ("depth", ctypes.c_int), ("visual", ctypes.c_void_p),
                ("root", ctypes.c_ulong), ("class_", ctypes.c_int), ("bit_gravity", ctypes.c_int),
                ("win_gravity", ctypes.c_int), ("backing_store", ctypes.c_int),
                ("backing_planes", ctypes.c_ulong), ("backing_pixel", ctypes.c_ulong),
                ("save_under", ctypes.c_int), ("colormap", ctypes.c_ulong),
                ("map_installed", ctypes.c_int), ("map_state", ctypes.c_int),
                ("all_event_masks", ctypes.c_long), ("your_event_mask", ctypes.c_long),
                ("do_not_propagate_mask", ctypes.c_long), ("override_redirect", ctypes.c_int),
                ("screen", ctypes.c_void_p)]


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
        self.x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
        self.x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
        self.x11.XDefaultRootWindow.restype = ctypes.c_ulong
        self.x11.XQueryTree.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p, ctypes.c_void_p,
                                        ctypes.c_void_p, ctypes.c_void_p]
        self.x11.XGetWindowAttributes.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p]
        self.x11.XResizeWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_uint, ctypes.c_uint]
        self.x11.XFree.argtypes = [ctypes.c_void_p]
        self.dpy = self.x11.XOpenDisplay(display.encode())
        if not self.dpy:
            raise Stop(f"cannot open {display}")

    def close(self):
        self.x11.XCloseDisplay(self.dpy)

    def code(self, name):
        sym = self.x11.XStringToKeysym(name.encode())
        code = self.x11.XKeysymToKeycode(self.dpy, sym) if sym else 0
        if not code:
            raise Stop(f"unknown key {name!r}")
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

    def resize(self, width, height):
        """Resizes the largest top-level window (the app's: there is no
        window manager, so the app's toplevel is a child of the root)."""
        root = self.x11.XDefaultRootWindow(self.dpy)
        root_ret, parent_ret = ctypes.c_ulong(), ctypes.c_ulong()
        children, count = ctypes.POINTER(ctypes.c_ulong)(), ctypes.c_uint()
        if not self.x11.XQueryTree(self.dpy, root, ctypes.byref(root_ret), ctypes.byref(parent_ret),
                                   ctypes.byref(children), ctypes.byref(count)):
            raise Stop("cannot list windows")
        best, best_area = None, 0
        attrs = XWindowAttributes()
        for i in range(count.value):
            if self.x11.XGetWindowAttributes(self.dpy, children[i], ctypes.byref(attrs)) \
                    and attrs.map_state == 2 and attrs.width * attrs.height > best_area:
                best, best_area = children[i], attrs.width * attrs.height
        if children:
            self.x11.XFree(children)
        if best is None:
            raise Stop("no window to resize")
        self.x11.XResizeWindow(self.dpy, best, width, height)
        self.x11.XFlush(self.dpy)
        time.sleep(0.1)

    def click(self, x, y):
        self.xtst.XTestFakeMotionEvent(self.dpy, -1, x, y, 0)
        self.xtst.XTestFakeButtonEvent(self.dpy, 1, 1, 0)
        self.xtst.XTestFakeButtonEvent(self.dpy, 1, 0, 0)
        self.x11.XFlush(self.dpy)
        time.sleep(0.1)


def start_xvfb(width, height, auth, holder):
    """Starts Xvfb on a display it picks itself; returns ":N". The process
    goes into `holder` at once, so cleanup sees it whatever happens next."""
    read_fd, write_fd = os.pipe()
    xvfb = holder["xvfb"] = subprocess.Popen(["Xvfb", "-displayfd", str(write_fd), "-auth", auth, "-nolisten", "tcp",
                             "-screen", "0", f"{width}x{height}x24"],
                            pass_fds=(write_fd,), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=True)
    os.close(write_fd)
    with os.fdopen(read_fd) as f:
        number = f.readline().strip()
    if not number.isdigit() or xvfb.poll() is not None:
        raise Stop("Xvfb did not start")
    return f":{number}"


def stop(proc):
    if proc is None or proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()


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
    p.add_argument("--decoration-layout", default="",
                   help="GTK's gtk-decoration-layout for the window's buttons, e.g. ':close' as GNOME and "
                        "phones have it (default: GTK's own, with minimize and maximize)")
    a = p.parse_args()
    if not a.out or "/" in a.out or a.out.startswith("."):
        sys.exit("--out is a file name stem, not a path")
    extra = dict(kv.partition("=")[::2] for kv in a.env)
    if PROTECTED & extra.keys():
        sys.exit(f"--env may not set {sorted(PROTECTED & extra.keys())}")
    if a.data:
        source = os.path.realpath(a.data)
        if any(source == d or source.startswith(d + os.sep) for d in map(os.path.realpath, REAL_DATA)):
            sys.exit("refusing a real data directory")
    for tool in ("Xvfb", "xauth", "import"):
        if not shutil.which(tool):
            sys.exit(f"{tool} is not installed")
    width, height = (int(v) for v in a.size.lower().split("x"))
    os.makedirs(SHOTS, exist_ok=True)

    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)
    signal.signal(signal.SIGALRM, on_signal)
    work = tempfile.mkdtemp(prefix="rappel-shot-")
    app = x = log = None
    holder = {"xvfb": None}
    saved = []
    failed = False
    try:
        data = os.path.join(work, "data")
        if a.data:
            # The database files only, never a -wal or -shm of a running app.
            shutil.copytree(a.data, data, ignore=shutil.ignore_patterns("*-wal", "*-shm"))
        else:
            os.makedirs(data, mode=0o700)
            subprocess.run(["cargo", "run", "-q", "--locked", "-p", "rappel-core", "--example", "fill_scratch_db",
                            "--", data] + a.fill.split(), cwd=REPO, check=True, stdout=subprocess.DEVNULL,
                           timeout=600)
        with open(os.path.join(data, "window-state.ini"), "w") as f:
            f.write(f"[window]\nwidth={width}\nheight={height}\nmaximized=false\ndeck={a.deck}\n")
        subprocess.run(["cargo", "build", "-q", "--locked", "-p", "rappel"], cwd=REPO, check=True, timeout=900)

        # From here on the run is timed: building is not.
        signal.alarm(RUN_SECONDS)
        deadline = time.monotonic() + STEP_SECONDS
        auth = os.path.join(work, "Xauthority")
        open(auth, "w").close()
        display = start_xvfb(width, height, auth, holder)
        xvfb = holder["xvfb"]
        subprocess.run(["xauth", "-f", auth, "add", display, ".", secrets.token_hex(16)], check=True, timeout=10)
        os.environ["XAUTHORITY"] = auth  # for this script's own XOpenDisplay and `import`
        home = os.path.join(work, "home")
        env = {k: v for k, v in os.environ.items() if k not in PROTECTED}
        env.update(extra)
        env.update(DISPLAY=display, XAUTHORITY=auth, GDK_BACKEND="x11", GSK_RENDERER="cairo", NO_AT_BRIDGE="1",
                   GTK_A11Y="none", DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent/rappel-harness",
                   DBUS_SYSTEM_BUS_ADDRESS="unix:path=/nonexistent/rappel-harness",
                   ADW_DEBUG_COLOR_SCHEME=f"prefer-{a.scheme}", RAPPEL_DATA_DIR=data, HOME=home,
                   XDG_CONFIG_HOME=f"{home}/.config", XDG_CACHE_HOME=f"{home}/.cache",
                   XDG_DATA_HOME=f"{home}/.local/share", XDG_STATE_HOME=f"{home}/.local/state",
                   XDG_RUNTIME_DIR=os.path.join(work, "run"))
        os.makedirs(env["XDG_RUNTIME_DIR"], mode=0o700)
        if a.decoration_layout:
            if not all(c.isalnum() or c in ":,-_" for c in a.decoration_layout):
                sys.exit("--decoration-layout takes button names, ',' and ':' only")
            gtk_config = os.path.join(env["XDG_CONFIG_HOME"], "gtk-4.0")
            os.makedirs(gtk_config, mode=0o700)
            with open(os.path.join(gtk_config, "settings.ini"), "w") as f:
                f.write(f"[Settings]\ngtk-decoration-layout={a.decoration_layout}\n")
        log = open(os.path.join(SHOTS, a.out + ".log"), "w")
        app = subprocess.Popen([os.path.join(REPO, "target", "debug", "rappel")], env=env, stdout=log, stderr=log,
                               start_new_session=True)
        x = X(display)
        for step in a.steps.split():
            if xvfb.poll() is not None:
                raise Stop("Xvfb exited")
            if app.poll() is not None:
                print(f"the app exited with {app.returncode}; see {log.name}", file=sys.stderr)
                failed = True
                break
            left = deadline - time.monotonic()
            if left <= 0:
                print("time limit reached", file=sys.stderr)
                failed = True
                break
            kind, _, arg = step.partition(":")
            arg = arg.replace("%20", " ")
            if kind == "wait":
                time.sleep(min(float(arg), left))
            elif kind == "shot":
                path = os.path.join(SHOTS, f"{a.out}-{arg}.png" if arg else f"{a.out}.png")
                subprocess.run(["import", "-display", display, "-window", "root", path], check=True, timeout=20)
                saved.append(path)
            elif kind == "key":
                x.combo(arg)
            elif kind == "type":
                x.type(arg)
            elif kind == "resize":
                rw, rh = (int(v) for v in arg.split(","))
                x.resize(rw, rh)
            elif kind == "click":
                cx, cy = (int(v) for v in arg.split(","))
                x.click(cx, cy)
            else:
                raise Stop(f"unknown step {step!r}")
    except Stop as err:
        print(f"stopped: {err}", file=sys.stderr)
        failed = True
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        if x is not None:
            x.close()
        stop(app)
        stop(holder["xvfb"])
        if log is not None:
            log.close()
        shutil.rmtree(work, ignore_errors=True)
    for path in saved:
        print(path)
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
