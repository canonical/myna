#!/usr/bin/env python3
"""Throwaway Tkinter probe of the Myna configuration surface.

Not a product: a design-space instrument. It renders every knob it can find on
the installed snaps, shows what a Settings panel would have to guess because
no config schema exists yet (docs/configuration-api.md 3.4), and puts the live
status and resource cost of each backend next to those knobs.

Reads are unprivileged (`snap run <backend> get|status|list-*`, systemd
accounting, the session bus). Writes go through pkexec and are always shown as
literal commands before they run.

    ./dev/config-ui.py --font-size 14     # or Ctrl +/- / Ctrl-0 at runtime
"""

import argparse
import array
import json
import math
import os
import pathlib
import queue
import subprocess
import threading
import tkinter as tk
from tkinter import font as tkfont
from tkinter import messagebox, ttk

# Every Tk named font is rescaled against TkDefaultFont, so one number drives
# the whole window (Text widgets included) and the relative sizes hold.
SCALED_FONTS = (
    "TkDefaultFont",
    "TkTextFont",
    "TkFixedFont",
    "TkMenuFont",
    "TkHeadingFont",
    "TkCaptionFont",
    "TkSmallCaptionFont",
    "TkIconFont",
    "TkTooltipFont",
)
FONT_RANGE = (7, 30)

TIMEOUT = 8
FAST_POLL = 2.0
CLIENT_SNAP = "myna"
SETTINGS = (
    pathlib.Path(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")))
    / "myna"
    / "settings.json"
)

# Keys a real `describe-config` would flag itself. Guessed here, and the guess
# is displayed as a guess: that is the point of the panel.
NO_RESTART = {"sleep-idle-seconds", "verbose"}
ADVANCED = {"ws.unix-socket", "verbose"}


def run(cmd, timeout=TIMEOUT):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError) as exc:
        return 127, "", str(exc)


def jrun(cmd):
    rc, out, err = run(cmd)
    if rc != 0:
        return None
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return None


_CLI_CACHE = {}


def modelctl(snap):
    """The snap's modelctl command. The app name is not always the snap name
    (myna-funasr exposes it as `myna-funasr.funasr`), and nothing declares it."""
    if snap in _CLI_CACHE:
        return _CLI_CACHE[snap]
    candidates = [[snap]]
    for entry in sorted(pathlib.Path("/snap/bin").glob(f"{snap}.*")):
        candidates.append([entry.name])
    for cand in candidates:
        rc, out, _ = run(["snap", "run"] + cand + ["status", "--format=json"], timeout=15)
        if rc == 0 and "entrypoints" in out:
            _CLI_CACHE[snap] = ["snap", "run"] + cand
            return _CLI_CACHE[snap]
    _CLI_CACHE[snap] = ["snap", "run", snap]
    return _CLI_CACHE[snap]


def parse_flat_yaml(text):
    """`modelctl get` emits one flat `key: value` per line."""
    values = {}
    for line in text.splitlines():
        if not line.strip() or line.startswith(("#", " ")) or ":" not in line:
            continue
        key, _, raw = line.partition(":")
        values[key.strip()] = coerce(raw.strip())
    return values


def coerce(raw):
    if raw in ("true", "false"):
        return raw == "true"
    for cast in (int, float):
        try:
            return cast(raw)
        except ValueError:
            pass
    return raw


def human_bytes(n):
    if n is None:
        return "-"
    for unit in ("B", "K", "M", "G", "T"):
        if n < 1024 or unit == "T":
            return f"{n:.0f}{unit}" if unit == "B" else f"{n:.1f}{unit}"
        n /= 1024.0
    return "-"


def mem_total():
    try:
        for line in pathlib.Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                return int(line.split()[1]) * 1024
    except OSError:
        pass
    return 0


# --------------------------------------------------------------------------
# discovery + probes
# --------------------------------------------------------------------------


def discover():
    """Backends are snaps offering a `ubustt-socket` content slot.

    `snap connections --all` prints the interface as plain `content` when the
    slot is unconnected, so match on the slot column, not the interface: an
    installed-but-unwired backend is a state the panel has to show.
    """
    rc, out, _ = run(["snap", "connections", "--all"])
    backends, wiring = set(), []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) < 3 or not parts[2].endswith(":ubustt-socket"):
            continue
        plug, slot = parts[1], parts[2]
        backends.add(slot.split(":")[0])
        wiring.append((plug, slot, parts[3] if len(parts) > 3 else ""))
    return sorted(backends), wiring


def services_of(snap):
    rc, out, _ = run(["snap", "services", snap])
    units = []
    for line in out.splitlines()[1:]:
        parts = line.split()
        if parts and parts[0].startswith(snap + "."):
            units.append("snap." + parts[0] + ".service")
    return units


