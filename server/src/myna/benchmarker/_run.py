"""Snap sweep runner: the one place a benchmark matrix is produced.

One YAML file lists the *snaps* to benchmark. For each one the runner purges any
existing install, sideloads the packed snap plus its components, selects an
engine by hardware detection, then sweeps **every model variant that engine
offers** x **every emission mode the snap exposes** x **every config point the
target declares** - each with a **cold** sample (model-load-from-cold) and a
**warm** sweep - and purges again.

Every combination is its own row, never an average - the matrix exists to show
the shape of the trade-off:

- **model variant** (``whisper tiny/base/small``): an order of magnitude apart
  in both accuracy and cost. ``list-models`` reports the options, so the config
  does not name them (a ``models:`` allowlist can narrow the sweep).
- **emission mode** (batch / streaming): a shipped configuration toggle
  (``modelctl set streaming=``), so both settings are real user-facing
  configurations. Snaps whose adapter is commit-on-finalize only (funasr,
  audio8, qwen-c) expose no such key and are swept batch-only.
- **config point** (``configs:``): any other shipped knob worth a row -
  whisper's ``compute-type`` (the quantization axis), parakeet's
  ``stream-arm-seconds``, nemotron's ``att-context-size``. Each entry names the
  modes it applies to, so a batch-only knob is sweepable and a latency dial
  does not multiply the batch rows.

Labels come out as ``<snap>/<engine>/<model>/<mode>[-<config>]``.

    sudo myna-bench run --config bench.yaml
    sudo myna-bench run --config bench.yaml --only myna-whisper
    myna-bench plan --config bench.yaml          # no root, installs nothing

**Snaps only, by design.** Benchmarking a ``myna-server`` spawned from a venv
measured something we do not ship: different confinement, different engine
selection, different resident set. The only configuration that means anything
is the one a user installs.

**The label is an output, not an input.** ``use-engine --auto`` chooses the
engine by hardware detection, not this file; the runner reads it back with
``show-engine`` and stamps ``<snap>/<engine>/<model>`` onto every record. A
config that named the engine could only ever disagree with reality. The one
input is ``--label-suffix``, stamped as ``<snap>+<suffix>``: two builds of the
same snap (e.g. base vs maxstack encoder) are indistinguishable from inside,
and the summary dedups by label, so without it a rebuild silently shadows the
run it was meant to be compared against.

**Purge between targets.** ``snap remove --purge`` drops $SNAP_COMMON, so each
target re-runs auto-selection from clean rather than inheriting whatever engine
was last active. It also guarantees one resident model at a time: backends
idle-unload on a timer (``sleep-idle-seconds``, 300 by default), so without a
purge a finished backend keeps its weights in RAM while the next one loads.

**Usability budget.** A backend slower than the budget is a product failure,
not a datapoint to wait for. The warm sweep runs under a wall-clock deadline;
overrunning it stops the sweep and stamps the target ``usability_fail`` with the
clips it managed. Measured per run, never predicted - a backend gets to prove
itself on the actual hardware.

Config format::

    manifest: ./corpus/manifest.json
    out: ./results.jsonl
    root: .                       # base for relative paths (default: config's dir)
    cold_clip: librispeech-84-121123-0000  # one clip, run first, tagged cold
    clips: []                     # warm sweep clips; omit = whole manifest
    sweep_budget_seconds: 600

    targets:
      - snap: myna-whisper
        # Either an explicit artefact list...
        files:
          - ./snaps/myna-whisper_1.0_amd64.snap
          - ./snaps/myna-whisper+model-tiny.comp
        # ...or a source-tree directory, whose packed snap and snapcraft.yaml
        # declared components are globbed and validated.
        dir: ../whisper-snap
        cli: myna-whisper.whisper   # modelctl command (default: derived, then the snap name)
        service: myna-whisper.server
        socket: /var/snap/myna-whisper/common/run/ubustt.sock
        models: [tiny, base]        # optional allowlist
        configs:
          - label: int8
            modes: [batch]
            settings: {compute-type: int8}
"""

from __future__ import annotations

import asyncio
import json
import os
import pwd
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

import yaml

DEFAULT_SWEEP_BUDGET_S = 600.0

BATCH = "batch"
STREAMING = "streaming"
MODES = (BATCH, STREAMING)

# Snaps this runner is allowed to remove. Purging is destructive (it drops
# $SNAP_COMMON and $SNAP_DATA), so it is restricted to the backends this project
# builds. Anything else on the machine is off limits, whatever the config says.
PURGEABLE = frozenset(
    {
        "myna-whisper",
        "myna-parakeet",
        "myna-sherpa",
        "myna-qwen",
        "myna-nemotron",
        "myna-funasr",
        "myna-fake-backend",
        "myna-audio8",
    }
)


class SweepOverran(Exception):
    """The warm sweep exceeded its wall-clock budget."""


class TargetUnavailable(Exception):
    """This target's artefacts are missing; the others can still run."""


