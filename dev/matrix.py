"""Matrix runner: sweep the packaged inference snaps and aggregate (T11).

One YAML file lists the *snaps* to benchmark. For each one the runner purges any
existing install, sideloads the locally packed snap plus its components, selects
an engine by hardware detection, then sweeps **every model variant that engine
offers** - `use-model` per variant, each with a **cold** sample
(model-load-from-cold) and a **warm** sweep - and purges again.

Every combination is its own row, never an average - the matrix exists to show
the shape of the trade-off:

- **model variant** (`whisper tiny/base/small`): an order of magnitude apart in
  both accuracy and cost. `list-models` reports the options, so the config does
  not name them (a `models:` allowlist can narrow the sweep).
- **emission mode** (batch / streaming): a shipped configuration toggle
  (`snap set <snap> streaming=`), so both settings are real user-facing
  configurations. Snaps whose adapter is commit-on-finalize only (funasr,
  qwen-c) expose no such key and are swept batch-only.

Labels come out as ``<snap>/<engine>/<model>/<mode>``.

    sudo server/.venv/bin/python dev/matrix.py --config dev/matrix.yaml
    sudo server/.venv/bin/python dev/matrix.py --config dev/matrix.yaml --only whisper
    uv run python dev/matrix.py --config dev/matrix.yaml --dry-run

Call ``server/.venv``'s interpreter directly rather than going through
``uv run``: sudo resets the environment (sudo-rs does not implement ``-E`` at
all), so ``uv`` would resolve a different project root under root's HOME.
``server/.venv`` is the project venv - it is the one ``uv sync`` manages, and
the only one carrying psutil/yaml/myna. Results and logs are chowned back to
``SUDO_UID`` afterwards, so the next unprivileged run can still append to them.

**Snaps only, by design.** Benchmarking a `myna-server` spawned from the venv
measured something we do not ship: different confinement, different engine
selection, different resident set. The only configuration that means anything
is the one a user installs.

**The label is an output, not an input.** `modelctl use-engine --auto` chooses
the engine by hardware detection, not this file; the runner reads it back with
`show-engine` and stamps `<snap>/<engine>/<model>` onto every record. A config
that named the engine could only ever disagree with reality.

**Purge between targets.** `snap remove --purge` drops $SNAP_COMMON, so each
target re-runs auto-selection from clean rather than inheriting whatever engine
was last active. It also guarantees one resident model at a time: backends
idle-unload on a timer (`sleep-idle-seconds`, 300 by default), so without a
purge a finished backend keeps its weights in RAM while the next one loads.

**Usability budget.** A backend slower than the budget is a product failure,
not a datapoint to wait for. The warm sweep runs under a wall-clock deadline;
overrunning it stops the sweep and stamps the target `usability_fail` with the
clips it managed. Measured per run, never predicted - a backend gets to prove
itself on the actual hardware.

The runner never scores anything itself - it shells out to ``dev/bench.py`` per
target (one source of truth for WER/latency) and ``dev/aggregate.py`` at the end.
"""

from __future__ import annotations

import argparse
import json
import os
import pwd
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import psutil
import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
LOG_DIR = REPO_ROOT / "results" / "matrix-logs"

# Wall-clock budget for a warm sweep, in seconds. Overrun = usability failure.
DEFAULT_SWEEP_BUDGET_S = 600.0

# Snaps this runner is allowed to remove. Purging is destructive (it drops
# $SNAP_COMMON and $SNAP_DATA), so it is restricted to the backends this repo
# builds. Anything else on the machine is off limits, whatever the config says.
PURGEABLE = frozenset(
    {
        "whisper",
        "parakeet",
        "sherpa",
        "qwen",
        "nemotron",
        "myna-funasr",
        "myna-fake-backend",
    }
)


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

    def _tree(self) -> list[psutil.Process]:
        try:
            root = psutil.Process(self.pid)
            return [root, *root.children(recursive=True)]
        except psutil.Error:
            return []

    def run(self) -> None:
        while not self._stop_event.is_set():
            procs = self._tree()
            rss = 0
            for pr in procs:
                try:
                    rss += pr.memory_info().rss
                except psutil.Error:
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


