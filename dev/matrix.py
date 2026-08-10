"""Matrix runner: sweep model backends × configs and aggregate (T11).

Config-driven successor to ``dev/run-matrix.sh``. One YAML file lists the
*targets* to benchmark; the runner provisions each backend on a socket, takes a
**cold** sample (model-load-from-cold) then a **warm** sweep, stamps hardware
provenance onto every record, and finally prints the aggregate matrix.

Two provisioners, so you can work **locally before anything is in the store**:

- ``server`` — the runner spawns ``myna-server`` itself (no snap, no sudo). The
  freshly-started process loads the model lazily on the first request, so the
  cold sample is genuinely cold. This is the local-first path.
- ``snap`` — drive an already-installed snap: optionally switch its engine/model
  and ``snap restart`` it to force a cold load. For store / local-snap runs.

    uv run python dev/matrix.py --config dev/matrix.yaml
    uv run python dev/matrix.py --config dev/matrix.yaml --only whisper-base/cpu
    uv run python dev/matrix.py --config dev/matrix.yaml --dry-run

The runner never scores anything itself — it shells out to ``dev/bench.py`` per
target (one source of truth for WER/latency) and ``dev/aggregate.py`` at the end.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import psutil
import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
VENV_BIN = Path(sys.executable).parent
LOG_DIR = REPO_ROOT / "results" / "matrix-logs"


def _server_cmd() -> list[str]:
    """The ``myna-server`` entry point in the active venv (fallback: PATH)."""
    candidate = VENV_BIN / "myna-server"
    return [str(candidate)] if candidate.exists() else ["myna-server"]


def wait_for_socket(path: Path, timeout: float = 60.0) -> bool:
    """Poll until the server has bound the UDS (the file appears), or timeout.

    The server creates the socket file only once it is listening, so its
    existence is a sufficient readiness signal — and unlike a bare connect()
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
        parts = [p.strip() for p in line.split(",")]
        if len(parts) == 2 and parts[0].isdigit() and parts[1].isdigit():
            usage[int(parts[0])] = int(parts[1])
    return usage


class ResourceSampler(threading.Thread):
    """Background peak-memory sampler for a process tree (RSS via psutil, VRAM
    via nvidia-smi). Sampled at a modest interval so it barely perturbs the
    latency it runs alongside; disable with ``--no-resources`` for pristine
    timing. ``peak_vram_mb`` stays None when no GPU process is seen.
    """

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


class ServerTarget:
    """A backend the runner spawns itself (``provision: server``)."""

    def __init__(self, spec: dict):
        self.label = spec["label"]
        self.adapter = spec["adapter"]
        self.model = spec.get("model")
        self.device = spec.get("device")
        self.env = spec.get("env", {})
        self.socket = Path(spec.get("socket") or f"/tmp/myna-matrix-{_slug(self.label)}.sock")
        self._proc: subprocess.Popen | None = None
        self._log = None
        self.logpath = LOG_DIR / f"{_slug(self.label)}.log"

    def start(self) -> None:
        if self.socket.exists():
            self.socket.unlink()
        cmd = _server_cmd() + ["--adapter", self.adapter, "--socket", str(self.socket)]
        if self.model:
            # A model can be a bare name (whisper "base") or a local directory
            # (qwen weights). Resolve the latter against the repo so the spawned
            # server finds it regardless of cwd.
            candidate = REPO_ROOT / str(self.model)
            cmd += [
                "--model",
                str(candidate) if candidate.exists() else str(self.model),
            ]
        if self.device:
            cmd += ["--device", self.device]
        env = {**os.environ, **{k: str(v) for k, v in self.env.items()}}
        # Redirect the server's own logs to a file so they don't interleave with
        # the matrix/bench stdout (the noisy `websockets.server INFO connection
        # open` lines). The file stays for debugging.
        LOG_DIR.mkdir(parents=True, exist_ok=True)
        self._log = self.logpath.open("w")
        self._proc = subprocess.Popen(cmd, env=env, stdout=self._log, stderr=subprocess.STDOUT)
        if not wait_for_socket(self.socket):
            self.stop()
            raise SystemExit(f"[{self.label}] server did not come up on {self.socket}")
        print(f"[{self.label}] server up (logs: {self.logpath.relative_to(REPO_ROOT)})")

    def stop(self) -> None:
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self._proc.kill()
            self._proc = None
        if self._log is not None:
            self._log.close()
            self._log = None
        if self.socket.exists():
            self.socket.unlink()

    @property
    def pid(self) -> int | None:
        return self._proc.pid if self._proc is not None else None

    # A spawned process is cold on start; no per-sample reset needed between the
    # cold and warm bench calls (one long-lived process, like the snap daemon).
    def make_cold(self) -> None:
        pass


