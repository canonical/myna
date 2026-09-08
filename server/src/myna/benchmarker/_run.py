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
- **engine** (``engines:``): cpu, nvidia-gpu. Omitted, the machine decides and
  the target is swept once. Named, each engine is selected, configured and swept
  in turn - the device is not a setting, it is which engine is active.
- **config point** (``configs:``): any other shipped knob worth a row -
  whisper's ``compute-type`` (the quantization axis), parakeet's
  ``stream-arm-seconds``, nemotron's ``att-context-size``. Each entry names the
  modes and the engines it applies to, so a batch-only knob is sweepable, a
  latency dial does not multiply the batch rows, and a precision that exists
  only on CUDA is not requested on CPU. Values must be explicit: ``auto`` defers
  the choice, so the row could not say what it measured.

Labels come out as ``<snap>/<engine>/<model>/<mode>[-<config>]``.

    sudo myna-bench run --config bench.yaml
    sudo myna-bench run --config bench.yaml --only myna-whisper
    myna-bench plan --config bench.yaml          # no root, installs nothing

**Snaps only, by design.** Benchmarking a ``myna-server`` spawned from a venv
measured something we do not ship: different confinement, different engine
selection, different resident set. The only configuration that means anything
is the one a user installs.

**The label is read back, never assumed.** A target that names no engines is
swept once on whatever ``use-engine --auto`` picks, which is what a machine
would do; a target that names them is swept once per engine, which is the only
way to compare two of them, since a machine only makes one auto-selection.
Either way the runner reads the result back with ``show-engine`` and stamps
``<snap>/<engine>/<model>`` onto every record, and a named engine that will not
activate fails rather than falling back - a CPU number under a GPU label is
wrong in the one way nobody checks. The other input is ``--label-suffix``,
stamped as ``<snap>+<suffix>``: two builds of the same snap (e.g. base vs
maxstack encoder) are indistinguishable from inside, and the summary dedups by
label, so without it a rebuild silently shadows the run it was meant to be
compared against.

**A label is a name; the settings are the measurement.** Every record carries
``provenance.settings``, the complete assignment its cell served under. Without
it a results file cannot answer what a row actually ran - which is how a sweep
came back with a ``batch-auto`` row that was int8 on one model and float32 on
the next.

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
        files:                      # paths or globs; exactly one .snap
          - ./snaps/myna-whisper_*.snap
          - ./snaps/myna-whisper+*.comp
        cli: myna-whisper.whisper   # modelctl command (default: read from the snap)
        service: myna-whisper.server
        socket: /var/snap/myna-whisper/common/run/ubustt.sock
        models: [tiny, base]        # optional allowlist
        engines: [cpu, nvidia-gpu]  # optional; omitted = one auto-selected pass
        configs:
          - label: int8
            modes: [batch]
            engines: [cpu]          # optional; omitted = every engine
            settings: {compute-type: int8}