class SweepOverran(Exception):
    """The warm sweep exceeded its wall-clock budget."""


def _run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, **kw)


def _declared_components(snap_dir: Path) -> set[str]:
    """Component names from the project's snapcraft.yaml.

    The .comp files on disk are not authoritative: a directory accumulates
    artifacts from branches and renames (qwen+qwen-vllm.comp outlived the vLLM
    branch by two months). Installing an undeclared component fails, so trust
    the manifest and ignore the debris.
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
        raise SystemExit(f"no {snap}_*.snap in {snap_dir} - pack it first")
    if len(packed) > 1:
        raise SystemExit(f"several {snap}_*.snap in {snap_dir}: {[p.name for p in packed]}")
    declared = _declared_components(snap_dir)
    comps = [
        p for p in sorted(snap_dir.glob(f"{snap}+*.comp")) if p.stem.split("+", 1)[1] in declared
    ]
    missing = declared - {p.stem.split("+", 1)[1] for p in comps}
    if missing:
        raise SystemExit(f"{snap}: declared components not packed: {sorted(missing)} - repack")
    return [str(p) for p in [*packed, *comps]]


def _modelctl_command(snap_dir: Path, snap: str) -> str:
    """The command that invokes this snap's modelctl CLI.

    Snapd exposes an app as a bare `<snap>` only when the app name matches the
    snap name, and as `<snap>.<app>` otherwise. `myna-funasr` names its CLI app
    `funasr`, so its command is `myna-funasr.funasr` - assuming the snap name
    works for whisper and qwen and fails for funasr, which is exactly what it
    did. The CLI app is the non-daemon one.
    """
    recipe = snap_dir / "snap" / "snapcraft.yaml"
    if recipe.exists():
        parsed = yaml.safe_load(recipe.read_text(encoding="utf-8")) or {}
        for name, app in (parsed.get("apps") or {}).items():
            if isinstance(app, dict) and "daemon" not in app:
                return name if name == snap else f"{snap}.{name}"
    return snap


class SnapTarget:
    """A locally packed snap: purge, sideload, measure, purge."""

    def __init__(self, spec: dict):
        self.snap = spec["snap"]
        if self.snap not in PURGEABLE:
            raise SystemExit(
                f"{self.snap!r} is not in the purge allowlist {sorted(PURGEABLE)} - "
                "this runner removes what it benchmarks, so it refuses unknown snaps"
            )
        self.dir = REPO_ROOT / spec["dir"]
        self.cli = spec.get("cli") or _modelctl_command(self.dir, self.snap)
        self.service = spec.get("service", f"{self.snap}.server")
        self.socket = Path(spec.get("socket") or f"/var/snap/{self.snap}/common/run/ubustt.sock")
        # Optional allowlist: which model variants to sweep. Omitted = every
        # option the active engine declares.
        self.only_models: list[str] = list(spec.get("models") or [])
        # Filled in after install, from the snap itself. The config never says.
        self.engine: str | None = None
        self.model: str | None = None
        self.streaming = False

    @property
    def label(self) -> str:
        """``<snap>/<engine>/<model>``.

        The engine is whatever auto-selection landed on; the model is whichever
        variant the sweep is currently on. Both are read back or set by the
        runner, never taken from config.
        """
        parts = [self.snap, self.engine or "unknown-engine"]
        if self.model:
            parts.append(self.model)
        parts.append("streaming" if self.streaming else "batch")
        return "/".join(parts)

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
        files = _snap_files(self.dir, self.snap)
        print(f"[{self.snap}] installing {len(files)} file(s): {[Path(f).name for f in files]}")
        _run(["snap", "install", "--dangerous", *files])
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

    def _connect_plugs(self) -> None:
        """Connect the interfaces a sideloaded snap does not get automatically.

        `snap install --dangerous` carries no snap declaration, so manual-connect
        plugs stay unconnected. `hardware-observe` is the one that matters: the
        install hook checks `snapctl is-connected hardware-observe` before
        running `use-engine --auto`, and without it the snap installs with *no
        active engine* and the daemon exits 1 on every start.
        """
        out = subprocess.run(
            ["snap", "connections", self.snap],
            capture_output=True,
            text=True,
            check=False,
        ).stdout
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

        `--auto` only where there is an actual choice. Most of these snaps ship
        a single CPU engine and deliberately avoid hardware scoring (their
        engine scripts bypass `modelctl run` for exactly that reason), so they
        carry neither `pciutils` nor a `hardware-observe` plug on the CLI app.
        Demanding auto-selection from them fails on lspci to answer a question
        with one possible answer.

        Where there are several engines the machine still decides - never a name
        from the config.

        ``--no-restart`` because the caller starts the service afterwards; left
        to itself, ``use-engine`` restarts the snap as a side effect and reports
        the whole selection as failed if that start does not take.
        """
        engines = sorted(p.name for p in (self.dir / "engines").iterdir() if p.is_dir())
        selector = [engines[0]] if len(engines) == 1 else ["--auto"]
        print(f"[{self.snap}] engines={engines} -> use-engine {selector[0]}")
        _run([self.cli, "use-engine", *selector, "--assume-yes", "--no-restart"])

    def _describe(self) -> None:
        """Ask the snap which engine auto-selection actually landed on."""
        self.engine = self._modelctl_field(["show-engine", "--format=json"], ("name", "engine"))
        print(f"[{self.snap}] serving engine={self.engine}")

    def models(self) -> list[str]:
        """Model variants the active engine offers, in manifest order.

        `list-models` prints one id per line (its --format flag is still a TODO
        upstream), and lists exactly the active engine's `model.options` - so a
        CPU engine and a GPU engine can legitimately offer different weights.
        """
        try:
            out = subprocess.run(
                [self.cli, "list-models"],
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            ).stdout
        except (OSError, subprocess.SubprocessError):
            return []
        found = [line.strip() for line in out.splitlines() if line.strip()]
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
        no progressive path (funasr, qwen-c: commit-on-finalize only) never set
        it, so a missing key means "batch is the only mode", not "unconfigured".
        """
        return (
            subprocess.run(
                [self.cli, "get", "streaming"],
                capture_output=True,
                timeout=30,
                check=False,
            ).returncode
            == 0
        )

    def set_streaming(self, streaming: bool) -> None:
        """Switch emission mode and come back up cold."""
        value = "true" if streaming else "false"
        _run([self.cli, "set", "--assume-yes", "--no-restart", f"streaming={value}"])
        self.streaming = streaming
        subprocess.run(["snap", "restart", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            mode = "streaming" if streaming else "batch"
            raise SystemExit(f"[{self.snap}] socket did not return after switching to {mode}")

    def use_model(self, model: str) -> None:
        """Switch weights and come back up cold.

        `use-model` restarts the snap, which unloads the previous weights - so
        each variant is measured from a genuine cold load, and only one model is
        ever resident.
        """
        _run([self.cli, "use-model", model, "--assume-yes"])
        self.model = model
        if not wait_for_socket(self.socket):
            raise SystemExit(f"[{self.snap}] socket did not return after switching to {model}")

    def _modelctl_field(self, args: list[str], keys: tuple[str, ...]) -> str | None:
        try:
            out = subprocess.run(
                [self.cli, *args],
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            ).stdout
            data = json.loads(out)
        except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
            return None
        if isinstance(data, dict):
            for key in keys:
                if isinstance(data.get(key), str):
                    return data[key]
        return None

    def stop(self) -> None:
        self.purge()

    def make_cold(self) -> None:
        """A freshly installed snap has never served a request; nothing to reset."""

    @property
    def pid(self) -> int | None:
        """Best-effort daemon PID for resource sampling (systemd MainPID)."""
        try:
            out = subprocess.run(
                [
                    "systemctl",
                    "show",
                    f"snap.{self.service}.service",
                    "--property=MainPID",
                    "--value",
                ],
                capture_output=True,
                text=True,
                timeout=5,
                check=True,
            ).stdout.strip()
            return int(out) if out.isdigit() and int(out) > 0 else None
        except (OSError, subprocess.SubprocessError, ValueError):
            return None


def _cpu_model() -> str | None:
    """CPU model name from /proc/cpuinfo.

    modelctl's show-machine reports architecture and manufacturer-id but not the
    marketing name, and "AuthenticAMD amd64" is not something you can compare
    two benchmark runs across.
    """
    try:
        for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            key, _, value = line.partition(":")
            if key.strip() == "model name":
                return value.strip()
    except OSError:
        pass
    return None


def show_machine(snap: str) -> dict:
    """Hardware provenance straight from modelctl, not hand-annotated YAML.

    Hardware is a property of the machine, so any installed inference snap can
    answer for all of them. Hand-written provenance is the kind that goes stale
    without anyone noticing.

    ``machine`` stays a short string because the aggregate table prints it as a
    column; the full detection dump rides along under ``hardware`` so a record
    can be re-examined later without rerunning anything.
    """
    detected: dict = {}
    try:
        out = subprocess.run(
            [snap, "show-machine", "--format=json"],
            capture_output=True,
            text=True,
            timeout=60,
            check=True,
        ).stdout
        detected = json.loads(out)
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
        detected = {}
    ram = (detected.get("memory") or {}).get("total-ram")
    return {
        "machine": socket.gethostname(),
        "cpu": _cpu_model(),
        "ram_gb": round(ram / 1e9, 1) if isinstance(ram, (int, float)) else None,
        "hardware": detected,
    }


def _demote() -> None:
    """Drop back to the invoking user for a child process.

    The runner needs root for snap install/remove, but results files must stay
    writable by the human afterwards - otherwise the next non-sudo run fails on
    append to a root-owned JSONL.
    """
    uid = os.environ.get("SUDO_UID")
    gid = os.environ.get("SUDO_GID")
    if not uid or not gid:
        return
    os.setgid(int(gid))
    os.setuid(int(uid))


def _chown_to_invoker(path: Path) -> None:
    uid = os.environ.get("SUDO_UID")
    gid = os.environ.get("SUDO_GID")
    if not uid or not gid or not path.exists():
        return
    os.chown(path, int(uid), int(gid))


def run_bench(
    *,
    socket: Path,
    label: str,
    manifest: Path,
    out: Path,
    provenance: dict,
    cold: bool,
    clips: list[str],
    streaming: bool = False,
    budget_s: float | None = None,
) -> None:
    cmd = [
        sys.executable,
        str(REPO_ROOT / "dev" / "bench.py"),
        "--socket",
        str(socket),
        "--label",
        label,
        "--manifest",
        str(manifest),
        "--out",
        str(out),
        "--batch",
        "--provenance",
        json.dumps(provenance),
    ]
    if cold:
        cmd.append("--cold")
    if streaming:
        # Must match how the snap was launched: bench.py scores the progressive
        # metrics (time_to_first_committed, commit_stability) only in this mode,
        # and the server only emits them when its own --streaming is set.
        cmd.append("--streaming")
    if budget_s:
        cmd += ["--budget-seconds", str(budget_s)]
    cmd += clips
    try:
        # bench.py polices the budget itself and exits 2, so the clips that did
        # land are written before it stops. The timeout here is only a backstop
        # for a backend that wedges inside a single clip.
        subprocess.run(  # noqa: S603
            cmd,
            check=True,
            timeout=(budget_s * 2 if budget_s else None),
            preexec_fn=_demote,
        )
    except subprocess.TimeoutExpired as exc:
        raise SweepOverran(f"wedged past {budget_s * 2:.0f}s (backstop kill)") from exc
    except subprocess.CalledProcessError as exc:
        if exc.returncode == 2:
            raise SweepOverran(f"exceeded {budget_s:.0f}s budget") from exc
        raise


def _sweep_one(
    *,
    target: SnapTarget,
    manifest: Path,
    out: Path,
    provenance: dict,
    cold_clip: str | None,
    warm_clips: list[str],
    budget: float,
    sample_resources: bool,
    resources_path: Path,
    broken: list[tuple[str, str]],
    unusable: list[tuple[str, str]],
) -> None:
    """Cold sample + warm sweep for one engine/model combination.

    Failures are recorded against this variant only: a model that is too slow
    or broken must not cost the sweep the *other* variants of the same snap.
    """
    label = target.label
    sampler = None
    if sample_resources and target.pid is not None:
        sampler = ResourceSampler(target.pid)
        sampler.start()
    try:
        if cold_clip:
            print(f"[{label}] cold sample ({cold_clip})")
            run_bench(
                socket=target.socket,
                label=label,
                manifest=manifest,
                out=out,
                provenance=provenance,
                cold=True,
                clips=[cold_clip],
                streaming=target.streaming,
            )
        print(f"[{label}] warm sweep (budget {budget:.0f}s)")
        run_bench(
            socket=target.socket,
            label=label,
            manifest=manifest,
            out=out,
            provenance={**provenance, "sweep_budget_seconds": budget},
            cold=False,
            clips=list(warm_clips),
            streaming=target.streaming,
            budget_s=budget,
        )
    except SweepOverran as exc:
        # Slower than the budget is a product verdict, not a datapoint to wait
        # out. Whatever clips landed before the deadline are kept and flagged,
        # so a partial WER cannot pass as a full sweep.
        unusable.append((label, str(exc)))
        print(f"[{label}] USABILITY FAIL: {exc}")
    except subprocess.CalledProcessError as exc:
        broken.append((label, f"exited {exc.returncode}"))
        print(f"[{label}] FAILED: exited {exc.returncode} - skipping variant")
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
                            "label": label,
                            "snap": target.snap,
                            "peak_rss_mb": rss,
                            "peak_vram_mb": vram,
                        }
                    )
                    + "\n"
                )
            _chown_to_invoker(resources_path)


def _resolve_user_home() -> None:
    """Point HOME at the invoking user so uv/venv caches are not root-owned."""
    uid = os.environ.get("SUDO_UID")
    if uid:
        try:
            os.environ["HOME"] = pwd.getpwuid(int(uid)).pw_dir
        except (KeyError, ValueError):
            pass


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--config", type=Path, default=REPO_ROOT / "dev" / "matrix.yaml")
    parser.add_argument("--only", action="append", help="run only this snap (repeatable)")
    parser.add_argument("--dry-run", action="store_true", help="print the plan; install nothing")
    parser.add_argument(
        "--keep-results",
        action="store_true",
        help="append to the results file instead of resetting it",
    )
    parser.add_argument(
        "--no-resources",
        action="store_true",
        help="skip peak RAM/VRAM sampling (for pristine latency timing)",
    )
    parser.add_argument(
        "--budget",
        type=float,
        default=None,
        help=f"warm-sweep wall-clock budget in seconds (default {DEFAULT_SWEEP_BUDGET_S:.0f})",
    )
    args = parser.parse_args()

    if not args.config.exists():
        raise SystemExit(f"no config at {args.config}")
    cfg = yaml.safe_load(args.config.read_text(encoding="utf-8"))

    manifest = (REPO_ROOT / cfg.get("manifest", "corpus/real/manifest.json")).resolve()
    out = (REPO_ROOT / cfg.get("out", "results/bench.jsonl")).resolve()
    cold_clip = cfg.get("cold_clip")
    warm_clips = cfg.get("clips", [])
    budget = args.budget or cfg.get("sweep_budget_seconds") or DEFAULT_SWEEP_BUDGET_S
    targets = cfg.get("targets", [])
    if args.only:
        targets = [t for t in targets if t["snap"] in set(args.only)]
    if not targets:
        raise SystemExit("no targets selected")

    print(f"config={args.config}  manifest={manifest.name}  out={out}")
    print(f"targets: {', '.join(t['snap'] for t in targets)}")
    print(f"warm-sweep budget: {budget:.0f}s per target")
    if cold_clip:
        print(f"cold clip: {cold_clip}")
    if args.dry_run:
        for spec in targets:
            t = SnapTarget(spec)
            print(f"  - {t.snap:20} files={[Path(f).name for f in _snap_files(t.dir, t.snap)]}")
            print(f"    {'':20} socket={t.socket}")
        return

    if os.geteuid() != 0:
        raise SystemExit(
            "this runner installs and purges snaps, so it needs root:\n"
            f"  sudo {sys.executable} {' '.join(sys.argv)}"
        )
    _resolve_user_home()

    if not args.keep_results and out.exists():
        print(f"resetting {out}")
        out.unlink()
    resources_path = out.parent / "matrix-resources.jsonl"
    if not args.keep_results and resources_path.exists():
        resources_path.unlink()

    hardware: dict = {}
    broken: list[tuple[str, str]] = []
    unusable: list[tuple[str, str]] = []
    for spec in targets:
        target = SnapTarget(spec)
        print(f"\n=== {target.snap} ===")
        try:
            target.start()
            if not hardware:
                # First installed snap answers for the machine; they all would.
                hardware = show_machine(target.snap)
            provenance = {**hardware, "provision": "snap"}
            variants = target.models()
            # Emission mode is a shipped configuration toggle, so both settings
            # are real product configurations and both get measured. Snaps whose
            # adapter has no progressive path expose no key, and are batch only.
            togglable = target.supports_streaming()
            modes = [False, True] if togglable else [False]
            print(
                f"[{target.snap}] variants={variants or '(none reported)'} "
                f"modes={['streaming' if m else 'batch' for m in modes]}"
            )
            for model in variants or [None]:
                # One install, every weight the engine offers. `use-model`
                # restarts the snap, so each variant loads cold and only one is
                # resident at a time - the same property the purge gives us
                # between snaps.
                if model:
                    target.use_model(model)
                for streaming in modes:
                    # Only touch the key on snaps that have one. Setting it on a
                    # batch-only snap fails ("key not found") and would take the
                    # whole target down over a no-op.
                    if togglable:
                        target.set_streaming(streaming)
                    _sweep_one(
                        target=target,
                        manifest=manifest,
                        out=out,
                        provenance=provenance,
                        cold_clip=cold_clip,
                        warm_clips=warm_clips,
                        budget=budget,
                        sample_resources=not args.no_resources,
                        resources_path=resources_path,
                        broken=broken,
                        unusable=unusable,
                    )
        except subprocess.CalledProcessError as exc:
            broken.append((target.label, f"exited {exc.returncode}"))
            print(f"[{target.label}] FAILED: exited {exc.returncode} - skipping target")
        except Exception as exc:  # noqa: BLE001 - one target must not kill the sweep
            broken.append((target.label, f"{type(exc).__name__}: {exc}"))
            print(f"[{target.label}] FAILED: {type(exc).__name__}: {exc} - skipping target")
        finally:
            target.stop()

    _chown_to_invoker(out)
    if LOG_DIR.exists():
        for log in LOG_DIR.glob("*.log"):
            _chown_to_invoker(log)

    if out.exists() and out.stat().st_size:
        print("\n===================== MATRIX =====================")
        subprocess.run(
            [
                sys.executable,
                str(REPO_ROOT / "dev" / "aggregate.py"),
                "--in",
                str(out),
                "--by-category",
            ],
            check=True,
        )
    else:
        # Every target failed. Aggregating nothing raises, and a traceback here
        # would bury the per-target reasons printed below, which are the point.
        print("\nno results to aggregate - every target failed")
    if unusable:
        print(f"\n{len(unusable)} target(s) failed the usability budget:")
        for label, why in unusable:
            print(f"  - {label}: {why}")
    if broken:
        # After the table, so it is the last thing on screen: a target missing
        # from the matrix is easy to overlook, and a silently absent row is
        # exactly how a "surprising" result gets published.
        print(f"\n{len(broken)} target(s) produced no data:")
        for label, why in broken:
            print(f"  - {label}: {why}")
    if broken or unusable:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
