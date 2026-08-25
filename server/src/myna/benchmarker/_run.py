"""Snap sweep runner for the standalone benchmarker.

Adapted from dev/matrix.py for standalone distribution. The main difference
is that targets list their snap/component files explicitly (no source-tree
``dir``), so testers only need the packed artefacts, not a checkout.

Config format (bench.yaml):

    manifest: ./corpus/manifest.json
    out: ./results.jsonl
    cold_clip: librispeech-84-121123-0000  # optional: one clip run --cold first
    sweep_budget_seconds: 600              # optional, default 600

    targets:
      - snap: myna-whisper
        files:
          - ./snaps/myna-whisper_1.0_amd64.snap
          - ./snaps/myna-whisper+cpu.comp
          - ./snaps/myna-whisper+nvidia-gpu.comp
        cli: myna-whisper.whisper  # modelctl command (default: the snap name)
        # service: myna-whisper.server  # optional (default: <snap>.server)
        # socket: /var/snap/...    # optional (default: standard path)
        # models: [tiny, base]     # optional model allowlist
        streaming_configs:         # optional: sweep streaming at multiple settings
          - label: arm3s
            settings:
              stream-arm-seconds: "3"

The output file starts with a ``{"type": "machine", ...}`` header record,
followed by one record per clip × snap × model × mode combination. The schema
is identical to dev/bench.py so dev/aggregate.py and the ``summarize``
subcommand work unchanged.
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
from pathlib import Path

import yaml

DEFAULT_SWEEP_BUDGET_S = 600.0


# ---------------------------------------------------------------------------
# Helpers shared with matrix.py
# ---------------------------------------------------------------------------


def _run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, **kw)


def wait_for_socket(path: Path, timeout: float = 120.0) -> bool:
    """Poll until the snap has bound the UDS, or timeout."""
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


class SweepOverran(Exception):
    """The warm sweep exceeded its wall-clock budget."""


def _chown_to_invoker(path: Path) -> None:
    uid = os.environ.get("SUDO_UID")
    gid = os.environ.get("SUDO_GID")
    if not uid or not gid or not path.exists():
        return
    os.chown(path, int(uid), int(gid))


def _resolve_user_home() -> None:
    """Point HOME at the invoking user so uv/venv caches are not root-owned."""
    uid = os.environ.get("SUDO_UID")
    if uid:
        try:
            os.environ["HOME"] = pwd.getpwuid(int(uid)).pw_dir
        except (KeyError, ValueError):
            pass


# ---------------------------------------------------------------------------
# SnapTarget
# ---------------------------------------------------------------------------


class SnapTarget:
    """A sideloaded snap: purge, install provided files, measure, purge.

    Unlike the matrix runner's SnapTarget, this one accepts the packed
    artefact paths directly rather than deriving them from a source tree.
    """

    def __init__(self, spec: dict):
        self.snap: str = spec["snap"]
        raw_files = spec.get("files") or []
        if not raw_files:
            raise SystemExit(f"{self.snap!r}: no files listed in config (need .snap + .comp)")
        # Resolve relative to cwd at parse time.
        self.files: list[str] = [str(Path(f).resolve()) for f in raw_files]
        self.cli: str = spec.get("cli") or self.snap
        self.service: str = spec.get("service") or f"{self.snap}.server"
        self.socket: Path = Path(
            spec.get("socket") or f"/var/snap/{self.snap}/common/run/ubustt.sock"
        )
        self.only_models: list[str] = list(spec.get("models") or [])
        self.engine: str | None = None
        self.model: str | None = None
        self.streaming: bool = False
        self.config_suffix: str = ""

    @property
    def label(self) -> str:
        parts = [self.snap, self.engine or "unknown-engine"]
        if self.model:
            parts.append(self.model)
        mode = "streaming" if self.streaming else "batch"
        if self.streaming and self.config_suffix:
            mode = f"streaming-{self.config_suffix}"
        parts.append(mode)
        return "/".join(parts)

    def purge(self) -> None:
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
        subprocess.run(["snap", "stop", self.service], capture_output=True, check=False)
        self._reset_failed()
        self._select_engine()
        subprocess.run(["snap", "start", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            raise SystemExit(
                f"[{self.snap}] socket {self.socket} did not appear — "
                f"check: journalctl -u snap.{self.service}"
            )
        self._describe()

    def _connect_plugs(self) -> None:
        out = subprocess.run(
            ["snap", "connections", self.snap],
            capture_output=True,
            text=True,
            check=False,
        ).stdout
        for line in out.splitlines()[1:]:
            parts = line.split()
            if len(parts) >= 3 and parts[2] == "-" and parts[1] != "-":
                print(f"[{self.snap}] connecting {parts[1]}")
                subprocess.run(["snap", "connect", parts[1]], capture_output=True, check=False)

    def _reset_failed(self) -> None:
        subprocess.run(
            ["systemctl", "reset-failed", f"snap.{self.service}.service"],
            capture_output=True,
            check=False,
        )

    def _select_engine(self) -> None:
        """Activate an engine via auto-detection; fall back to the first available."""
        result = subprocess.run(
            [self.cli, "use-engine", "--auto", "--assume-yes", "--no-restart"],
            capture_output=True,
            check=False,
        )
        if result.returncode == 0:
            return
        # --auto failed (hardware-observe disconnected, e.g. sideloaded snap);
        # fall back to the first listed engine.
        try:
            out = subprocess.run(
                [self.cli, "list-engines", "--format=json"],
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            ).stdout
            engines = [e["name"] for e in json.loads(out).get("engines", [])]
        except (OSError, subprocess.SubprocessError, ValueError, KeyError):
            engines = []
        if engines:
            print(f"[{self.snap}] --auto failed; selecting first engine: {engines[0]}")
            _run([self.cli, "use-engine", engines[0], "--assume-yes", "--no-restart"])
        else:
            # Last resort: let the snap decide on start.
            print(f"[{self.snap}] engine selection skipped (auto failed, no list-engines output)")

    def _describe(self) -> None:
        self.engine = self._modelctl_field(["show-engine", "--format=json"], ("name", "engine"))
        print(f"[{self.snap}] serving engine={self.engine}")

    def models(self) -> list[str]:
        try:
            out = subprocess.run(
                [self.cli, "list-models", "--format=json"],
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            ).stdout
            found = [m["name"] for m in json.loads(out).get("models", [])]
        except (OSError, subprocess.SubprocessError, ValueError, KeyError):
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
        value = "true" if streaming else "false"
        _run([self.cli, "set", "--assume-yes", "--no-restart", f"streaming={value}"])
        self.streaming = streaming
        self.config_suffix = ""
        subprocess.run(["snap", "restart", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            mode = "streaming" if streaming else "batch"
            raise SystemExit(f"[{self.snap}] socket did not return after switching to {mode}")

    def set_streaming_variant(self, settings: dict, suffix: str) -> None:
        set_args = ["streaming=true"] + [f"{k}={v}" for k, v in settings.items()]
        _run([self.cli, "set", "--assume-yes", "--no-restart", *set_args])
        self.streaming = True
        self.config_suffix = suffix
        subprocess.run(["snap", "restart", self.service], capture_output=True, check=False)
        if not wait_for_socket(self.socket):
            raise SystemExit(
                f"[{self.snap}] socket did not return after switching to streaming-{suffix}"
            )

    def use_model(self, model: str) -> None:
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

    @property
    def pid(self) -> int | None:
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


# ---------------------------------------------------------------------------
# JSONL output helper
# ---------------------------------------------------------------------------


class _JsonlWriter:
    """Append-only JSONL writer that serialises dicts one per line."""

    def __init__(self, path: Path):
        self._path = path
        self._fp = path.open("a", encoding="utf-8")

    def write(self, record: dict) -> None:
        self._fp.write(json.dumps(record) + "\n")
        self._fp.flush()

    def close(self) -> None:
        self._fp.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


# ---------------------------------------------------------------------------
# Sweep logic
# ---------------------------------------------------------------------------


def _sweep_one(
    *,
    target: SnapTarget,
    clips_cold: list,
    clips_warm: list,
    budget: float,
    out: _JsonlWriter,
    provenance: dict,
    resources_path: Path,
    sample_resources: bool,
    broken: list[tuple[str, str]],
    unusable: list[tuple[str, str]],
) -> None:
    from myna.benchmarker._bench import run_clips

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
                    budget_seconds=None,
                    out_fp=out,
                )
            )
            if overran:
                unusable.append((label, "cold sample overran"))
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
                budget_seconds=budget,
                out_fp=out,
            )
        )
        if overran:
            unusable.append((label, f"exceeded {budget:.0f}s budget"))

    except subprocess.CalledProcessError as exc:
        broken.append((label, f"exited {exc.returncode}"))
        print(f"[{label}] FAILED: exited {exc.returncode}")
    except Exception as exc:  # noqa: BLE001
        broken.append((label, f"{type(exc).__name__}: {exc}"))
        print(f"[{label}] FAILED: {type(exc).__name__}: {exc}")
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


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def cmd_run(args) -> None:  # noqa: ANN001
    config_path = Path(args.config)
    if not config_path.exists():
        raise SystemExit(
            f"config not found: {config_path}\n"
            "Create bench.yaml or pass --config path/to/bench.yaml.\n"
            "See: python3 myna-bench.pyz run --help"
        )

    cfg = yaml.safe_load(config_path.read_text(encoding="utf-8"))

    manifest_path = Path(cfg.get("manifest", "corpus/manifest.json"))
    out_path = Path(args.out or cfg.get("out", "results.jsonl"))
    cold_clip_id: str | None = cfg.get("cold_clip")
    warm_clip_ids: list[str] = cfg.get("clips") or []
    budget = args.budget or cfg.get("sweep_budget_seconds") or DEFAULT_SWEEP_BUDGET_S
    targets_cfg = cfg.get("targets") or []

    if not targets_cfg:
        raise SystemExit("no targets in config")

    if not manifest_path.exists():
        raise SystemExit(
            f"manifest not found: {manifest_path}\n"
            "Run: python3 myna-bench.pyz download-corpus --out corpus"
        )

    if os.geteuid() != 0:
        raise SystemExit(
            "run requires root (installs and purges snaps):\n"
            f"  sudo python3 {sys.argv[0]} run --config {args.config}"
        )

    _resolve_user_home()

    from myna.testbed.corpus import load_manifest

    all_clips = list(load_manifest(manifest_path))
    clip_by_id = {c.id: c for c in all_clips}

    if cold_clip_id:
        if cold_clip_id not in clip_by_id:
            raise SystemExit(
                f"cold_clip {cold_clip_id!r} not in manifest; "
                f"available: {sorted(clip_by_id)[:5]}..."
            )
        clips_cold = [clip_by_id[cold_clip_id]]
    else:
        clips_cold = []

    if warm_clip_ids:
        missing = [cid for cid in warm_clip_ids if cid not in clip_by_id]
        if missing:
            raise SystemExit(f"clips not in manifest: {missing}")
        clips_warm = [clip_by_id[cid] for cid in warm_clip_ids]
    else:
        clips_warm = [c for c in all_clips if c.id != cold_clip_id]

    out_path.parent.mkdir(parents=True, exist_ok=True)
    resources_path = out_path.parent / (out_path.stem + "-resources.jsonl")

    if not args.keep_results and out_path.exists():
        print(f"resetting {out_path}")
        out_path.unlink()
    if not args.keep_results and resources_path.exists():
        resources_path.unlink()

    from myna.benchmarker.machine import collect as collect_machine

    machine = collect_machine()
    print(
        f"\nmachine: {machine['hostname']}  cpu: {machine['cpu']}  ram: {machine['ram_gb']} GB"
        + (f"  gpu: {machine['gpu']} {machine['gpu_vram_gb']} GB" if machine["gpu"] else "")
    )
    print(f"manifest: {manifest_path.name}  cold={len(clips_cold)} warm={len(clips_warm)} clips")
    print(f"warm-sweep budget: {budget:.0f}s per target")
    print(f"output: {out_path}\n")

    broken: list[tuple[str, str]] = []
    unusable: list[tuple[str, str]] = []

    with _JsonlWriter(out_path) as out:
        out.write(machine)

        for spec in targets_cfg:
            target = SnapTarget(spec)
            print(f"\n=== {target.snap} ===")
            try:
                target.start()
                provenance = {
                    "machine": machine["hostname"],
                    "cpu": machine["cpu"],
                    "ram_gb": machine["ram_gb"],
                    "gpu": machine["gpu"],
                    "gpu_vram_gb": machine["gpu_vram_gb"],
                    "provision": "snap",
                }
                variants = target.models()
                togglable = target.supports_streaming()
                streaming_configs: list[dict] = list(spec.get("streaming_configs") or [])
                streaming_desc = (
                    [f"streaming-{sc['label']}" for sc in streaming_configs]
                    if streaming_configs
                    else (["streaming"] if togglable else [])
                )
                print(
                    f"[{target.snap}] variants={variants or '(none reported)'} "
                    f"modes={['batch'] + streaming_desc}"
                )

                for model in variants or [None]:
                    if model:
                        target.use_model(model)
                    for streaming in [False, True] if togglable else [False]:
                        if streaming and streaming_configs:
                            for sc in streaming_configs:
                                target.set_streaming_variant(
                                    settings=sc.get("settings") or {}, suffix=sc["label"]
                                )
                                _sweep_one(
                                    target=target,
                                    clips_cold=clips_cold,
                                    clips_warm=clips_warm,
                                    budget=budget,
                                    out=out,
                                    provenance=provenance,
                                    resources_path=resources_path,
                                    sample_resources=not args.no_resources,
                                    broken=broken,
                                    unusable=unusable,
                                )
                        else:
                            if togglable:
                                target.set_streaming(streaming)
                            _sweep_one(
                                target=target,
                                clips_cold=clips_cold,
                                clips_warm=clips_warm,
                                budget=budget,
                                out=out,
                                provenance=provenance,
                                resources_path=resources_path,
                                sample_resources=not args.no_resources,
                                broken=broken,
                                unusable=unusable,
                            )
            except SystemExit:
                raise
            except Exception as exc:  # noqa: BLE001
                broken.append((target.snap, f"{type(exc).__name__}: {exc}"))
                print(f"[{target.snap}] FAILED: {type(exc).__name__}: {exc} — skipping target")
            finally:
                target.stop()

    _chown_to_invoker(out_path)
    _chown_to_invoker(resources_path)

    if out_path.exists() and out_path.stat().st_size:
        print("\n===================== RESULTS =====================")
        from myna.benchmarker._summarize import cmd_summarize

        class _FakeArgs:
            infile = str(out_path)
            by_category = False

        cmd_summarize(_FakeArgs())

    if unusable:
        print(f"\n{len(unusable)} target(s) failed the usability budget:")
        for label, why in unusable:
            print(f"  - {label}: {why}")
    if broken:
        print(f"\n{len(broken)} target(s) failed:")
        for label, why in broken:
            print(f"  - {label}: {why}")

    print(f"\nresults written to {out_path}")
    print("Share this file with the project team to contribute to the leaderboard.")
