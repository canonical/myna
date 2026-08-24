#!/usr/bin/env python3
"""Throwaway Tkinter probe of the Myna configuration surface.

Not a product: a design-space instrument. It renders every knob it can find on
the installed snaps, shows what a Settings panel would have to guess because
no config schema exists yet (docs/configuration-api.md 3.4), and puts the live
status and resource cost of each backend next to those knobs.

Reads are unprivileged (`snap run <backend> get|status|list-*`, systemd
accounting, the session bus). Writes go through pkexec and are always shown as
literal commands before they run.

    ./dev/config-ui.py
"""

import json
import os
import pathlib
import queue
import subprocess
import threading
import tkinter as tk
from tkinter import messagebox, ttk

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


class BackendTab(ttk.Frame):
    def __init__(self, master, app, snap):
        super().__init__(master, padding=8)
        self.app, self.snap = app, snap
        self.widgets = {}
        self.data = {}
        self.live = {}

        status = ttk.LabelFrame(self, text="Status", padding=6)
        status.pack(fill="x")
        self.status_labels = {}
        for row, key in enumerate(
            ("service", "engine", "model", "socket", "memory", "peak", "cpu time", "disk", "since")
        ):
            ttk.Label(status, text=key + ":").grid(
                row=row // 3, column=(row % 3) * 2, sticky="e", padx=(8, 2), pady=1
            )
            var = tk.StringVar(value="...")
            ttk.Label(status, textvariable=var, width=34, anchor="w").grid(
                row=row // 3, column=(row % 3) * 2 + 1, sticky="w"
            )
            self.status_labels[key] = var

        self.knobs = ttk.LabelFrame(self, text="Configuration", padding=6)
        self.knobs.pack(fill="both", expand=True, pady=(8, 0))

        actions = ttk.Frame(self)
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
        ttk.Label(
            self, textvariable=self.gap_var, foreground="#a05000", wraplength=880, justify="left"
        ).pack(fill="x")

    # -- rendering -------------------------------------------------------
    def reload(self):
        self.app.spawn(lambda: probe_backend(self.snap), self.render)

    def render(self, data):
        self.data = data
        for child in self.knobs.winfo_children():
            child.destroy()
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
                row=row, column=0, columnspan=5, sticky="w"
            )

        guessed = len(data.get("config", {}))
        self.gap_var.set(
            f"Schema gaps: {guessed} keys rendered with types inferred from their current "
            "values. No titles, descriptions, ranges, enums, defaults, restart flags or "
            "per-option availability are exposed by the snap; the restart column below is a "
            "hardcoded guess. This is exactly what `describe-config` would supply."
        )

    def selector(self, row, key, verb, current, options, notes):
        options = [o for o in options if o]
        if not options:
            return row
        ttk.Label(self.knobs, text=key).grid(row=row, column=0, sticky="w", padx=(0, 6))
        var = tk.StringVar(value=current or "")
        box = ttk.Combobox(self.knobs, textvariable=var, values=options, state="readonly", width=32)
        box.grid(row=row, column=1, sticky="w")
        ttk.Label(self.knobs, text=notes.get(current, ""), foreground="#666").grid(
            row=row, column=2, sticky="w", padx=6
        )
        ttk.Label(self.knobs, text="selector", foreground="#666").grid(
            row=row, column=3, sticky="w"
        )
        ttk.Label(self.knobs, text="restart", foreground="#a05000").grid(
            row=row, column=4, sticky="w"
        )
        self.widgets[key] = ("selector", var, current or "", verb)
        return row + 1

    def knob(self, row, key, value, scope):
        label = key + ("  (advanced)" if key in ADVANCED else "")
        ttk.Label(self.knobs, text=label).grid(row=row, column=0, sticky="w", padx=(0, 6))
        if isinstance(value, bool):
            var = tk.BooleanVar(value=value)
            ttk.Checkbutton(self.knobs, variable=var).grid(row=row, column=1, sticky="w")
        else:
            var = tk.StringVar(value=str(value))
            ttk.Entry(self.knobs, textvariable=var, width=35).grid(row=row, column=1, sticky="w")
        kind = type(value).__name__
        ttk.Label(self.knobs, text=f"inferred {kind}", foreground="#666").grid(
            row=row, column=2, sticky="w", padx=6
        )
        ttk.Label(self.knobs, text=scope, foreground="#666").grid(row=row, column=3, sticky="w")
        ttk.Label(
            self.knobs, text="" if key in NO_RESTART else "restart", foreground="#a05000"
        ).grid(row=row, column=4, sticky="w")
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