"""

from __future__ import annotations

import asyncio
import glob
import json
import os
import pwd
import subprocess
import sys
import tempfile
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


def _resolve_files(patterns: list[str], root: Path, snap: str) -> list[str]:
    """The artefacts to sideload, from a target's ``files:``.

    Every entry is a glob, so a config can name
    ``whisper-snap/myna-whisper_*.snap`` and keep working across a version bump.
    That is what the old ``dir:`` form existed to do, back when it was also the
    only way ``plan`` could read a target's axes; the snap answers for those
    itself now (see ``snap_metadata``), so a glob is all that was left of it and
    one config dialect serves an in-tree sweep and a tester's copied artefacts
    alike.

    A pattern that matches nothing is the target not being built yet rather than
    a broken config, so it reads as unavailable: ``plan`` collects it and ``run``
    skips that target and carries on. Exactly one ``.snap`` must survive the
    expansion - zero installs nothing, and two are the two packed revisions a
    branch switch leaves behind, where picking either silently benchmarks a
    build nobody asked for.
    """
    resolved: list[str] = []
    for pattern in patterns:
        joined = pattern if os.path.isabs(pattern) else str(root / pattern)
        matches = sorted(glob.glob(joined))
        if not matches:
            raise TargetUnavailable(f"{snap}: nothing matches {pattern!r} - pack it first")
        resolved.extend(matches)
    files = list(dict.fromkeys(resolved))
    packed = [f for f in files if f.endswith(".snap")]
    if len(packed) != 1:
        raise TargetUnavailable(
            f"{snap}: files: must resolve to exactly one .snap, got "
            f"{[Path(f).name for f in packed]}"
        )
    return files


def _engine_manifests(engines_dir: Path) -> dict[str, dict]:
    """``{engine: {"models": [...], "configurations": {...}}}`` from engine.yaml."""
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
            "devices": dict(parsed.get("devices") or {}),
        }
    return out


def engine_blocked_here(devices: dict, machine: dict) -> str | None:
    """Why this engine cannot run on this machine, or None if it might.

    Deliberately one-sided. It answers for the single requirement that actually
    varies between the machines we benchmark on - a GPU engine on a box with no
    GPU - and nothing else. modelctl owns real device matching (``lscompute``
    scores every clause against detected hardware); a second implementation here
    would be one more thing to keep in step, and a wrong "cannot run" is worse
    than a wasted install because it silently drops a row. So anything this
    cannot be sure about runs.

    Only ``allof`` clauses are requirements; an ``anyof`` clause is by
    definition one of several ways to qualify, so it never blocks.

    Knowing this before installing is the difference between a target that is
    skipped and one that is *reported broken* after sideloading several GB to
    find out - which is what a GPU-only snap in the config used to cost a
    CPU-only machine, and why they were commented out instead.
    """
    required = list((devices or {}).get("allof") or [])
    if any(clause.get("type") == "gpu" for clause in required) and not machine.get("gpu"):
        return "needs a GPU; none detected here"
    return None


_SNAP_METADATA: dict[str, dict] = {}


def snap_metadata(snap_file: Path) -> dict:
    """``{"engines": ..., "cli": ..., "streaming": ...}`` read out of a packed snap.

    Every axis but the corpus is a property of the artefact, so this is where
    ``plan`` gets them: the .snap carries ``engines/*/engine.yaml``,
    ``meta/snap.yaml`` and ``meta/hooks/install``, which between them name the
    engines, the models each offers, the knobs each declares, the CLI app and
    whether there is an emission-mode toggle at all. Reading them is what lets
    one config dialect serve both an in-tree sweep and a tester's copied
    artefacts: before it there was a second, source-tree-only form of a target,
    and a config naming a knob the snap does not have first showed up as a dead
    cell hours into the sweep.

    ``{}`` when the file cannot be read (no squashfs-tools, or a placeholder
    used by a test). Every caller treats that as "ask the snap once it is
    installed".
    """
    key = str(snap_file)
    if key in _SNAP_METADATA:
        return _SNAP_METADATA[key]
    meta: dict = {}
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "snap"
        extracted = subprocess.run(
            ["unsquashfs", "-q", "-n", "-f", "-d", str(root), str(snap_file), "engines", "meta"],
            capture_output=True,
            text=True,
            check=False,
        )
        if extracted.returncode == 0 and root.is_dir():
            hook = root / "meta" / "hooks" / "install"
            meta = {
                "engines": _engine_manifests(root / "engines"),
                "cli": _snap_yaml_command(root / "meta" / "snap.yaml"),
                "streaming": (
                    "streaming=" in hook.read_text(encoding="utf-8") if hook.exists() else None
                ),
            }
    _SNAP_METADATA[key] = meta
    return meta


def _snap_yaml_command(snap_yaml: Path) -> str | None:
    """The command that invokes this snap's modelctl CLI, from ``meta/snap.yaml``.

    Snapd exposes an app as a bare ``<snap>`` only when the app name matches the
    snap name, and as ``<snap>.<app>`` otherwise. ``myna-funasr`` names its CLI
    app ``funasr``, so its command is ``myna-funasr.funasr`` - assuming the snap
    name works for whisper and qwen and fails for funasr, which is exactly what
    it did. The CLI app is the non-daemon one.
    """
    if not snap_yaml.exists():
        return None
    parsed = yaml.safe_load(snap_yaml.read_text(encoding="utf-8")) or {}
    snap = parsed.get("name")
    for name, app in (parsed.get("apps") or {}).items():
        if isinstance(app, dict) and "daemon" not in app:
            return str(name) if name == snap else f"{snap}.{name}"
    return None


# ---------------------------------------------------------------------------
# Config points
# ---------------------------------------------------------------------------


# Values that mean "decide for me" rather than naming a setting. A config point
# is a row in a comparison table, so it has to be an assignment: whisper's
# ``compute-type: auto`` resolves to each model's own MODEL_COMPUTE_TYPE, which
# is int8 on tiny and float32 on base - so one "auto" row is two different
# arithmetics under one name, and against an explicit int8 row it is a duplicate
# on tiny and an unlabelled float32 on base. Both were measured that way before
# this check existed. The shipped default is still auto; what is refused is
# putting a defer in a results table.
DEFERRED_VALUES = frozenset({"auto", "default"})


@dataclass(frozen=True)
class Variant:
    """One config point: a named settings assignment, and where it applies.

    ``modes`` exists because knobs are not all latency dials. ``compute-type``
    changes batch decoding and streaming alike; ``stream-arm-seconds`` means
    nothing in batch mode and sweeping it there would triple the rows for three
    identical numbers.

    ``engines`` exists because a knob's *values* are engine-specific even when
    its name is not: CTranslate2 takes int8_float32 and float32 on CPU and
    float16, int8_float16 and float32 on CUDA, and rejects the CUDA types on a
    CPU device. Empty means every engine, which is right for a knob like
    ``stream-arm-seconds``.
    """

    label: str
    modes: tuple[str, ...]
    settings: dict[str, str] = field(default_factory=dict)
    engines: tuple[str, ...] = ()


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
        deferred = sorted(k for k, v in settings.items() if v.strip().lower() in DEFERRED_VALUES)
        if deferred:
            raise SystemExit(
                f"{snap}/{label}: {deferred} is set to a value that defers the choice, so the "
                "row cannot say what ran - it resolves per model and per engine. Name the "
                "setting explicitly (whisper cpu: int8_float32, float32; whisper nvidia-gpu: "
                "float16, int8_float16, float32), one config point each."
            )
        variants.append(
            Variant(
                label=label,
                modes=modes,
                settings=settings,
                engines=tuple(entry.get("engines") or ()),
            )
        )
    return variants


def variants_for(
    variants: list[Variant], mode: str, engine: str | None = None
) -> list[Variant | None]:
    """Config points applying to ``mode`` on ``engine``; ``[None]`` if none.

    A mode no config claims still gets exactly one row, at whatever the snap
    shipped - never zero rows, which would silently drop half the matrix. The
    same holds for an engine whose points are all scoped to another one: a CPU
    machine running a config written for both still measures the CPU engine at
    its shipped precision rather than dropping the target.

    ``engine`` of None means "not known yet" and does not filter, which is what
    a plan for a target whose engine is decided on the machine has to do.
    """
    applicable = [
        v
        for v in variants
        if mode in v.modes and (not v.engines or engine is None or engine in v.engines)
    ]
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
        if not spec.get("files"):
            raise SystemExit(
                f"{self.snap}: target needs files: - paths or globs naming the packed "
                "snap and the components to install with it"
            )
        self.files: list[str] = _resolve_files(list(spec["files"]), root, self.snap)
        self.snap_file: Path = next(Path(f) for f in self.files if f.endswith(".snap"))
        # The snap name is the command only when an app shares it, which is true
        # for none of these snaps, so it is read out of the artefact's
        # meta/snap.yaml. The fallback is for a snap that cannot be unsquashed.
        self.cli: str = spec.get("cli") or self._metadata().get("cli") or self.snap
        self.service: str = spec.get("service") or f"{self.snap}.server"
        self.socket: Path = Path(
            spec.get("socket") or f"/var/snap/{self.snap}/common/run/ubustt.sock"
        )
        # Optional allowlist: which model variants to sweep. Omitted = every
        # option the active engine declares.
        self.only_models: list[str] = list(spec.get("models") or [])
        # Optional engine axis: which engines to measure, each in turn. Omitted
        # = one pass on whatever hardware detection picks (see engines_to_sweep).
        self.only_engines: list[str] = list(spec.get("engines") or [])
        self.variants: list[Variant] = parse_variants(spec, self.snap)
        # Filled in after install, from the snap itself. The config never says.
        self.engine: str | None = None
        self.model: str | None = None
        self.streaming: bool = False
        self.config_suffix: str = ""
        # Shipped value of every key any config touches, read once per engine
        # so a config that omits a key restores it rather than inheriting the
        # previous config's value.
        self._baseline: dict[str, str] = {}
        # The complete key=value assignment the current cell is serving under,
        # stamped onto every record it produces. Without it a label is the only
        # record of what ran, and a label is a name someone chose.
        self.applied: dict[str, str] = {}
        # Engines this machine cannot run, filled in by check_machine.
        self.blocked: dict[str, str] = {}

    def check_machine(self, machine: dict) -> None:
        """Rule out engines this machine cannot run, before anything installs.

        A target left with none is unavailable in exactly the sense an unpacked
        one is: a state of the machine, not a mistake in the config. That is
        what lets a GPU-only target sit in the config permanently instead of
        being commented out - on a GPU box it runs, on this one it is skipped,
        and neither needs an edit.
        """
        engines = self.static_engines()
        self.blocked = {
            name: reason
            for name, detail in engines.items()
            if (reason := engine_blocked_here(detail.get("devices") or {}, machine))
        }
        wanted = self.only_engines or list(engines)
        unrunnable = [e for e in wanted if e in self.blocked]
        if engines and wanted and len(unrunnable) == len(wanted):
            raise TargetUnavailable(
                f"{self.snap}: no engine it ships can run here "
                f"({'; '.join(f'{e}: {self.blocked[e]}' for e in unrunnable)})"
            )

    def _metadata(self) -> dict:
        """Engines, CLI app and streaming toggle, read out of the packed snap."""
        return snap_metadata(self.snap_file)

    def static_engines(self) -> dict[str, dict]:
        """Engines this snap ships, read without installing it."""
        return self._metadata().get("engines") or {}

    def static_streaming(self) -> bool | None:
        """Whether the snap declares an emission-mode toggle, read offline."""
        return self._metadata().get("streaming")

    # -- identity ---------------------------------------------------------

    def _label_for(self, mode: str, config_suffix: str) -> str:
        snap = f"{self.snap}+{self.label_suffix}" if self.label_suffix else self.snap
        parts = [snap, self.engine or "unknown-engine"]
        if self.model:
            parts.append(self.model)
        parts.append(f"{mode}-{config_suffix}" if config_suffix else mode)
        return "/".join(parts)

    @property
    def label(self) -> str:
        """``<snap>[+suffix]/<engine>/<model>/<mode>[-<config>]``.

        The engine is the one the sweep selected or the one auto-selection
        landed on; either way it is read back from the snap with ``show-engine``
        rather than taken on trust. The model is whichever variant the sweep is
        currently on.
        """
        return self._label_for(STREAMING if self.streaming else BATCH, self.config_suffix)

    def cell_label(self, mode: str, variant: Variant | None) -> str:
        """The label a cell will carry, before it has been applied.

        Applying a cell can fail - a value the engine refuses takes the daemon
        down - and the failure has to be recorded against the row that caused
        it, which means naming the row before trying it.
        """
        return self._label_for(mode, variant.label if variant else "")

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
        """Install the snap and leave it stopped, with no engine selected yet.

        Selection is a separate step because it is an axis: ``select_engine`` is
        called once per engine the target sweeps.
        """
        self.purge()
        print(
            f"[{self.snap}] installing {len(self.files)} file(s): "
            f"{[Path(f).name for f in self.files]}"
        )
        _run(["snap", "install", "--dangerous", *self.files])
        self._connect_plugs()

    def select_engine(self, name: str | None = None) -> None:
        """Activate an engine and bring the socket up on it.

        ``None`` leaves the choice to hardware detection, which is what a target
        that does not name engines wants. A name is an override, and the only
        way to measure the engine the machine would not have picked - a CPU
        number from a box with a GPU in it, or the reverse.
        """
        # Before the first selection the daemon is crash-looping toward its
        # systemd start limit (install left no active engine); after one it is
        # serving. Stop covers both, then clear the failure state, both *before*
        # `use-engine`: it restarts the snap itself and reports the whole
        # selection as failed if systemd refuses. Stop first, or the loop can
        # re-fail between the reset and the start.
        # Engines can offer different weights, so nothing about the previous
        # engine's model survives the switch.
        self.model = None
        subprocess.run(["snap", "stop", self.service], capture_output=True, check=False)
        self._reset_failed()
        self._select_engine(name)
        subprocess.run(["snap", "start", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            raise SystemExit(
                f"[{self.snap}] socket {self.socket} did not appear - "
                f"check: journalctl -u snap.{self.service}"
            )
        self._describe()
        self._read_baseline()

    def engines_to_sweep(self) -> list[str | None]:
        """The engines this target measures, in order; ``[None]`` = auto-select.

        Naming engines is the only way to compare them: one machine runs one
        auto-selection, so without this a GPU box can never produce the CPU row
        it is being compared against, and a config written for both machines
        silently measures whichever half the hardware chose.
        """
        available = self.static_engines() or {}
        if not self.only_engines:
            # Auto-selection, unless this machine has ruled some engines out and
            # left exactly one standing - then name it, because `--auto` would
            # be scoring engines it cannot pick against one it can.
            runnable = [e for e in available if e not in self.blocked]
            return [runnable[0]] if len(available) > 1 and len(runnable) == 1 else [None]
        unknown = [e for e in self.only_engines if available and e not in available]
        if unknown:
            raise SystemExit(
                f"{self.snap}: engines: names {unknown}, which this snap does not ship "
                f"(it has {sorted(available)})"
            )
        return [e for e in self.only_engines if e not in self.blocked]

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

    def _select_engine(self, name: str | None = None) -> None:
        """Activate an engine, since sideloading skipped the hook that would.

        ``--auto`` only where there is an actual choice *and* the config named
        none. Most of these snaps ship a single CPU engine and deliberately
        avoid hardware scoring (their engine scripts bypass ``modelctl run`` for
        exactly that reason), so they carry neither pciutils nor a
        ``hardware-observe`` plug on the CLI app. Demanding auto-selection from
        them fails on lspci to answer a question with one possible answer.

        A named engine is taken at its word and never falls back: silently
        measuring the CPU engine under a config that asked for the GPU one
        produces a row that is wrong in the one way nobody checks.

        ``--no-restart`` because the caller starts the service afterwards; left
        to itself, ``use-engine`` restarts the snap as a side effect and reports
        the whole selection as failed if that start does not take.
        """
        engines = self._engines()
        if name is not None:
            print(f"[{self.snap}] use-engine {name} (named by config)")
            result = _capture([self.cli, "use-engine", name, "--assume-yes", "--no-restart"])
            if result.returncode != 0:
                raise SystemExit(
                    f"[{self.snap}] engine {name!r} could not be selected on this machine: "
                    f"{result.stderr.strip()}"
                )
            return
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
        return sorted(self.static_engines())

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
        self._baseline = {}
        keys = sorted({k for v in self._variants_here() for k in v.settings})
        for key in keys:
            got = _capture([self.cli, "get", key])
            if got.returncode != 0:
                raise SystemExit(
                    f"[{self.snap}] config key {key!r} is not offered by this snap "
                    f"(engine {self.engine}); scope it with engines: [<engine>], "
                    "drop it from configs:, or fix the name"
                )
            self._baseline[key] = got.stdout.strip()
        if self._baseline:
            print(f"[{self.snap}] config baseline: {self._baseline}")

    def _variants_here(self) -> list[Variant]:
        """Config points that apply to the engine currently active.

        Not every engine takes every knob - ``compute-type`` exists on whisper
        and on nothing else, and its legal values differ between whisper's own
        two engines - so a config point scoped to another engine must not be
        read, set, or counted as a row here.
        """
        return [v for v in self.variants if not v.engines or self.engine in v.engines]

    def cells(self, modes: list[str]) -> list[tuple[str, Variant | None]]:
        """Every (mode, config point) pair to measure on the active engine."""
        return [
            (mode, variant)
            for mode in modes
            for variant in variants_for(self._variants_here(), mode, self.engine)
        ]

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
            settings["streaming"] = "true" if mode == STREAMING else "false"
            args.insert(0, f"streaming={settings['streaming']}")
        if args:
            _run([self.cli, "set", "--assume-yes", "--no-restart", *args])
        self.streaming = mode == STREAMING
        self.config_suffix = variant.label if variant else ""
        self.applied = settings
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
    mode: str,
    variant: Variant | None,
    togglable: bool,
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
    """Configure one matrix cell, then cold-sample and warm-sweep it.

    Applying the cell belongs inside this boundary because it is the step most
    likely to fail: a value the engine refuses takes the daemon down with it,
    and that has to cost the one row that asked for it rather than every
    remaining row of the target.

    Failures are recorded against this cell only: a model that is too slow or
    broken must not cost the sweep the *other* cells of the same snap.
    """
    from myna.benchmarker._bench import AllClipsFailed, run_clips

    label = target.cell_label(mode, variant)
    sampler = None

    try:
        target.apply(mode=mode, variant=variant, togglable=togglable)
        # The complete assignment this cell served under, so a row records what
        # ran and not just what it was called.
        provenance = {**provenance, "settings": dict(target.applied)}
        if sample_resources and target.pid is not None:
            sampler = ResourceSampler(target.pid)
            sampler.start()
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
    except SystemExit as exc:
        # A cell that could not be brought up - typically a setting the engine
        # refuses, which takes the daemon down. SystemExit is not an Exception,
        # so without this clause it would escape past the catch-all below and
        # cost the target every row it had left.
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
    from myna.benchmarker.machine import collect as collect_machine

    machine = collect_machine()
    for spec in cfg.targets:
        snap = spec.get("snap", "(unnamed)")
        try:
            target = SnapTarget(spec, cfg.root, args.label_suffix)
            target.check_machine(machine)
        except TargetUnavailable as exc:
            # Collected, not fatal: a plan that stops at the first unpacked snap
            # hides every target after it, which is the half you needed to see.
            unavailable.append(str(exc))  # TargetUnavailable already names the snap
            print(f"  {snap:20} SKIPPED - {exc}")
            continue
        variants = target.variants
        print(f"  {snap:20} cli={target.cli}  socket={target.socket}")
        print(f"  {'':20} files={[Path(f).name for f in target.files]}")
        engines = target.static_engines()
        if not engines:
            print(f"  {'':20} axes unknown (engines unreadable; read at install time)")
            continue
        streams = target.static_streaming()
        modes = list(MODES) if streams else [BATCH]
        # Which engines actually run: the ones named, or the single one the
        # machine will pick from those shipped.
        # What will actually run here: the engines named (minus any this machine
        # rules out), or every engine it could auto-select between.
        selected = {
            e for e in (target.only_engines or engines) if e in engines and e not in target.blocked
        }
        rows_by_engine: dict[str, list[str]] = {}
        for engine, detail in engines.items():
            models = detail["models"] or ["(engine default)"]
            if target.only_models:
                models = [m for m in models if m in set(target.only_models)]
            rows_by_engine[engine] = [
                f"{snap}/{engine}/{model}/"
                + (mode if variant is None else f"{mode}-{variant.label}")
                for model in models
                for mode in modes
                for variant in variants_for(variants, mode, engine)
            ]
            total += len(rows_by_engine[engine])
            mark = (
                f"  ({target.blocked[engine]})"
                if engine in target.blocked
                else ""
                if engine in selected
                else "  (not selected)"
            )
            print(f"  {'':20} engine {engine}: {len(rows_by_engine[engine])} row(s){mark}")
            for row in rows_by_engine[engine]:
                print(f"  {'':22} {row}")
        run_here = [rows_by_engine[e] for e in engines if e in selected and e not in target.blocked]
        # Named engines all run; an unnamed target runs exactly one, so quote
        # its largest rather than the sum - a box with a GPU in it would
        # otherwise be told to plan a day around double the sweep it will start.
        will_run += (
            sum(len(r) for r in run_here)
            if target.only_engines
            else max((len(r) for r in run_here), default=0)
        )
        if target.only_engines:
            print(f"  {'':20} engines: {target.only_engines} - each is measured in turn")
        elif len(engines) > 1:
            print(
                f"  {'':20} (one engine is chosen on the machine; only its rows will run - "
                "name them in engines: to measure both)"
            )
        unknown_engines = sorted(set(target.only_engines) - set(engines))
        if unknown_engines:
            problems.append(
                f"{snap}: engines: names {unknown_engines}; this snap ships {sorted(engines)}"
            )
        for variant in variants:
            scope = variant.engines or tuple(engines)
            stray_engines = sorted(set(scope) - set(engines))
            if stray_engines:
                problems.append(
                    f"{snap}/{variant.label}: engines: names {stray_engines}; "
                    f"this snap ships {sorted(engines)}"
                )
            # A key is only meaningful on the engines the point is scoped to, so
            # that is where it has to be declared - a compute-type point scoped
            # to nvidia-gpu is not a mistake just because the cpu engine has no
            # such knob, and one scoped to nothing is a mistake as soon as any
            # engine it would run on lacks the key.
            for engine in scope:
                if engine not in engines:
                    continue
                missing = sorted(set(variant.settings) - set(engines[engine]["configurations"]))
                if missing:
                    problems.append(
                        f"{snap}/{variant.label}: {missing} not declared by engine "
                        f"{engine!r}; scope the point with engines: [...] or fix the name"
                    )

    print(f"\n{total} row(s) across all engines.")
    print(f"at most {will_run} will run here.")
    print(
        f"upper bound if every one of those spends its full {cfg.budget:.0f}s budget: "
        f"{will_run * cfg.budget / 3600:.1f} h"
    )
    if unavailable:
        print("\nthese targets will be skipped:")
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
    skipped: list[str] = []
    detected: dict = {}

    with _JsonlWriter(cfg.out, machine["hostname"]) as out:
        out.write(machine)

        for spec in cfg.targets:
            snap = spec.get("snap", "(unnamed)")
            try:
                target = SnapTarget(spec, cfg.root, args.label_suffix)
                target.check_machine(machine)
            except TargetUnavailable as exc:
                # Not packed, or nothing it ships can run here. Neither is a
                # failure to report as one: the sweep skips it and says so.
                skipped.append(str(exc))
                out.status(snap, "skipped", str(exc))
                print(f"\n=== {snap} ===\nSKIPPED: {exc}")
                continue
            print(f"\n=== {target.snap} ===")
            try:
                target.start()
                for engine in target.engines_to_sweep():
                    # One install, every engine the config asks for. `use-engine`
                    # restarts the snap, so each engine comes up clean; the
                    # baseline and the applicable config points are re-read
                    # against it, since neither the knobs nor their legal values
                    # are shared between engines.
                    target.select_engine(engine)
                    if not detected:
                        # The first installed snap answers for the machine; they
                        # all would. Detection is a property of the box, not the
                        # snap.
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
                    cells = target.cells(list(MODES) if togglable else [BATCH])
                    described = [
                        mode if variant is None else f"{mode}-{variant.label}"
                        for mode, variant in cells
                    ]
                    print(
                        f"[{target.snap}] engine={target.engine} "
                        f"models={models or '(none reported)'} cells={described}"
                    )
                    for model in models or [None]:
                        # Every weight the engine offers. `use-model` restarts
                        # the snap, so each variant loads cold and only one is
                        # resident at a time - the same property the purge gives
                        # us between snaps.
                        if model:
                            target.use_model(model)
                        for mode, variant in cells:
                            _sweep_one(
                                target=target,
                                mode=mode,
                                variant=variant,
                                togglable=togglable,
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

    if skipped:
        print(f"\n{len(skipped)} target(s) skipped:")
        for reason in skipped:
            print(f"  - {reason}")
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