def wait_for_socket(path: Path, timeout: float = 120.0) -> bool:
    """Poll until the snap has bound the UDS (the file appears), or timeout.

    The server creates the socket file only once it is listening, so its
    existence is a sufficient readiness signal - and unlike a bare connect()
    probe it does not trip the websockets server's "invalid HTTP request"
    handshake handler (a zero-byte connect-and-close looks like a broken client).
    A short settle covers the gap between bind and the accept loop being ready.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            time.sleep(0.1)
            return True
        time.sleep(0.2)
    return False


def _run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, **kw)


def _capture(cmd: list[str], timeout: float = 30.0) -> subprocess.CompletedProcess:
    """Run a probe and never raise: a missing tool is an answer, not a crash.

    Everything that goes through here is asking the machine a question it is
    allowed to decline - which models an engine offers, whether a config key
    exists, what the daemon's PID is. A snap CLI that is not installed, or a
    systemctl that is not there, has to read as "no answer" so the caller can
    fall back, rather than taking the sweep down from inside a probe.
    """
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=False)
    except (OSError, subprocess.SubprocessError) as exc:
        return subprocess.CompletedProcess(cmd, 1, stdout="", stderr=str(exc))


def _gpu_memory_by_pid() -> dict[int, int]:
    """pid -> VRAM MiB, from nvidia-smi. Empty if no GPU / tool absent."""
    try:
        out = subprocess.run(
            [
                "nvidia-smi",
                "--query-compute-apps=pid,used_memory",
                "--format=csv,noheader,nounits",
            ],
            capture_output=True,
            text=True,
            timeout=5,
            check=True,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return {}
    usage: dict[int, int] = {}
    for line in out.splitlines():
        pid, _, mem = line.partition(",")
        try:
            usage[int(pid.strip())] = int(mem.strip())
        except ValueError:
            continue
    return usage


class ResourceSampler(threading.Thread):
    """Sample peak RSS (and VRAM) of a process tree until stopped."""

    def __init__(self, pid: int, interval: float = 0.5):
        super().__init__(daemon=True)
        self.pid = pid
        self.interval = interval
        self._stop_event = threading.Event()
        self.peak_rss_mb = 0.0
        self.peak_vram_mb: float | None = None

    def _tree(self):
        try:
            import psutil

            root = psutil.Process(self.pid)
            return [root, *root.children(recursive=True)]
        except Exception:  # noqa: BLE001
            return []

    def run(self) -> None:
        while not self._stop_event.is_set():
            procs = self._tree()
            rss = 0
            for pr in procs:
                try:
                    rss += pr.memory_info().rss
                except Exception:  # noqa: BLE001
                    pass
            self.peak_rss_mb = max(self.peak_rss_mb, rss / 1e6)
            gpu = _gpu_memory_by_pid()
            if gpu:
                pids = {pr.pid for pr in procs}
                mine = sum(m for p, m in gpu.items() if p in pids)
                if mine:
                    self.peak_vram_mb = max(self.peak_vram_mb or 0.0, float(mine))
            self._stop_event.wait(self.interval)

    def stop(self) -> None:
        self._stop_event.set()
        self.join(timeout=2)


def _chown_to_invoker(path: Path) -> None:
    """Hand a file back to the human, so the next non-sudo run can append."""
    uid = os.environ.get("SUDO_UID")
    gid = os.environ.get("SUDO_GID")
    if not uid or not gid or not path.exists():
        return
    os.chown(path, int(uid), int(gid))


def _resolve_user_home() -> None:
    """Point HOME at the invoking user so caches are not left root-owned."""
    uid = os.environ.get("SUDO_UID")
    if uid:
        try:
            os.environ["HOME"] = pwd.getpwuid(int(uid)).pw_dir
        except (KeyError, ValueError):
            pass


# ---------------------------------------------------------------------------
# Source-tree targets
# ---------------------------------------------------------------------------


def _declared_components(snap_dir: Path) -> set[str]:
    """Component names from the project's snapcraft.yaml.

    The .comp files on disk are not authoritative: a directory accumulates
    artifacts from branches and renames (myna-qwen+qwen-vllm.comp outlived the
    vLLM branch by two months). Installing an undeclared component fails, so
    trust the manifest and ignore the debris.
    """
    recipe = snap_dir / "snap" / "snapcraft.yaml"
    if not recipe.exists():
        return set()
    parsed = yaml.safe_load(recipe.read_text(encoding="utf-8")) or {}
    return set(parsed.get("components") or {})


def _snap_files(snap_dir: Path, snap: str) -> list[str]:
    """The packed snap plus the components its snapcraft.yaml declares.

    Components are sideloaded in the same ``snap install`` invocation as the
    snap they belong to; snapd resolves ``<snap>+<component>.comp`` by name.
    """
    packed = sorted(snap_dir.glob(f"{snap}_*.snap"))
    if not packed:
        raise TargetUnavailable(f"no {snap}_*.snap in {snap_dir} - pack it first")
    if len(packed) > 1:
        raise TargetUnavailable(f"several {snap}_*.snap in {snap_dir}: {[p.name for p in packed]}")
    declared = _declared_components(snap_dir)
    comps = [
        p for p in sorted(snap_dir.glob(f"{snap}+*.comp")) if p.stem.split("+", 1)[1] in declared
    ]
    missing = declared - {p.stem.split("+", 1)[1] for p in comps}
    if missing:
        raise TargetUnavailable(
            f"{snap}: declared components not packed: {sorted(missing)} - repack"
        )
    return [str(p) for p in [*packed, *comps]]


def _modelctl_command(snap_dir: Path, snap: str) -> str:
    """The command that invokes this snap's modelctl CLI.

    Snapd exposes an app as a bare ``<snap>`` only when the app name matches the
    snap name, and as ``<snap>.<app>`` otherwise. ``myna-funasr`` names its CLI
    app ``funasr``, so its command is ``myna-funasr.funasr`` - assuming the snap
    name works for whisper and qwen and fails for funasr, which is exactly what
    it did. The CLI app is the non-daemon one.
    """
    recipe = snap_dir / "snap" / "snapcraft.yaml"
    if recipe.exists():
        parsed = yaml.safe_load(recipe.read_text(encoding="utf-8")) or {}
        for name, app in (parsed.get("apps") or {}).items():
            if isinstance(app, dict) and "daemon" not in app:
                return name if name == snap else f"{snap}.{name}"
    return snap


def declares_streaming(snap_dir: Path) -> bool | None:
    """Whether the snap's install hook declares the ``streaming`` config key.

    The key *is* the capability declaration - a snap whose adapter has no
    progressive path never sets it. The hook is the only static record of that,
    so this is how ``plan`` predicts the mode axis without installing anything.
    Returns None when there is no hook to read (an artefact-only target).
    """
    hook = snap_dir / "snap" / "hooks" / "install"
    if not hook.exists():
        return None
    return "streaming=" in hook.read_text(encoding="utf-8")


def engine_options(snap_dir: Path) -> dict[str, dict]:
    """``{engine: {"models": [...], "configurations": {...}}}`` from engine.yaml.

    Static counterpart to ``list-models`` for ``plan``. Which engine is *active*
    is decided by hardware detection at run time, so the plan reports every
    engine the snap ships and says the choice is made on the machine.
    """
    engines_dir = snap_dir / "engines"
    if not engines_dir.is_dir():
        return {}
    out: dict[str, dict] = {}
    for entry in sorted(p for p in engines_dir.iterdir() if p.is_dir()):
        manifest = entry / "engine.yaml"
        if not manifest.exists():
            continue
        parsed = yaml.safe_load(manifest.read_text(encoding="utf-8")) or {}
        out[parsed.get("name") or entry.name] = {
            "models": list((parsed.get("model") or {}).get("options") or []),
            "configurations": dict(parsed.get("configurations") or {}),
        }
    return out


# ---------------------------------------------------------------------------
# Config points
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Variant:
    """One config point: a named settings assignment and the modes it applies to.

    ``modes`` exists because knobs are not all latency dials. ``compute-type``
    changes batch decoding and streaming alike; ``stream-arm-seconds`` means
    nothing in batch mode and sweeping it there would triple the rows for three
    identical numbers.
    """

    label: str
    modes: tuple[str, ...]
    settings: dict[str, str] = field(default_factory=dict)


def parse_variants(spec: dict, snap: str) -> list[Variant]:
    """Read and validate a target's ``configs:`` list."""
    if "streaming_configs" in spec:
        raise SystemExit(
            f"{snap}: 'streaming_configs' was replaced by 'configs', which names the modes "
            "each entry applies to. Rewrite as:\n"
            "  configs:\n"
            "    - label: arm3s\n"
            "      modes: [streaming]\n"
            "      settings: {stream-arm-seconds: '3'}"
        )
    variants: list[Variant] = []
    seen: set[str] = set()
    for entry in spec.get("configs") or []:
        label = entry.get("label")
        if not label:
            raise SystemExit(f"{snap}: every entry in configs: needs a label")
        if label in seen:
            raise SystemExit(f"{snap}: duplicate config label {label!r}")
        seen.add(label)
        modes = tuple(entry.get("modes") or MODES)
        bad = [m for m in modes if m not in MODES]
        if bad:
            raise SystemExit(f"{snap}/{label}: unknown mode(s) {bad}; expected {list(MODES)}")
        settings = {str(k): str(v) for k, v in (entry.get("settings") or {}).items()}
        if not settings:
            raise SystemExit(f"{snap}/{label}: configs entries need settings to be worth a row")
        if "streaming" in settings:
            raise SystemExit(
                f"{snap}/{label}: 'streaming' is the mode axis, not a setting - "
                "use modes: [batch] / [streaming] instead"
            )
        variants.append(Variant(label=label, modes=modes, settings=settings))
    return variants