class ClientTab(ttk.Frame):
    MODES = ("auto", "streaming", "batch")

    def __init__(self, master, app, wiring):
        super().__init__(master, padding=8)
        self.app = app

        box = ttk.LabelFrame(self, text=f"Dictation client ({CLIENT_SNAP})", padding=6)
        box.pack(fill="x")
        self.vars = {}
        for row, key in enumerate(("state", "audio", "error", "processes", "client RSS")):
            ttk.Label(box, text=key + ":").grid(row=row, column=0, sticky="e", padx=(0, 6))
            var = tk.StringVar(value="...")
            ttk.Label(box, textvariable=var, anchor="w", width=60).grid(
                row=row, column=1, sticky="w"
            )
            self.vars[key] = var
        self.level = ttk.Progressbar(box, maximum=1.0, length=220)
        self.level.grid(row=1, column=2, padx=8)

        mode = ttk.LabelFrame(self, text="streaming_mode (settings.json, unprivileged)", padding=6)
        mode.pack(fill="x", pady=8)
        self.mode = tk.StringVar(value="auto")
        for value in self.MODES:
            ttk.Radiobutton(mode, text=value, value=value, variable=self.mode).pack(
                side="left", padx=6
            )
        ttk.Button(mode, text="Save", command=self.save_mode).pack(side="left", padx=12)
        self.mode_note = tk.StringVar()
        ttk.Label(mode, textvariable=self.mode_note, foreground="#666").pack(side="left")

        wire = ttk.LabelFrame(self, text="Backend wiring (snap connections)", padding=6)
        wire.pack(fill="both", expand=True)
        buttons = ttk.Frame(wire)
        buttons.pack(fill="x")
        self.wire = tk.Text(wire, height=10, wrap="none")
        self.wire.pack(fill="both", expand=True)

        connected = [w for w in wiring if w[0] != "-"]
        unconnected = [w for w in wiring if w[0] == "-"]
        for plug, slot, note in connected:
            self.wire.insert("end", f"{plug:28} -> {slot:28} {note}\n")
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
    def __init__(self):
        super().__init__()
        self.title("Myna configuration (prototype)")
        self.results = queue.Queue()
        self.geometry("980x760")
        self.total_ram = mem_total()

        head = ttk.Frame(self, padding=(8, 6))
        head.pack(fill="x")
        self.headline = tk.StringVar(value="probing...")
        ttk.Label(head, textvariable=self.headline, font=("", 11, "bold")).pack(side="left")
        self.footprint = ttk.Progressbar(head, maximum=max(self.total_ram, 1), length=260)
        self.footprint.pack(side="right")

        self.book = ttk.Notebook(self)
        self.book.pack(fill="both", expand=True, padx=6, pady=6)

        self.log = tk.Text(self, height=7, wrap="none")
        self.log.pack(fill="x", padx=6, pady=(0, 6))
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

        self.drain()
        self.after(200, self.poll)

    def build_system(self):
        frame = ttk.Frame(self.book, padding=8)
        text = tk.Text(frame, wrap="none")
        text.pack(fill="both", expand=True)
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
            self.headline.set(
                f"{len(self.backends)} backend(s): {', '.join(self.backends)}   "
                f"resident {human_bytes(total)} ({share:.1f}% of RAM)"
            )
            self.after(int(FAST_POLL * 1000), self.poll)

        self.spawn(work, done)


if __name__ == "__main__":
    App().mainloop()