class SnapTarget:
    """An installed snap the runner drives (``provision: snap``)."""

    def __init__(self, spec: dict):
        self.label = spec["label"]
        self.snap = spec["snap"]
        self.socket = Path(spec["socket"])
        self.use_engine = spec.get("use_engine")
        self.use_model = spec.get("use_model")
        self.service = spec.get("service", f"{self.snap}.server")

    def start(self) -> None:
        if self.use_engine:
            _sudo(self.snap, "use-engine", self.use_engine, "--assume-yes")
        if self.use_model:
            _sudo(self.snap, "use-model", self.use_model, "--assume-yes")
        self.make_cold()

    def stop(self) -> None:
        pass  # leave the snap running

    def make_cold(self) -> None:
        subprocess.run(["sudo", "snap", "restart", self.service], check=True)
        if not wait_for_socket(self.socket):
            raise SystemExit(f"[{self.label}] snap socket {self.socket} did not appear")

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


def _sudo(snap: str, *args: str) -> None:
    subprocess.run(["sudo", snap, *args], check=True)


def _slug(label: str) -> str:
    return "".join(c if c.isalnum() else "-" for c in label).strip("-")


def make_target(spec: dict):
    kind = spec.get("provision", "server")
    if kind == "server":
        return ServerTarget(spec)
    if kind == "snap":
        return SnapTarget(spec)
    raise SystemExit(f"unknown provision kind: {kind!r} (want server|snap)")


def run_bench(
    *,
    socket: Path,
    label: str,
    manifest: Path,
    out: Path,
    provenance: dict,
    cold: bool,
    clips: list[str],
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
    cmd += clips
    subprocess.run(cmd, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--config", type=Path, default=REPO_ROOT / "dev" / "matrix.yaml")
    parser.add_argument("--only", action="append", help="run only this target label (repeatable)")
    parser.add_argument("--dry-run", action="store_true", help="print the plan; provision nothing")
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
    args = parser.parse_args()

    if not args.config.exists():
        raise SystemExit(f"no config at {args.config}")
    cfg = yaml.safe_load(args.config.read_text(encoding="utf-8"))

    manifest = (REPO_ROOT / cfg.get("manifest", "corpus/real/manifest.json")).resolve()
    out = (REPO_ROOT / cfg.get("out", "results/bench.jsonl")).resolve()
    hardware = cfg.get("hardware", {})
    cold_clip = cfg.get("cold_clip")
    warm_clips = cfg.get("clips", [])
    targets = cfg.get("targets", [])
    if args.only:
        targets = [t for t in targets if t["label"] in set(args.only)]
    if not targets:
        raise SystemExit("no targets selected")

    print(f"config={args.config}  manifest={manifest.name}  out={out}")
    print(f"hardware={hardware or '(unset)'}")
    print(f"targets: {', '.join(t['label'] for t in targets)}")
    if cold_clip:
        print(f"cold clip: {cold_clip}")
    if args.dry_run:
        for spec in targets:
            t = make_target(spec)
            print(f"  - {t.label:24} provision={spec.get('provision', 'server')} socket={t.socket}")
        return

    if not args.keep_results and out.exists():
        print(f"resetting {out}")
        out.unlink()
    resources_path = out.parent / "matrix-resources.jsonl"
    if not args.keep_results and resources_path.exists():
        resources_path.unlink()

    for spec in targets:
        target = make_target(spec)
        provenance = {**hardware, "provision": spec.get("provision", "server")}
        print(f"\n=== {target.label} ===")
        sampler = None
        try:
            target.start()
            if not args.no_resources and target.pid is not None:
                sampler = ResourceSampler(target.pid)
                sampler.start()
            if cold_clip:
                print(f"[{target.label}] cold sample ({cold_clip})")
                run_bench(
                    socket=target.socket,
                    label=target.label,
                    manifest=manifest,
                    out=out,
                    provenance=provenance,
                    cold=True,
                    clips=[cold_clip],
                )
            print(f"[{target.label}] warm sweep")
            run_bench(
                socket=target.socket,
                label=target.label,
                manifest=manifest,
                out=out,
                provenance=provenance,
                cold=False,
                clips=list(warm_clips),
            )
        finally:
            if sampler is not None:
                sampler.stop()
                rss = round(sampler.peak_rss_mb, 1)
                vram = round(sampler.peak_vram_mb, 1) if sampler.peak_vram_mb else None
                print(
                    f"[{target.label}] peak RSS {rss} MB"
                    + (f" / VRAM {vram} MB" if vram else " / VRAM --")
                )
                with resources_path.open("a", encoding="utf-8") as fp:
                    fp.write(
                        json.dumps(
                            {
                                "label": target.label,
                                "machine": hardware.get("machine"),
                                "peak_rss_mb": rss,
                                "peak_vram_mb": vram,
                            }
                        )
                        + "\n"
                    )
            target.stop()

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


if __name__ == "__main__":
    main()