def unit_stats(units):
    total = {"mem": 0, "peak": 0, "cpu": 0, "state": "inactive", "since": ""}
    for unit in units:
        rc, out, _ = run(
            [
                "systemctl",
                "show",
                unit,
                "-p",
                "MemoryCurrent",
                "-p",
                "MemoryPeak",
                "-p",
                "CPUUsageNSec",
                "-p",
                "ActiveState",
                "-p",
                "ExecMainStartTimestamp",
            ]
        )
        props = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
        for key, prop in (
            ("mem", "MemoryCurrent"),
            ("peak", "MemoryPeak"),
            ("cpu", "CPUUsageNSec"),
        ):
            try:
                total[key] += int(props.get(prop, "0"))
            except ValueError:
                pass
        if props.get("ActiveState") == "active":
            total["state"] = "active"
            total["since"] = props.get("ExecMainStartTimestamp", "")
    return total


def disk_of(snap):
    """Squashfs sizes of the snap and its components, current revision only."""
    try:
        rev = os.path.basename(os.readlink(f"/snap/{snap}/current"))
    except OSError:
        rev = ""
    rc, out, _ = run(["findmnt", "-bnr", "-o", "TARGET,SIZE"])
    seen, total = set(), 0
    for line in out.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        target, size = parts
        if not target.startswith(f"/snap/{snap}/") or target in seen:
            continue
        if rev and not target.endswith("/" + rev):
            continue
        seen.add(target)
        try:
            total += int(size)
        except ValueError:
            pass
    return total


def probe_backend(snap):
    """Everything a Settings panel would need for one backend snap."""
    data = {"snap": snap}
    cli = modelctl(snap)
    data["cli"] = cli
    data["status"] = jrun(cli + ["status", "--format=json"]) or {}
    rc, out, err = run(cli + ["get"])
    data["config"] = parse_flat_yaml(out) if rc == 0 else {}
    data["config_error"] = err.strip() if rc != 0 else ""
    data["models"] = jrun(cli + ["list-models", "--format=json"]) or {}
    data["engines"] = jrun(cli + ["list-engines", "--format=json"]) or {}
    return data


def engine_keys(data):
    """Keys the active engine manifest owns (engine scope), with defaults."""
    active = (data.get("engines") or {}).get("active-engine")
    for engine in (data.get("engines") or {}).get("engines") or []:
        if engine.get("name") == active:
            return engine.get("configurations") or {}
    return {}


def audio_sources():
    """PipeWire capture nodes, for the device knob a Settings panel needs.

    Device selection exists today only as `myna-desktop --target <node.name>`:
    it is in no settings file and in no snap config, so nothing persists it.
    """
    rc, out, _ = run(["pw-dump"], timeout=6)
    try:
        nodes = json.loads(out)
    except (json.JSONDecodeError, TypeError):
        return []
    sources = []
    for node in nodes:
        props = ((node.get("info") or {}).get("props")) or {}
        if props.get("media.class") not in ("Audio/Source", "Audio/Duplex"):
            continue
        name = props.get("node.name")
        if name:
            sources.append((name, props.get("node.description") or name))
    return sources