def variants_for(variants: list[Variant], mode: str) -> list[Variant | None]:
    """Config points applying to ``mode``; ``[None]`` (shipped defaults) if none.

    A mode no config mentions still gets exactly one row, at whatever the snap
    shipped - never zero rows, which would silently drop half the matrix.
    """
    applicable = [v for v in variants if mode in v.modes]
    return list(applicable) if applicable else [None]


# ---------------------------------------------------------------------------
# SnapTarget
# ---------------------------------------------------------------------------


class SnapTarget:
    """A packed snap: purge, sideload, measure, purge."""

    def __init__(self, spec: dict, root: Path, label_suffix: str = ""):
        self.snap: str = spec["snap"]
        if self.snap not in PURGEABLE:
            raise SystemExit(
                f"{self.snap!r} is not in the purge allowlist {sorted(PURGEABLE)} - "
                "this runner removes what it benchmarks, so it refuses unknown snaps"
            )
        self.label_suffix = label_suffix
        self.dir: Path | None = (root / spec["dir"]).resolve() if spec.get("dir") else None
        raw_files = spec.get("files") or []
        if raw_files:
            self.files: list[str] = [str((root / f).resolve()) for f in raw_files]
        elif self.dir is not None:
            self.files = _snap_files(self.dir, self.snap)
        else:
            raise SystemExit(f"{self.snap}: target needs either files: or dir:")
        self.cli: str = spec.get("cli") or (
            _modelctl_command(self.dir, self.snap) if self.dir else self.snap
        )
        self.service: str = spec.get("service") or f"{self.snap}.server"
        self.socket: Path = Path(
            spec.get("socket") or f"/var/snap/{self.snap}/common/run/ubustt.sock"
        )
        # Optional allowlist: which model variants to sweep. Omitted = every
        # option the active engine declares.
        self.only_models: list[str] = list(spec.get("models") or [])
        self.variants: list[Variant] = parse_variants(spec, self.snap)
        # Filled in after install, from the snap itself. The config never says.
        self.engine: str | None = None
        self.model: str | None = None
        self.streaming: bool = False
        self.config_suffix: str = ""
        # Shipped value of every key any config touches, read once after
        # install so a config that omits a key restores it rather than
        # inheriting the previous config's value.
        self._baseline: dict[str, str] = {}

    # -- identity ---------------------------------------------------------

    @property
    def label(self) -> str:
        """``<snap>[+suffix]/<engine>/<model>/<mode>[-<config>]``.

        The engine is whatever auto-selection landed on; the model is whichever
        variant the sweep is currently on. Both are read back or set by the
        runner, never taken from config.
        """
        snap = f"{self.snap}+{self.label_suffix}" if self.label_suffix else self.snap
        parts = [snap, self.engine or "unknown-engine"]
        if self.model:
            parts.append(self.model)
        mode = STREAMING if self.streaming else BATCH
        if self.config_suffix:
            mode = f"{mode}-{self.config_suffix}"
        parts.append(mode)
        return "/".join(parts)

    # -- lifecycle --------------------------------------------------------

    def purge(self) -> None:
        """Remove the snap and its data. A no-op if it was never installed."""
        subprocess.run(
            ["snap", "remove", "--purge", self.snap],
            capture_output=True,
            text=True,
            check=False,
        )

    def start(self) -> None:
        self.purge()
        print(
            f"[{self.snap}] installing {len(self.files)} file(s): "
            f"{[Path(f).name for f in self.files]}"
        )
        _run(["snap", "install", "--dangerous", *self.files])
        self._connect_plugs()
        # Install left no active engine, so the daemon is crash-looping toward
        # its systemd start limit. Stop it, then clear the failure state, both
        # *before* selecting an engine: `use-engine` restarts the snap itself
        # and reports the whole selection as failed if systemd refuses. Stop
        # first, or the loop can re-fail between the reset and the start.
        subprocess.run(["snap", "stop", self.service], capture_output=True, check=False)
        self._reset_failed()
        self._select_engine()
        subprocess.run(["snap", "start", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            raise SystemExit(
                f"[{self.snap}] socket {self.socket} did not appear - "
                f"check: journalctl -u snap.{self.service}"
            )
        self._describe()
        self._read_baseline()

    def stop(self) -> None:
        self.purge()

    def _connect_plugs(self) -> None:
        """Connect the interfaces a sideloaded snap does not get automatically.

        ``snap install --dangerous`` carries no snap declaration, so
        manual-connect plugs stay unconnected. Key ones that matter:
        - ``hardware-observe``: the install hook checks ``snapctl is-connected
          hardware-observe`` before running ``use-engine --auto``; without it the
          snap installs with no active engine and the daemon exits 1 every start.
        - ``system-observe``: lets the inference runtime read /sys/fs/cgroup/**
          and /proc/** to size its thread pool correctly.
        """
        out = _capture(["snap", "connections", self.snap]).stdout
        for line in out.splitlines()[1:]:  # skip the header row
            parts = line.split()
            # Columns: Interface, Plug, Slot, Notes. An unconnected plug has "-"
            # for its slot; a plug of "-" means the row is a slot this snap offers.
            if len(parts) >= 3 and parts[2] == "-" and parts[1] != "-":
                print(f"[{self.snap}] connecting {parts[1]}")
                subprocess.run(["snap", "connect", parts[1]], capture_output=True, check=False)

    def _reset_failed(self) -> None:
        """Clear the systemd failure state left by the engine-less install."""
        subprocess.run(
            ["systemctl", "reset-failed", f"snap.{self.service}.service"],
            capture_output=True,
            check=False,
        )

    def _select_engine(self) -> None:
        """Activate an engine, since sideloading skipped the hook that would.

        ``--auto`` only where there is an actual choice. Most of these snaps ship
        a single CPU engine and deliberately avoid hardware scoring (their engine
        scripts bypass ``modelctl run`` for exactly that reason), so they carry
        neither pciutils nor a ``hardware-observe`` plug on the CLI app.
        Demanding auto-selection from them fails on lspci to answer a question
        with one possible answer.

        Where there are several engines the machine still decides - never a name
        from the config.

        ``--no-restart`` because the caller starts the service afterwards; left
        to itself, ``use-engine`` restarts the snap as a side effect and reports
        the whole selection as failed if that start does not take.
        """
        engines = self._engines()
        selector = [engines[0]] if len(engines) == 1 else ["--auto"]
        print(f"[{self.snap}] engines={engines or '(unknown)'} -> use-engine {selector[0]}")
        result = _capture([self.cli, "use-engine", *selector, "--assume-yes", "--no-restart"])
        if result.returncode == 0:
            return
        # --auto can fail where hardware-observe is disconnected. Fall back to
        # the first engine the snap lists rather than losing the whole target.
        if engines and selector != [engines[0]]:
            print(f"[{self.snap}] --auto failed; selecting first engine: {engines[0]}")
            _run([self.cli, "use-engine", engines[0], "--assume-yes", "--no-restart"])
            return
        raise SystemExit(f"[{self.snap}] no engine could be selected: {result.stderr.strip()}")

    def _engines(self) -> list[str]:
        """Engine names, from the snap if it will say, else the source tree."""
        out = _capture([self.cli, "list-engines", "--format=json"])
        if out.returncode == 0:
            try:
                return [e["name"] for e in json.loads(out.stdout).get("engines", [])]
            except (ValueError, KeyError, TypeError):
                pass
        if self.dir is not None:
            return sorted(engine_options(self.dir))
        return []

    def _describe(self) -> None:
        """Ask the snap which engine auto-selection actually landed on."""
        self.engine = self._modelctl_field(["show-engine", "--format=json"], ("name", "engine"))
        print(f"[{self.snap}] serving engine={self.engine}")

    def _read_baseline(self) -> None:
        """Record the shipped value of every key any config touches.

        Settings persist across a sweep, so a config that sets compute-type=int8
        would leak into the next config that does not mention it. Restoring from
        this baseline makes each row a complete assignment rather than a
        difference from whatever ran before it.
        """
        keys = sorted({k for v in self.variants for k in v.settings})
        for key in keys:
            got = _capture([self.cli, "get", key])
            if got.returncode != 0:
                raise SystemExit(
                    f"[{self.snap}] config key {key!r} is not offered by this snap "
                    f"(engine {self.engine}); drop it from configs: or fix the name"
                )
            self._baseline[key] = got.stdout.strip()
        if self._baseline:
            print(f"[{self.snap}] config baseline: {self._baseline}")

    # -- axes -------------------------------------------------------------

    def models(self) -> list[str]:
        """Model variants the active engine offers, in manifest order.

        ``list-models --format=json`` lists exactly the active engine's
        ``model.options`` - so a CPU engine and a GPU engine can legitimately
        offer different weights.
        """
        out = _capture([self.cli, "list-models", "--format=json"])
        if out.returncode != 0:
            return []
        try:
            found = [m["name"] for m in json.loads(out.stdout).get("models", [])]
        except (ValueError, KeyError, TypeError):
            return []
        if not self.only_models:
            return found
        unknown = set(self.only_models) - set(found)
        if unknown:
            raise SystemExit(
                f"{self.snap}: configured models not offered by {self.engine}: {sorted(unknown)}"
            )
        return [m for m in found if m in set(self.only_models)]

    def supports_streaming(self) -> bool:
        """Whether the snap exposes an emission-mode toggle.

        The config key *is* the capability declaration: snaps whose adapter has
        no progressive path (funasr, audio8, qwen-c: commit-on-finalize only)
        never set it, so a missing key means "batch is the only mode", not
        "unconfigured".
        """
        return _capture([self.cli, "get", "streaming"]).returncode == 0

    def use_model(self, model: str) -> None:
        """Switch weights and come back up cold.

        ``use-model`` restarts the snap, which unloads the previous weights - so
        each variant is measured from a genuine cold load, and only one model is
        ever resident.
        """
        _run([self.cli, "use-model", model, "--assume-yes"])
        self.model = model
        if not wait_for_socket(self.socket):
            raise SystemExit(f"[{self.snap}] socket did not return after switching to {model}")

    def apply(self, *, mode: str, variant: Variant | None, togglable: bool) -> None:
        """Put the snap in one matrix cell, in a single restart.

        The emission toggle and every config key are written in one ``set`` call
        so a cell costs one restart, not one per key. Keys the variant omits are
        restored from the baseline, so a cell is an absolute configuration and
        not a difference from the cell before it.
        """
        settings = dict(self._baseline)
        if variant is not None:
            settings.update(variant.settings)
        args = [f"{k}={v}" for k, v in sorted(settings.items())]
        if togglable:
            args.insert(0, f"streaming={'true' if mode == STREAMING else 'false'}")
        if args:
            _run([self.cli, "set", "--assume-yes", "--no-restart", *args])
        self.streaming = mode == STREAMING
        self.config_suffix = variant.label if variant else ""
        subprocess.run(["snap", "restart", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            raise SystemExit(f"[{self.snap}] socket did not return after switching to {self.label}")

    def _modelctl_field(self, args: list[str], keys: tuple[str, ...]) -> str | None:
        out = _capture([self.cli, *args])
        if out.returncode != 0:
            return None
        try:
            data = json.loads(out.stdout)
        except json.JSONDecodeError:
            return None
        if isinstance(data, dict):
            for key in keys:
                if isinstance(data.get(key), str):
                    return data[key]
        return None

    @property
    def pid(self) -> int | None:
        """Best-effort daemon PID for resource sampling (systemd MainPID)."""
        out = _capture(
            ["systemctl", "show", f"snap.{self.service}.service", "--property=MainPID", "--value"],
            timeout=5,
        )
        value = out.stdout.strip()
        return int(value) if value.isdigit() and int(value) > 0 else None


def show_machine(cli: str) -> dict:
    """Hardware detection straight from modelctl, not hand-annotated YAML.

    Hardware is a property of the machine, so any installed inference snap can
    answer for all of them. Hand-written provenance is the kind that goes stale
    without anyone noticing.
    """
    out = _capture([cli, "show-machine", "--format=json"], timeout=60)
    if out.returncode != 0:
        return {}
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        return {}


# ---------------------------------------------------------------------------
# JSONL output
# ---------------------------------------------------------------------------


class _JsonlWriter:
    """Append-only JSONL writer that serialises dicts one per line."""

    def __init__(self, path: Path, machine: str = "unknown"):
        self._path = path
        self._machine = machine
        self._fp = path.open("a", encoding="utf-8")

    def write(self, record: dict) -> None:
        self._fp.write(json.dumps(record) + "\n")
        self._fp.flush()

    def status(self, label: str, status: str, reason: str = "") -> None:
        """Record how a row finished, so "no data" is not read as "clean".

        A clip that never ran (usability_fail cut the sweep short, or the target
        was broken outright) shows up as zero records for that label - which a
        plain WER/count table cannot distinguish from a clean pass. This is the
        one durable record of that outcome: console output from *this* run is
        gone by the time someone re-summarises the results later, which is
        exactly when it matters most. Written on success too, so a later clean
        rerun of the same row overwrites a stale failure from an earlier one.

        Carries the machine because results files get concatenated into a
        leaderboard, where a status keyed on the label alone would attach one
        host's failure to every other host's row.
        """
        self.write({"machine": self._machine, "label": label, "status": status, "reason": reason})

    def close(self) -> None:
        self._fp.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


# ---------------------------------------------------------------------------
# Sweep
# ---------------------------------------------------------------------------


def _sweep_one(
    *,
    target: SnapTarget,
    clips_cold: list,
    clips_warm: list,
    budget: float,
    out: _JsonlWriter,
    provenance: dict,
    corpus: dict[str, str],
    resources_path: Path,
    sample_resources: bool,
    broken: list[tuple[str, str]],
    unusable: list[tuple[str, str]],
    machine: str = "unknown",
) -> None:
    """Cold sample + warm sweep for one matrix cell.

    Failures are recorded against this cell only: a model that is too slow or
    broken must not cost the sweep the *other* cells of the same snap.
    """
    from myna.benchmarker._bench import AllClipsFailed, run_clips

    label = target.label
    sampler = None
    if sample_resources and target.pid is not None:
        sampler = ResourceSampler(target.pid)
        sampler.start()

    try:
        if clips_cold:
            print(f"[{label}] cold sample ({clips_cold[0].id})")
            overran, _ = asyncio.run(
                run_clips(
                    socket=target.socket,
                    clips=clips_cold,
                    label=label,
                    cold=True,
                    streaming=target.streaming,
                    provenance=provenance,
                    corpus=corpus,
                    budget_seconds=None,
                    out_fp=out,
                )
            )
            if overran:
                unusable.append((label, "cold sample overran"))
                out.status(label, "usability_fail", "cold sample overran")
                return

        print(f"[{label}] warm sweep (budget {budget:.0f}s)")
        overran, _ = asyncio.run(
            run_clips(
                socket=target.socket,
                clips=clips_warm,
                label=label,
                cold=False,
                streaming=target.streaming,
                provenance={**provenance, "sweep_budget_seconds": budget},
                corpus=corpus,
                budget_seconds=budget,
                out_fp=out,
            )
        )
        if overran:
            # Slower than the budget is a product verdict, not a datapoint to
            # wait out. Whatever clips landed are kept and flagged, so a partial
            # WER cannot pass as a full sweep.
            reason = f"exceeded {budget:.0f}s budget"
            unusable.append((label, reason))
            out.status(label, "usability_fail", reason)
            print(f"[{label}] USABILITY FAIL: {reason}")
        else:
            out.status(label, "ok")

    except AllClipsFailed as exc:
        broken.append((label, str(exc)))
        out.status(label, "broken", str(exc))
        print(f"[{label}] FAILED: {exc} - skipping cell")
    except subprocess.CalledProcessError as exc:
        # Kept ahead of the catch-all: the return code is the whole diagnosis
        # for a snap command that refused, and "CalledProcessError(...)" buries it.
        broken.append((label, f"exited {exc.returncode}"))
        out.status(label, "broken", f"exited {exc.returncode}")
        print(f"[{label}] FAILED: exited {exc.returncode} - skipping cell")
    except Exception as exc:  # noqa: BLE001 - one cell must not kill the sweep
        broken.append((label, f"{type(exc).__name__}: {exc}"))
        out.status(label, "broken", f"{type(exc).__name__}: {exc}")
        print(f"[{label}] FAILED: {type(exc).__name__}: {exc} - skipping cell")
    finally:
        if sampler is not None:
            sampler.stop()
            rss = round(sampler.peak_rss_mb, 1)
            vram = round(sampler.peak_vram_mb, 1) if sampler.peak_vram_mb else None
            print(f"[{label}] peak RSS {rss} MB" + (f" / VRAM {vram} MB" if vram else " / VRAM --"))
            with resources_path.open("a", encoding="utf-8") as fp:
                fp.write(
                    json.dumps(
                        {
                            "machine": machine,
                            "label": label,
                            "snap": target.snap,
                            "peak_rss_mb": rss,
                            "peak_vram_mb": vram,
                        }
                    )
                    + "\n"
                )
            _chown_to_invoker(resources_path)


# ---------------------------------------------------------------------------
# Config loading
# ---------------------------------------------------------------------------


@dataclass
class SweepConfig:
    path: Path
    root: Path
    manifest: Path
    out: Path
    cold_clip: str | None
    warm_clip_ids: list[str]
    budget: float
    targets: list[dict]


def load_config(
    config_path: Path,
    *,
    only: list[str] | None,
    out_override: str | None,
    budget_override: float | None,
) -> SweepConfig:
    if not config_path.exists():
        raise SystemExit(
            f"config not found: {config_path}\n"
            "Create bench.yaml or pass --config path/to/bench.yaml (see bench.yaml.example)."
        )
    cfg = yaml.safe_load(config_path.read_text(encoding="utf-8")) or {}
    # Relative paths are resolved against the config, not the process's cwd:
    # the same file then works from any directory, which is what lets one
    # config serve both an in-tree run and a tester's copied artefacts.
    root = (config_path.parent / cfg.get("root", ".")).resolve()
    targets = cfg.get("targets") or []
    if only:
        targets = [t for t in targets if t.get("snap") in set(only)]
    if not targets:
        raise SystemExit("no targets selected")
    return SweepConfig(
        path=config_path,
        root=root,
        manifest=(root / cfg.get("manifest", "corpus/manifest.json")).resolve(),
        out=(
            Path(out_override) if out_override else (root / cfg.get("out", "results.jsonl"))
        ).resolve(),
        cold_clip=cfg.get("cold_clip"),
        warm_clip_ids=list(cfg.get("clips") or []),
        budget=budget_override or cfg.get("sweep_budget_seconds") or DEFAULT_SWEEP_BUDGET_S,
        targets=targets,
    )


# ---------------------------------------------------------------------------
# `plan`: the matrix, without installing anything
# ---------------------------------------------------------------------------


def cmd_plan(args) -> None:  # noqa: ANN001
    """Print the rows this config would produce. No root, no install.

    Everything here is read off the source tree, so it is a prediction, not a
    reading: which engine wins is decided by hardware detection on the machine
    at run time, and ``list-models`` is the authority on weights. For a target
    given as an artefact list there is nothing to read, and the plan says so
    rather than guessing.
    """
    cfg = load_config(
        Path(args.config), only=args.only, out_override=args.out, budget_override=args.budget
    )
    print(f"config={cfg.path}  manifest={cfg.manifest.name}  out={cfg.out}")
    print(f"warm-sweep budget: {cfg.budget:.0f}s per target")
    if cfg.cold_clip:
        print(f"cold clip: {cfg.cold_clip}")
    print()

    total = 0
    # Exactly one engine per target actually runs, so the wall-clock estimate
    # counts each target's largest engine rather than the sum over all of them.
    # A machine with an NVIDIA card would otherwise be quoted double the sweep
    # it is about to start, which is the number someone plans a day around.
    will_run = 0
    # Two kinds of finding, kept apart because only one is the config's fault.
    # A snap that is not packed yet is a state of the tree - `run` skips that
    # target and carries on - while a config naming a key no engine declares is
    # a mistake that will take a cell down mid-sweep. Only the latter fails.
    unavailable: list[str] = []
    problems: list[str] = []
    for spec in cfg.targets:
        snap = spec.get("snap", "(unnamed)")
        try:
            target = SnapTarget(spec, cfg.root, args.label_suffix)
        except TargetUnavailable as exc:
            # Collected, not fatal: a plan that stops at the first unpacked snap
            # hides every target after it, which is the half you needed to see.
            unavailable.append(f"{snap}: {exc}")
            print(f"  {snap:20} UNAVAILABLE - {exc}")
            continue
        variants = target.variants
        print(f"  {snap:20} cli={target.cli}  socket={target.socket}")
        print(f"  {'':20} files={[Path(f).name for f in target.files]}")
        if target.dir is None:
            print(f"  {'':20} axes unknown (artefact-only target; read at install time)")
            continue
        engines = engine_options(target.dir)
        streams = declares_streaming(target.dir)
        modes = list(MODES) if streams else [BATCH]
        widest = 0
        for engine, detail in engines.items():
            models = detail["models"] or ["(engine default)"]
            if target.only_models:
                models = [m for m in models if m in set(target.only_models)]
            rows = []
            for model in models:
                for mode in modes:
                    for variant in variants_for(variants, mode):
                        cell = mode if variant is None else f"{mode}-{variant.label}"
                        rows.append(f"{snap}/{engine}/{model}/{cell}")
            total += len(rows)
            widest = max(widest, len(rows))
            print(f"  {'':20} engine {engine}: {len(rows)} row(s)")
            for row in rows:
                print(f"  {'':22} {row}")
        will_run += widest
        if len(engines) > 1:
            print(f"  {'':20} (one engine is chosen on the machine; only its rows will run)")
        unknown = [k for v in variants for k in v.settings]
        declared = {k for d in engines.values() for k in d["configurations"]}
        stray = sorted(set(unknown) - declared)
        if stray:
            problems.append(f"{snap}: configs name key(s) no engine.yaml declares: {stray}")

    print(f"\n{total} row(s) across all engines; the engine chosen on the machine decides which.")
    print(f"at most {will_run} will run here (one engine per target).")
    print(
        f"upper bound if every one of those spends its full {cfg.budget:.0f}s budget: "
        f"{will_run * cfg.budget / 3600:.1f} h"
    )
    if unavailable:
        print("\nnot packed (these targets will be skipped):")
        for item in unavailable:
            print(f"  - {item}")
    if problems:
        print("\nconfig problems:")
        for problem in problems:
            print(f"  - {problem}")
        raise SystemExit(1)


# ---------------------------------------------------------------------------
# `run`
# ---------------------------------------------------------------------------


def cmd_run(args) -> None:  # noqa: ANN001
    cfg = load_config(
        Path(args.config), only=args.only, out_override=args.out, budget_override=args.budget
    )

    if not cfg.manifest.exists():
        raise SystemExit(
            f"manifest not found: {cfg.manifest}\nRun: myna-bench download-corpus --out corpus"
        )
    if os.geteuid() != 0:
        raise SystemExit(
            "this runner installs and purges snaps, so it needs root:\n"
            f"  sudo {sys.executable} {' '.join(sys.argv)}"
        )
    _resolve_user_home()

    if not args.skip_env_check:
        from myna.benchmarker.guard import HARD, check_sweep_environment

        violations = check_sweep_environment()
        for violation in violations:
            print(violation)
        if [v for v in violations if v.severity == HARD]:
            raise SystemExit(
                "environment check failed - fix the above, or pass --skip-env-check to "
                "record numbers you already know are not comparable"
            )

    from myna.testbed.corpus import load_manifest, verify_corpus

    all_clips = list(load_manifest(cfg.manifest))
    try:
        corpus = {
            "corpus_id": verify_corpus(cfg.manifest),
            "corpus_manifest": cfg.manifest.name,
        }
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc
    clip_by_id = {c.id: c for c in all_clips}

    if cfg.cold_clip and cfg.cold_clip not in clip_by_id:
        raise SystemExit(
            f"cold_clip {cfg.cold_clip!r} not in manifest; available: {sorted(clip_by_id)[:5]}..."
        )
    clips_cold = [clip_by_id[cfg.cold_clip]] if cfg.cold_clip else []
    if cfg.warm_clip_ids:
        missing = [cid for cid in cfg.warm_clip_ids if cid not in clip_by_id]
        if missing:
            raise SystemExit(f"clips not in manifest: {missing}")
        clips_warm = [clip_by_id[cid] for cid in cfg.warm_clip_ids]
    else:
        clips_warm = [c for c in all_clips if c.id != cfg.cold_clip]

    from myna.benchmarker._summarize import resources_path_for

    cfg.out.parent.mkdir(parents=True, exist_ok=True)
    resources_path = resources_path_for(cfg.out)
    if not args.keep_results:
        for path in (cfg.out, resources_path):
            if path.exists():
                print(f"resetting {path}")
                path.unlink()

    from myna.benchmarker.machine import collect as collect_machine

    machine = collect_machine()
    print(
        f"\nmachine: {machine['hostname']}  cpu: {machine['cpu']}  ram: {machine['ram_gb']} GB"
        + (f"  gpu: {machine['gpu']} {machine['gpu_vram_gb']} GB" if machine["gpu"] else "")
    )
    print(f"manifest: {cfg.manifest.name}  cold={len(clips_cold)} warm={len(clips_warm)} clips")
    print(f"warm-sweep budget: {cfg.budget:.0f}s per target")
    print(f"output: {cfg.out}\n")

    broken: list[tuple[str, str]] = []
    unusable: list[tuple[str, str]] = []
    detected: dict = {}

    with _JsonlWriter(cfg.out, machine["hostname"]) as out:
        out.write(machine)

        for spec in cfg.targets:
            snap = spec.get("snap", "(unnamed)")
            try:
                target = SnapTarget(spec, cfg.root, args.label_suffix)
            except TargetUnavailable as exc:
                broken.append((snap, str(exc)))
                out.status(snap, "broken", str(exc))
                print(f"\n=== {snap} ===\nUNAVAILABLE: {exc} - skipping target")
                continue
            print(f"\n=== {target.snap} ===")
            try:
                target.start()
                if not detected:
                    # The first installed snap answers for the machine; they
                    # all would. Detection is a property of the box, not the snap.
                    detected = show_machine(target.cli)
                provenance = {
                    "machine": machine["hostname"],
                    "cpu": machine["cpu"],
                    "ram_gb": machine["ram_gb"],
                    "gpu": machine["gpu"],
                    "gpu_vram_gb": machine["gpu_vram_gb"],
                    "provision": "snap",
                    "hardware": detected,
                }
                models = target.models()
                togglable = target.supports_streaming()
                modes = list(MODES) if togglable else [BATCH]
                cells = [
                    (mode, variant)
                    for mode in modes
                    for variant in variants_for(target.variants, mode)
                ]
                described = [
                    mode if variant is None else f"{mode}-{variant.label}"
                    for mode, variant in cells
                ]
                print(f"[{target.snap}] models={models or '(none reported)'} cells={described}")
                for model in models or [None]:
                    # One install, every weight the engine offers. `use-model`
                    # restarts the snap, so each variant loads cold and only one
                    # is resident at a time - the same property the purge gives
                    # us between snaps.
                    if model:
                        target.use_model(model)
                    for mode, variant in cells:
                        target.apply(mode=mode, variant=variant, togglable=togglable)
                        _sweep_one(
                            target=target,
                            clips_cold=clips_cold,
                            clips_warm=clips_warm,
                            budget=cfg.budget,
                            out=out,
                            provenance=provenance,
                            corpus=corpus,
                            resources_path=resources_path,
                            sample_resources=not args.no_resources,
                            broken=broken,
                            unusable=unusable,
                            machine=machine["hostname"],
                        )
            except SystemExit as exc:
                broken.append((target.label, str(exc)))
                out.status(target.label, "broken", str(exc))
                print(f"[{target.label}] FAILED: {exc} - skipping target")
            except Exception as exc:  # noqa: BLE001 - one target must not kill the sweep
                broken.append((target.label, f"{type(exc).__name__}: {exc}"))
                out.status(target.label, "broken", f"{type(exc).__name__}: {exc}")
                print(f"[{target.label}] FAILED: {type(exc).__name__}: {exc} - skipping target")
            finally:
                target.stop()

    _chown_to_invoker(cfg.out)
    _chown_to_invoker(resources_path)

    if cfg.out.exists() and cfg.out.stat().st_size:
        print("\n===================== MATRIX =====================")
        from myna.benchmarker._summarize import cmd_summarize

        class _SummarizeArgs:
            infile = str(cfg.out)
            by_category = True
            sort = "wer"
            corpus = None

        try:
            cmd_summarize(_SummarizeArgs())
        except SystemExit as exc:
            # Aggregation refusing (e.g. every target failed, so no clip rows)
            # must not bury the per-target reasons printed below, which are the
            # point of the run.
            print(f"could not summarise: {exc}")
    else:
        print("\nno results to aggregate - every target failed")

    if unusable:
        print(f"\n{len(unusable)} cell(s) failed the usability budget:")
        for label, why in unusable:
            print(f"  - {label}: {why}")
    if broken:
        # After the table, so it is the last thing on screen: a target missing
        # from the matrix is easy to overlook, and a silently absent row is
        # exactly how a "surprising" result gets published.
        print(f"\n{len(broken)} cell(s)/target(s) failed:")
        for label, why in broken:
            print(f"  - {label}: {why}")

    print(f"\nresults written to {cfg.out}")