class MicProbe:
    """Reads the microphone directly through `pw-record`, independent of Myna.

    Deliberately opt-in and never auto-started: a settings panel that opens the
    mic is a privacy surface in its own right (and, confined, would need its own
    `pipewire` plug). Levels are computed here and discarded; no audio is kept.
    """

    RATE = 16000
    CHUNK = 800  # 50 ms of mono s16

    def __init__(self):
        self.proc = None
        self.level = (0.0, 0.0)
        self.error = ""

    def running(self):
        return self.proc is not None and self.proc.poll() is None

    def start(self, target=None):
        self.stop()
        self.error = ""
        cmd = ["pw-record", "--raw", "--rate", str(self.RATE), "--channels", "1", "--format", "s16"]
        if target:
            cmd += ["--target", target]
        try:
            self.proc = subprocess.Popen(
                cmd + ["-"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
            )
        except (OSError, FileNotFoundError) as exc:
            self.error = f"pw-record unavailable: {exc}"
            return
        threading.Thread(target=self.pump, args=(self.proc,), daemon=True).start()

    def pump(self, proc):
        while proc.poll() is None:
            data = proc.stdout.read(self.CHUNK * 2)
            if not data:
                break
            samples = array.array("h")
            samples.frombytes(data[: len(data) // 2 * 2])
            if not samples:
                continue
            rms = math.sqrt(sum(s * s for s in samples) / len(samples)) / 32768
            peak = max(abs(s) for s in samples) / 32768
            self.level = (min(rms, 1.0), min(peak, 1.0))
        self.level = (0.0, 0.0)

    def stop(self):
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            self.proc = None
        self.level = (0.0, 0.0)


def client_state():
    state = {"dbus": {}, "rss": 0, "pids": [], "settings": {}, "settings_error": ""}
    for prop in ("State", "AudioRms", "AudioPeak", "ErrorMessage"):
        rc, out, _ = run(
            [
                "busctl",
                "--user",
                "--json=short",
                "get-property",
                "org.myna.Dictation",
                "/org/myna/Dictation",
                "org.myna.Dictation",
                prop,
            ],
            timeout=3,
        )
        if rc == 0:
            try:
                state["dbus"][prop] = json.loads(out).get("data")
            except json.JSONDecodeError:
                pass
    rc, out, _ = run(["ps", "-o", "pid=,rss=", "-C", "myna-desktop"], timeout=3)
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2:
            state["pids"].append(parts[0])
            state["rss"] += int(parts[1]) * 1024
    try:
        state["settings"] = json.loads(SETTINGS.read_text())
    except FileNotFoundError:
        state["settings_error"] = "no settings.json (defaults apply)"
    except (OSError, json.JSONDecodeError) as exc:
        state["settings_error"] = str(exc)
    return state


# --------------------------------------------------------------------------
# UI
# --------------------------------------------------------------------------


def em(chars):
    """Width of `chars` digits in the current base font: the only unit this UI
    lays out in, so a font-size change moves everything together."""
    return tkfont.nametofont("TkDefaultFont").measure("0") * chars


def line_height():
    return tkfont.nametofont("TkDefaultFont").metrics("linespace")


def scrolled_text(master, height, wrap="none"):
    """A Text with both scrollbars: long output must stay reachable at any size."""
    frame = ttk.Frame(master)
    text = tk.Text(frame, height=height, wrap=wrap, width=1)
    ybar = ttk.Scrollbar(frame, orient="vertical", command=text.yview)
    xbar = ttk.Scrollbar(frame, orient="horizontal", command=text.xview)
    text.configure(yscrollcommand=ybar.set, xscrollcommand=xbar.set)
    text.grid(row=0, column=0, sticky="nsew")
    ybar.grid(row=0, column=1, sticky="ns")
    xbar.grid(row=1, column=0, sticky="ew")
    frame.rowconfigure(0, weight=1)
    frame.columnconfigure(0, weight=1)
    return frame, text


class Scrollable(ttk.Frame):
    """Vertically scrollable tab body.

    Fonts grow, the window does not: without this a larger font simply pushes
    controls off the bottom. Labels registered as elastic re-wrap to a fraction
    of the visible width instead of forcing the body wider than the canvas.
    """

    def __init__(self, master, padding=8):
        super().__init__(master)
        self.canvas = tk.Canvas(self, highlightthickness=0, borderwidth=0)
        bar = ttk.Scrollbar(self, orient="vertical", command=self.canvas.yview)
        self.canvas.configure(yscrollcommand=bar.set)
        self.canvas.grid(row=0, column=0, sticky="nsew")
        bar.grid(row=0, column=1, sticky="ns")
        self.rowconfigure(0, weight=1)
        self.columnconfigure(0, weight=1)

        self.body = ttk.Frame(self.canvas, padding=padding)
        self.window = self.canvas.create_window((0, 0), window=self.body, anchor="nw")
        self.elastic = []
        self.body.bind(
            "<Configure>",
            lambda _e: self.canvas.configure(scrollregion=self.canvas.bbox("all")),
        )
        self.canvas.bind("<Configure>", self.fit)

    def fit(self, event=None):
        width = event.width if event else self.canvas.winfo_width()
        self.canvas.itemconfigure(self.window, width=width)
        self.rewrap(width)

    def rewrap(self, width=None):
        width = width or self.canvas.winfo_width()
        for label, fraction in self.elastic:
            label.configure(wraplength=max(em(12), int(width * fraction) - em(4)))

    def stretchy(self, label, fraction=1.0):
        self.elastic.append((label, fraction))
        return label

    def scroll(self, units):
        self.canvas.yview_scroll(units, "units")


class BackendTab(Scrollable):
    STATUS_FIELDS = ("service", "engine", "model", "socket", "memory", "peak", "cpu", "disk", "up")
    PAIRS_PER_ROW = 2

    def __init__(self, master, app, snap):
        super().__init__(master)
        self.app, self.snap = app, snap
        self.widgets = {}
        self.data = {}
        self.live = {}

        status = ttk.LabelFrame(self.body, text="Status", padding=6)
        status.pack(fill="x")
        self.status_labels = {}
        for index, key in enumerate(self.STATUS_FIELDS):
            row, col = divmod(index, self.PAIRS_PER_ROW)
            ttk.Label(status, text=key + ":").grid(
                row=row, column=col * 2, sticky="ne", padx=(0, 6), pady=1
            )
            var = tk.StringVar(value="...")
            label = ttk.Label(status, textvariable=var, anchor="w", justify="left")
            label.grid(row=row, column=col * 2 + 1, sticky="ew")
            self.stretchy(label, 1.0 / self.PAIRS_PER_ROW)
            self.status_labels[key] = var
        for col in range(self.PAIRS_PER_ROW):
            status.columnconfigure(col * 2 + 1, weight=1)

        self.knobs = ttk.LabelFrame(self.body, text="Configuration", padding=6)
        self.knobs.pack(fill="both", expand=True, pady=(8, 0))
        self.knobs.columnconfigure(1, weight=1)

        actions = ttk.Frame(self.body)
        actions.pack(fill="x", pady=6)
        ttk.Button(actions, text="Apply changes", command=self.apply).pack(side="left")
        ttk.Button(actions, text="Reload", command=self.reload).pack(side="left", padx=4)
        ttk.Button(
            actions,
            text="Restart backend",
            command=lambda: self.app.privileged(
                [["snap", "restart", self.snap]], f"Restart {self.snap}"
            ),
        ).pack(side="left")

        self.gap_var = tk.StringVar()
        gap = ttk.Label(self.body, textvariable=self.gap_var, foreground="#a05000", justify="left")
        gap.pack(fill="x")
        self.stretchy(gap)

    # -- rendering -------------------------------------------------------
    def reload(self):
        self.app.spawn(lambda: probe_backend(self.snap), self.render)

    def render(self, data):
        self.data = data
        for child in self.knobs.winfo_children():
            child.destroy()
        self.elastic = [(w, f) for w, f in self.elastic if w.winfo_exists()]
        self.widgets.clear()

        eng_keys = engine_keys(data)
        models = (data.get("models") or {}).get("models") or []
        engines = (data.get("engines") or {}).get("engines") or []
        row = 0

        row = self.selector(
            row,
            "model",
            "use-model",
            (data.get("models") or {}).get("active-model"),
            [m.get("name") for m in models],
            {m.get("name"): m.get("disk-size", "") for m in models},
        )
        row = self.selector(
            row,
            "engine",
            "use-engine",
            (data.get("engines") or {}).get("active-engine"),
            [e.get("name") for e in engines],
            {
                e.get("name"): ("compatible" if e.get("compatible", True) else "incompatible")
                for e in engines
            },
        )

        for key, value in sorted(data.get("config", {}).items()):
            scope = "engine" if key in eng_keys else "package"
            row = self.knob(row, key, value, scope)

        if data.get("config_error"):
            ttk.Label(self.knobs, text=data["config_error"], foreground="red").grid(
                row=row, column=0, columnspan=3, sticky="w"
            )

        guessed = len(data.get("config", {}))
        self.gap_var.set(
            f"Schema gaps: {guessed} keys rendered with types inferred from their current "
            "values. No titles, descriptions, ranges, enums, defaults, restart flags or "
            "per-option availability are exposed by the snap; the restart marks above are a "
            "hardcoded guess. This is exactly what `describe-config` would supply."
        )
        self.rewrap()

    def meta(self, row, parts):
        """The type/scope/restart column, one wrapping label rather than three
        fixed ones: at 24pt three columns push the controls off the canvas."""
        label = ttk.Label(
            self.knobs, text="  ·  ".join(p for p in parts if p), foreground="#666", justify="left"
        )
        label.grid(row=row, column=2, sticky="w", padx=(8, 0))
        self.stretchy(label, 0.35)

    def selector(self, row, key, verb, current, options, notes):
        options = [o for o in options if o]
        if not options:
            return row
        ttk.Label(self.knobs, text=key).grid(row=row, column=0, sticky="w", padx=(0, 6), pady=2)
        var = tk.StringVar(value=current or "")
        ttk.Combobox(self.knobs, textvariable=var, values=options, state="readonly", width=1).grid(
            row=row, column=1, sticky="ew", pady=2
        )
        self.meta(row, ["selector", "restart", notes.get(current, "")])
        self.widgets[key] = ("selector", var, current or "", verb)
        return row + 1

    def knob(self, row, key, value, scope):
        label = key + ("  (advanced)" if key in ADVANCED else "")
        ttk.Label(self.knobs, text=label).grid(row=row, column=0, sticky="w", padx=(0, 6), pady=2)
        if isinstance(value, bool):
            var = tk.BooleanVar(value=value)
            ttk.Checkbutton(self.knobs, variable=var).grid(row=row, column=1, sticky="w", pady=2)
        else:
            var = tk.StringVar(value=str(value))
            ttk.Entry(self.knobs, textvariable=var, width=1).grid(
                row=row, column=1, sticky="ew", pady=2
            )
        self.meta(
            row,
            [
                f"inferred {type(value).__name__}",
                scope,
                "" if key in NO_RESTART else "restart",
            ],
        )
        self.widgets[key] = ("set", var, value, None)
        return row + 1

    # -- writes ----------------------------------------------------------
    def cli(self):
        return self.data.get("cli") or ["snap", "run", self.snap]

    def apply(self):
        cmds, restart = [], False
        for key, (kind, var, current, verb) in self.widgets.items():
            new = var.get()
            if isinstance(current, bool):
                new = bool(new)
            elif not isinstance(current, str):
                new = coerce(str(new))
            if new == current or new == "":
                continue
            if kind == "selector":
                cmds.append(self.cli() + [verb, str(new)])
                restart = True
            else:
                literal = str(new).lower() if isinstance(new, bool) else str(new)
                cmds.append(self.cli() + ["set", f"{key}={literal}"])
                restart = restart or key not in NO_RESTART
        if not cmds:
            messagebox.showinfo("Nothing to apply", "No values changed.")
            return
        note = (
            "\n\nThe configure hook restarts the backend; the socket drops briefly."
            if restart
            else ""
        )
        self.app.privileged(
            cmds, f"Apply {len(cmds)} change(s) to {self.snap}{note}", after=self.reload
        )

    # -- live poll -------------------------------------------------------
    def update_live(self, live):
        self.live = live
        st = self.data.get("status") or {}
        entry = (st.get("entrypoints") or {}).get("ubustt") or {}
        svc = ", ".join(f"{k}={v}" for k, v in (st.get("services") or {}).items())
        self.status_labels["service"].set(f"{live.get('state', '?')}  ({svc or 'no services'})")
        self.status_labels["engine"].set(st.get("engine", "-"))
        self.status_labels["model"].set((self.data.get("models") or {}).get("active-model", "-"))
        self.status_labels["socket"].set(entry.get("unix-socket", "-"))
        self.status_labels["memory"].set(human_bytes(live.get("mem")))
        self.status_labels["peak"].set(human_bytes(live.get("peak")))
        self.status_labels["cpu time"].set(f"{live.get('cpu', 0) / 1e9:.0f}s")
        self.status_labels["disk"].set(human_bytes(live.get("disk")))
        self.status_labels["since"].set(live.get("since", "-") or "-")


class ClientTab(Scrollable):
    MODES = ("auto", "streaming", "batch")

    def __init__(self, master, app, wiring):
        super().__init__(master)
        self.app = app

        box = ttk.LabelFrame(self.body, text=f"Dictation client ({CLIENT_SNAP})", padding=6)
        box.pack(fill="x")
        box.columnconfigure(1, weight=1)
        self.vars = {}
        for row, key in enumerate(("state", "audio", "error", "processes", "client RSS")):
            ttk.Label(box, text=key + ":").grid(row=row, column=0, sticky="ne", padx=(0, 6))
            var = tk.StringVar(value="...")
            label = ttk.Label(box, textvariable=var, anchor="w", justify="left")
            label.grid(row=row, column=1, sticky="ew")
            self.stretchy(label, 0.7)
            self.vars[key] = var
        self.level = ttk.Progressbar(box, maximum=1.0)
        self.level.grid(row=1, column=2, padx=8, sticky="e")
        app.bars.append((self.level, 18))
        published = ttk.Label(
            box,
            text="AudioRms/AudioPeak are published only while a dictation session is live, and "
            "zeroed when it ends (pump.rs P7). At idle this meter reads zero by contract, and "
            "with no myna-desktop running there is nothing on the bus at all.",
            foreground="#666",
            justify="left",
        )
        published.grid(row=5, column=0, columnspan=3, sticky="ew", pady=(6, 0))
        self.stretchy(published)

        mode = ttk.LabelFrame(
            self.body, text="streaming_mode (settings.json, unprivileged)", padding=6
        )
        mode.pack(fill="x", pady=8)
        choices = ttk.Frame(mode)
        choices.pack(fill="x")
        self.mode = tk.StringVar(value="auto")
        for value in self.MODES:
            ttk.Radiobutton(choices, text=value, value=value, variable=self.mode).pack(
                side="left", padx=(0, 8)
            )
        ttk.Button(choices, text="Save", command=self.save_mode).pack(side="left", padx=8)
        self.mode_note = tk.StringVar()
        note = ttk.Label(mode, textvariable=self.mode_note, foreground="#666", justify="left")
        note.pack(fill="x", pady=(4, 0))
        self.stretchy(note)

        mic = ttk.LabelFrame(self.body, text="Microphone (independent of Myna)", padding=6)
        mic.pack(fill="x", pady=(0, 8))
        mic.columnconfigure(0, weight=1)
        self.probe = MicProbe()
        self.sources = audio_sources()
        self.source = tk.StringVar()
        picker = ttk.Frame(mic)
        picker.grid(row=0, column=0, columnspan=2, sticky="ew")
        picker.columnconfigure(0, weight=1)
        ttk.Combobox(
            picker,
            textvariable=self.source,
            values=[f"{desc}  ({name})" for name, desc in self.sources],
            state="readonly",
            width=1,
        ).grid(row=0, column=0, sticky="ew")
        self.test_button = ttk.Button(picker, text="Test mic", command=self.toggle_mic)
        self.test_button.grid(row=0, column=1, padx=(8, 0))
        if self.sources:
            self.source.set(f"{self.sources[0][1]}  ({self.sources[0][0]})")

        self.mic_level = ttk.Progressbar(mic, maximum=1.0)
        self.mic_level.grid(row=1, column=0, sticky="ew", pady=(6, 0))
        app.bars.append((self.mic_level, 30))
        self.mic_note = tk.StringVar(
            value="Opens the mic directly through pw-record, so it answers 'is my microphone "
            "working' without Myna installed. Device choice is a real config knob with no home: "
            "it exists only as `myna-desktop --target <node.name>`, persisted nowhere."
        )
        note_label = ttk.Label(mic, textvariable=self.mic_note, foreground="#666", justify="left")
        note_label.grid(row=2, column=0, columnspan=2, sticky="ew", pady=(4, 0))
        self.stretchy(note_label)
        self.tick()

        wire = ttk.LabelFrame(self.body, text="Backend wiring (snap connections)", padding=6)
        wire.pack(fill="both", expand=True)
        buttons = ttk.Frame(wire)
        buttons.pack(fill="x")
        frame, self.wire = scrolled_text(wire, height=10)
        frame.pack(fill="both", expand=True)

        connected = [w for w in wiring if w[0] != "-"]
        unconnected = [w for w in wiring if w[0] == "-"]
        for plug, slot, note_text in connected:
            self.wire.insert("end", f"{plug:28} -> {slot:28} {note_text}\n")
        for _, slot, _ in unconnected:
            self.wire.insert(
                "end", f"{'(unconnected)':28}    {slot:28} backend is installed but unwired\n"
            )
            ttk.Button(
                buttons,
                text=f"Connect {slot.split(':')[0]}",
                command=lambda s=slot: app.privileged(
                    [["snap", "connect", f"{CLIENT_SNAP}:backend", s]],
                    f"Wire {CLIENT_SNAP} to {s}",
                ),
            ).pack(side="left", padx=(0, 6))

        if not wiring:
            self.wire.insert(
                "end",
                "No inference snap is installed. Install one (whisper, parakeet, funasr, ...) "
                "and connect it: snap connect myna:backend <snap>:ubustt-socket\n",
            )

        plugs = [w[0] for w in connected]
        if len(plugs) != len(set(plugs)):
            self.wire.insert(
                "end",
                "\nWARNING: one `backend` plug is connected to several backends. The launcher "
                "resolves a single $SNAP_DATA/backend/run/ubustt.sock, so which backend actually "
                "serves dictation is not visible or selectable here. A config API needs an "
                "explicit 'active backend' concept.\n",
            )
        if unconnected:
            self.wire.insert(
                "end",
                "\nAn unconnected backend is invisible to dictation, yet its service still runs "
                "and holds its model resident. Connection state belongs in the same panel as the "
                "knobs: it is the difference between a configured backend and a used one.\n",
            )
        self.wire.configure(state="disabled")

    def toggle_mic(self):
        if self.probe.running():
            self.probe.stop()
            self.test_button.configure(text="Test mic")
            return
        selected = self.source.get()
        target = selected.rsplit("(", 1)[-1].rstrip(")") if "(" in selected else None
        self.probe.start(target)
        self.test_button.configure(text="Stop test")
        if self.probe.error:
            self.mic_note.set(self.probe.error)
            self.test_button.configure(text="Test mic")

    def tick(self):
        """A VU needs a faster cadence than the 2 s system poll."""
        rms, peak = self.probe.level
        self.mic_level["value"] = peak
        if self.probe.running():
            self.mic_note.set(f"live: rms {rms:.3f}  peak {peak:.3f}  (pw-record, 16 kHz mono)")
        elif self.test_button["text"] == "Stop test":
            self.test_button.configure(text="Test mic")
        self.after(100, self.tick)

    def save_mode(self):
        try:
            SETTINGS.parent.mkdir(parents=True, exist_ok=True)
            doc = {}
            if SETTINGS.exists():
                doc = json.loads(SETTINGS.read_text())
            doc["streaming_mode"] = self.mode.get()
            SETTINGS.write_text(json.dumps(doc, indent=2) + "\n")
            self.mode_note.set(f"written to {SETTINGS}; takes effect on next dictation run")
        except (OSError, json.JSONDecodeError) as exc:
            messagebox.showerror("Save failed", str(exc))

    def update_live(self, state):
        dbus = state.get("dbus") or {}
        self.vars["state"].set(dbus.get("State", "no org.myna.Dictation on the session bus"))
        rms, peak = dbus.get("AudioRms", 0.0) or 0.0, dbus.get("AudioPeak", 0.0) or 0.0
        self.vars["audio"].set(f"rms {rms:.3f}  peak {peak:.3f}")
        self.level["value"] = peak
        self.vars["error"].set(dbus.get("ErrorMessage") or "-")
        self.vars["processes"].set(", ".join(state.get("pids") or []) or "not running")
        self.vars["client RSS"].set(human_bytes(state.get("rss")))
        if not self.mode_note.get():
            self.mode.set((state.get("settings") or {}).get("streaming_mode", "auto"))
            self.mode_note.set(state.get("settings_error") or f"loaded from {SETTINGS}")


class App(tk.Tk):
    def __init__(self, font_size=None):
        super().__init__()
        self.title("Myna configuration (prototype)")
        self.results = queue.Queue()
        self.total_ram = mem_total()
        self.bars = []

        self.base_sizes = {
            name: abs(tkfont.nametofont(name).cget("size")) or 10 for name in SCALED_FONTS
        }
        self.headline_font = tkfont.Font(name="MynaHeadline", exists=False)
        self.headline_font.configure(weight="bold")
        self.font_size = tk.IntVar(value=font_size or self.base_sizes["TkDefaultFont"])
        self.rescale(resize_window=False)
        for seq in ("<Control-plus>", "<Control-equal>", "<Control-KP_Add>"):
            self.bind_all(seq, lambda _e: self.bump(1))
        for seq in ("<Control-minus>", "<Control-KP_Subtract>"):
            self.bind_all(seq, lambda _e: self.bump(-1))
        self.bind_all("<Control-0>", lambda _e: self.bump(0))
        self.bind_all("<Button-4>", lambda e: self.wheel(e, -3))
        self.bind_all("<Button-5>", lambda e: self.wheel(e, 3))
        self.bind_all("<MouseWheel>", lambda e: self.wheel(e, -3 if e.delta > 0 else 3))
        self.geometry(
            f"{min(em(88), int(self.winfo_screenwidth() * 0.7))}x"
            f"{min(line_height() * 40, int(self.winfo_screenheight() * 0.8))}"
        )

        head = ttk.Frame(self, padding=(8, 6))
        head.pack(fill="x")
        head.columnconfigure(0, weight=1)
        self.headline = tk.StringVar(value="probing...")
        self.headline_label = ttk.Label(
            head, textvariable=self.headline, font=self.headline_font, justify="left"
        )
        self.headline_label.grid(row=0, column=0, sticky="w")
        zoom = ttk.Frame(head)
        zoom.grid(row=0, column=1, sticky="e", padx=8)
        ttk.Button(zoom, text="A-", width=3, command=lambda: self.bump(-1)).pack(side="left")
        ttk.Label(zoom, textvariable=self.font_size, width=3, anchor="center").pack(side="left")
        ttk.Button(zoom, text="A+", width=3, command=lambda: self.bump(1)).pack(side="left")
        self.footprint = ttk.Progressbar(head, maximum=max(self.total_ram, 1))
        self.footprint.grid(row=0, column=2, sticky="e")
        self.bars.append((self.footprint, 22))
        head.bind(
            "<Configure>",
            lambda e: self.headline_label.configure(wraplength=max(em(20), e.width - em(34))),
        )

        self.book = ttk.Notebook(self)
        self.book.pack(fill="both", expand=True, padx=6, pady=6)

        log_frame, self.log = scrolled_text(self, height=5)
        log_frame.pack(fill="x", padx=6, pady=(0, 6))
        self.say("reads are unprivileged; every write is shown before it runs")

        self.backends, wiring = discover()
        self.tabs = {}
        for snap in self.backends:
            tab = BackendTab(self.book, self, snap)
            self.book.add(tab, text=snap)
            self.tabs[snap] = tab
            tab.reload()
        self.client_tab = ClientTab(self.book, self, wiring)
        self.book.add(self.client_tab, text="client")
        self.system_tab = self.build_system()
        self.book.add(self.system_tab, text="machine")

        self.protocol("WM_DELETE_WINDOW", self.close)
        self.drain()
        self.after(200, self.poll)

    def close(self):
        self.client_tab.probe.stop()
        self.destroy()

    def build_system(self):
        frame = ttk.Frame(self.book, padding=8)
        inner, text = scrolled_text(frame, height=20)
        inner.pack(fill="both", expand=True)
        chunks = []
        for snap in self.backends:
            rc, out, _ = run(modelctl(snap) + ["show-machine"])
            chunks.append(f"### {snap} show-machine\n{out}")
            break  # the machine is the same for every snap
        rc, out, _ = run(["snap", "list"])
        wanted = set(self.backends) | {CLIENT_SNAP}
        mine = [line for line in out.splitlines() if line.split() and line.split()[0] in wanted]
        chunks.append("### installed Myna snaps\n" + "\n".join(mine))
        text.insert("end", "\n\n".join(chunks))
        text.configure(state="disabled")
        return frame

    def wheel(self, event, units):
        """Scroll whichever tab body the pointer is over."""
        widget = event.widget
        while widget is not None:
            if isinstance(widget, Scrollable):
                widget.scroll(units)
                return
            widget = getattr(widget, "master", None)

    def bump(self, step):
        """Ctrl +/-; Ctrl-0 restores the desktop's own size."""
        base = self.base_sizes["TkDefaultFont"]
        target = base if step == 0 else self.font_size.get() + step
        self.font_size.set(max(FONT_RANGE[0], min(FONT_RANGE[1], target)))
        self.rescale()

    def rescale(self, resize_window=True):
        """Resize every named font proportionally to the requested base size,
        then re-derive everything measured in pixels from the new font."""
        base = self.base_sizes["TkDefaultFont"]
        ratio = self.font_size.get() / base
        for name, size in self.base_sizes.items():
            tkfont.nametofont(name).configure(size=max(FONT_RANGE[0], round(size * ratio)))
        self.headline_font.configure(size=round(self.font_size.get() * 1.15))
        for bar, chars in self.bars:
            bar.configure(length=em(chars))
        if not resize_window:
            return
        for tab in list(getattr(self, "tabs", {}).values()) + [getattr(self, "client_tab", None)]:
            if tab is not None:
                tab.fit()

    def say(self, line):
        self.log.insert("end", line.rstrip() + "\n")
        self.log.see("end")

    def spawn(self, work, done):
        """Probe off the main thread; results land back through the queue that
        `drain` polls, so no worker ever touches a Tk call."""

        def runner():
            try:
                result = work()
            except Exception as exc:  # a probe must never kill the UI
                self.results.put((lambda _e=exc: self.say(f"probe failed: {_e}"), None))
                return
            self.results.put((done, result))

        threading.Thread(target=runner, daemon=True).start()

    def drain(self):
        while True:
            try:
                done, result = self.results.get_nowait()
            except queue.Empty:
                break
            done(result) if result is not None else done()
        self.after(80, self.drain)

    def privileged(self, cmds, prompt, after=None):
        preview = "\n".join("pkexec " + " ".join(c) for c in cmds)
        if not messagebox.askokcancel("Privileged write", f"{prompt}\n\n{preview}"):
            return

        def work():
            results = []
            for cmd in cmds:
                rc, out, err = run(["pkexec"] + cmd, timeout=300)
                results.append((cmd, rc, out, err))
            return results

        def done(results):
            for cmd, rc, out, err in results:
                self.say(f"$ pkexec {' '.join(cmd)}  -> rc={rc}")
                for stream in (out, err):
                    if stream.strip():
                        self.say("  " + stream.strip().replace("\n", "\n  "))
            if after:
                after()

        self.spawn(work, done)

    def poll(self):
        def work():
            live = {}
            for snap in self.backends:
                stats = unit_stats(services_of(snap))
                stats["disk"] = disk_of(snap)
                live[snap] = stats
            return live, client_state()

        def done(result):
            live, client = result
            total = 0
            for snap, stats in live.items():
                self.tabs[snap].update_live(stats)
                total += stats.get("mem", 0)
            total += client.get("rss", 0)
            self.client_tab.update_live(client)
            self.footprint["value"] = min(total, self.total_ram)
            share = 100.0 * total / self.total_ram if self.total_ram else 0
            found = (
                f"{len(self.backends)} backend(s): {', '.join(self.backends)}"
                if self.backends
                else "no backend snap found (nothing offers a ubustt-socket slot)"
            )
            self.headline.set(f"{found}   resident {human_bytes(total)} ({share:.1f}% of RAM)")
            self.after(int(FAST_POLL * 1000), self.poll)

        self.spawn(work, done)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--font-size",
        type=int,
        metavar="PT",
        help="base font size in points (default: the desktop's own). "
        "Ctrl+plus / Ctrl+minus / Ctrl+0 adjust it live.",
    )
    args = parser.parse_args()
    App(font_size=args.font_size).mainloop()


if __name__ == "__main__":
    main()
