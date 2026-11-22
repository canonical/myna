"""``myna-bench plugin-eval``: fetch, build, serve, grade and gate one plugin.

The pipeline behind ``dev/plugin-pipeline.sh <plugin-id>``. Starting from a
plugin file (``<snap>-snap/plugins/<name>.toml``) it

1. refuses a licence off the allow-list or a missing or stale NOTICE;
2. fetches the pinned files (Remote Settings CDN) into the model cache and
   refuses any whose sha256 or size differs from the plugin file;
3. stages the component the way ``snapcraft`` would lay it out;
4. serves it through the real provider daemon (``myna.translation`` or
   ``myna.text``) on a Unix socket;
5. grades it with the text suite (``text_suite``) under the gate config of its
   file (``text_gate``), and records the peak RSS of the daemon tree;
6. writes the full results (hypotheses, versions, hashes) under the results
   directory and the slim ``plugins/<name>.measured.json`` the generator
   reads. ``quality`` is never typed by hand: only this step writes it.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from myna.benchmarker import text_corpus, text_suite
from myna.benchmarker.text_gate import (
    GateConfig,
    GateError,
    find_plugin,
    load_plugin,
    socket_system,
)

LICENSES = ("Apache-2.0", "MIT", "MPL-2.0", "CC-BY-4.0")
COMMON_LICENSES = Path("/usr/share/common-licenses")
SCHEMA = 1
HOST = "chug"


class PipelineError(Exception):
    """The plugin must not be graded or published as it stands."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def licence_allowed(name: str) -> bool:
    import re

    return name in LICENSES or re.fullmatch(r"BSD(-[0-9]-Clause)?", name) is not None


def check_inputs(plugin: dict[str, Any], snap_dir: Path) -> Path:
    """Licence on the allow-list and the generated NOTICE naming every file.

    Returns the component's generated model directory."""
    if not licence_allowed(plugin.get("license", "")):
        raise PipelineError(f"licence {plugin.get('license')!r} is not on the allow-list")
    models: Path = snap_dir / "models" / plugin["name"]
    notice = models / "NOTICE"
    if not notice.is_file() or not notice.read_text(encoding="utf-8").strip():
        raise PipelineError(f"{notice}: missing NOTICE (run dev/gen-plugins.py)")
    text = notice.read_text(encoding="utf-8")
    for entry in plugin["files"]:
        if entry["sha256"] not in text:
            raise PipelineError(f"{notice} does not list {entry['name']} (run dev/gen-plugins.py)")
    if not (models / "myna-text-model.json").is_file():
        raise PipelineError(f"{models}: no myna-text-model.json (run dev/gen-plugins.py)")
    return models


def fetch_files(plugin: dict[str, Any], cache: Path) -> dict[str, Path]:
    """Every pinned file in ``cache``, verified; a mismatch deletes it and refuses."""
    cache.mkdir(parents=True, exist_ok=True)
    out: dict[str, Path] = {}
    for entry in plugin["files"]:
        path = cache / entry["name"]
        if not path.is_file() or sha256_file(path) != entry["sha256"]:
            url = entry.get("url")
            if not url:
                raise PipelineError(f"{entry['name']}: not cached and has no url")
            part = path.with_suffix(path.suffix + ".part")
            with urllib.request.urlopen(url, timeout=60) as reply, part.open("wb") as handle:  # noqa: S310
                shutil.copyfileobj(reply, handle)
            part.replace(path)
        if sha256_file(path) != entry["sha256"] or path.stat().st_size != entry["bytes"]:
            path.unlink()
            raise PipelineError(f"{entry['name']}: does not match the sha256 pinned in the plugin")
        out[entry["name"]] = path
    return out


def stage_component(
    plugin: dict[str, Any], models: Path, files: dict[str, Path], components: Path
) -> Path:
    """Lay the component out as the snap does: files, manifest, NOTICE, LICENSE."""
    out: Path = components / plugin["component"]
    out.mkdir(parents=True)
    for name, path in files.items():
        if _same_fs(path, out):
            os.link(path, out / name)
        else:
            shutil.copy2(path, out / name)
    for item in models.iterdir():
        shutil.copy2(item, out / item.name)
    licence = Path(plugin["license_file"]) if "license_file" in plugin else None
    licence = licence or COMMON_LICENSES / plugin["license"]
    if licence.is_file():
        shutil.copy2(licence, out / "LICENSE")
    return out


def _same_fs(a: Path, b: Path) -> bool:
    return a.stat().st_dev == b.stat().st_dev


def process_tree(pid: int) -> list[int]:
    """``pid`` and its descendants, from /proc."""
    children: dict[int, list[int]] = {}
    for stat in Path("/proc").glob("[0-9]*/stat"):
        try:
            fields = stat.read_text().rsplit(")", 1)[1].split()
            children.setdefault(int(fields[1]), []).append(int(stat.parent.name))
        except (OSError, IndexError, ValueError):
            continue
    tree, todo = [], [pid]
    while todo:
        current = todo.pop()
        tree.append(current)
        todo.extend(children.get(current, []))
    return tree


def peak_rss_mb(pids: list[int]) -> float:
    """Summed VmHWM of the processes, in MB."""
    total = 0.0
    for pid in pids:
        try:
            for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmHWM"):
                    total += int(line.split()[1]) / 1024
        except OSError:
            continue
    return total


@dataclass
class Daemon:
    process: subprocess.Popen[bytes]

    def stop(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def start_daemon(
    plugin: dict[str, Any], components: Path, sock: Path, bergamot: Path | None
) -> Daemon:
    module = "myna.translation.cli" if plugin["snap"] == "myna-translation" else "myna.text.cli"
    cmd = [sys.executable, "-m", module, "--socket", str(sock), "--components", str(components)]
    if module == "myna.translation.cli":
        if bergamot is None or not bergamot.is_file():
            raise PipelineError(f"myna-bergamot not found ({bergamot}); pass --bergamot")
        cmd += ["--bergamot", str(bergamot)]
    process = subprocess.Popen(cmd, stderr=subprocess.PIPE)  # noqa: S603
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if process.poll() is not None:
            err = (process.stderr.read() if process.stderr else b"").decode(errors="replace")
            raise PipelineError(f"{module} exited {process.returncode}: {err[-400:]}")
        if sock.exists():
            try:
                with socket.socket(socket.AF_UNIX) as probe:
                    probe.connect(str(sock))
                return Daemon(process)
            except OSError:
                pass
        time.sleep(0.1)
    process.kill()
    raise PipelineError(f"{module} did not open {sock} in 30 s")


def spot_check_state(plugin: dict[str, Any]) -> str:
    """``passed``, ``waived``, ``failed`` or ``missing``."""
    spot = plugin.get("spot_check")
    if spot:
        if not all(spot.get(k) for k in ("who", "date")) or not isinstance(
            spot.get("passed"), bool
        ):
            raise PipelineError("spot_check needs who, date and passed")
        return "passed" if spot["passed"] else "failed"
    return "waived" if plugin.get("gate", {}).get("spot_check_waiver") else "missing"


def quality_source(gate: GateConfig, score: float | None, n: int, suite: str) -> str:
    if score is None:
        return "not measured"
    measured = f"chrF {score:.3f} on {n} FLEURS pairs ({suite})"
    if gate.vendor_comet is not None:
        return f"{measured}; vendor FLORES COMET {gate.vendor_comet:g}"
    return f"{measured}; quality not measured by the vendor"


def build_results(
    plugin: dict[str, Any],
    report: text_suite.Report,
    suites: list[str],
    gate: GateConfig | None,
    meta: dict[str, Any],
) -> dict[str, Any]:
    gates = text_suite.to_json(report)["gates"]
    passed = bool(gates) and all(g["status"] == "pass" for g in gates)
    measured: dict[str, Any] = {}
    if plugin["task"] == "text.translate":
        detail = report.detail[suites[0]]
        chrf = next(g["value"] for g in gates if g["metric"] == "chrf")
        measured = {
            "chrf": chrf,
            "latency_ms": detail["latency_ms"],
            "n": detail["n"],
            "vendor_comet": gate.vendor_comet if gate else None,
        }
    return {
        "schema": SCHEMA,
        "plugin": plugin["id"],
        "component": plugin["component"],
        "snap": plugin["snap"],
        "gate": "pass" if passed else "fail",
        "spot_check": spot_check_state(plugin),
        "measured": measured,
        "gates": gates,
        "detail": report.detail,
        **meta,
    }


def measured_json(
    results: dict[str, Any],
    gate: GateConfig | None,
    suites: list[str],
    results_sha: str,
) -> dict[str, Any]:
    """The slim file the generator reads (``plugins/<name>.measured.json``)."""
    m = results["measured"]
    if m:
        p95 = m["latency_ms"]["p95"]
        score = round(m["chrf"], 3)
        source = quality_source(gate or GateConfig(), m["chrf"], m["n"], suites[0])
    else:
        values = {f"{g['suite']}.{g['metric']}": g["value"] for g in results["gates"]}
        p95 = max(results["detail"][s]["latency_ms"]["p95"] for s in suites)
        reduction, uer = values["disfluent_en.reduction"], values["clean_en.uer"]
        score = round(reduction, 3)
        source = f"cleanup reduction {reduction:.3f}, UER {uer:.4f} on the text corpus"
    return {
        "latency_ms": max(1, math.ceil(p95)),
        "quality": score,
        "quality_source": source,
        "gate": results["gate"],
        "results_sha256": results_sha,
    }


def results_dir_default() -> Path:
    return Path(os.environ.get("MYNA_RESULTS_DIR", "~/myna-processing-graphs/results")).expanduser()


def run(args: argparse.Namespace) -> dict[str, Any]:
    repo = Path(os.environ.get("MYNA_REPO_ROOT") or Path(__file__).resolve().parents[4])
    if socket.gethostname() != os.environ.get("MYNA_PIPELINE_HOST", HOST):
        raise PipelineError(
            f"the pipeline runs on chug only (this host is {socket.gethostname()}); "
            "use ~/myna-processing-graphs/chug.sh"
        )
    path = find_plugin(repo, args.plugin)
    plugin = load_plugin(path)
    snap_dir = path.parent.parent
    models = check_inputs(plugin, snap_dir)
    cache = Path(args.cache).expanduser() / plugin["snap"]
    files = fetch_files(plugin, cache)

    suites, gates = text_suite.plugin_plan(plugin)
    gate = next(iter(gates.values()), None)
    bergamot = Path(args.bergamot).expanduser() if args.bergamot else _find_bergamot()
    with tempfile.TemporaryDirectory(prefix="plugin-eval-") as scratch:
        components = Path(scratch) / "components"
        components.mkdir()
        stage_component(plugin, models, files, components)
        sock = Path(scratch) / "text.sock"
        daemon = start_daemon(plugin, components, sock, bergamot)
        try:
            system = socket_system(str(sock), plugin["id"])
            report = text_suite.evaluate(system, suites, args.repeats, gates)
            rss = peak_rss_mb(process_tree(daemon.process.pid))
        finally:
            daemon.stop()
    meta = {
        "rss_mb": round(rss, 1),
        "versions": {
            "python": platform.python_version(),
            "bergamot_sha256": sha256_file(bergamot) if bergamot and bergamot.is_file() else None,
            "corpus": {
                f"{n}.jsonl": text_corpus.sha256_file(text_corpus.CORPUS_DIR / f"{n}.jsonl")
                for n in suites
                if (text_corpus.CORPUS_DIR / f"{n}.jsonl").is_file()
            },
            "files": {e["name"]: e["sha256"] for e in plugin["files"]},
            "pin": plugin.get("pin"),
        },
        "gate_config": vars(gate) if gate else None,
        "repeats": args.repeats,
        "date": time.strftime("%Y-%m-%d"),
    }
    results = build_results(plugin, report, suites, gate, meta)
    out_dir = Path(args.results_dir or results_dir_default()) / "plugins"
    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / f"{plugin['id']}.json"
    out.write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    slim = measured_json(results, gate, suites, sha256_file(out))
    if not args.no_measured:
        target = path.with_suffix(".measured.json")
        target.write_text(json.dumps(slim, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        results["measured_file"] = str(target)
    results["results_file"] = str(out)
    print(text_suite.format_report(report))
    print(f"gate: {results['gate']}  spot check: {results['spot_check']}  rss: {rss:.0f} MB")
    print(f"results: {out}")
    return results


SNAP_BERGAMOT = Path("/snap/myna-translation/current/bin/myna-bergamot")


def _find_bergamot() -> Path | None:
    found = os.environ.get("MYNA_BERGAMOT") or shutil.which("myna-bergamot")
    if found:
        return Path(found)
    return SNAP_BERGAMOT if SNAP_BERGAMOT.is_file() else None


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("plugin", help="plugin id (or its toml file)")
    parser.add_argument("--bergamot", help="the myna-bergamot binary ($MYNA_BERGAMOT, PATH, snap)")
    parser.add_argument(
        "--cache",
        default=os.environ.get("MYNA_MODEL_CACHE", "~/.cache/myna/models"),
        help="model download cache (shared with dev/stage-plugins.sh)",
    )
    parser.add_argument(
        "--results-dir",
        help="where plugins/<id>.json goes (default ~/myna-processing-graphs/results)",
    )
    parser.add_argument("--repeats", type=int, default=3, help="warm latency passes (default 3)")
    parser.add_argument(
        "--no-measured",
        action="store_true",
        help="do not rewrite plugins/<name>.measured.json",
    )


def cmd_plugin_eval(args: argparse.Namespace) -> None:
    try:
        results = run(args)
    except (PipelineError, GateError, text_corpus.CorpusError) as err:
        print(f"error: {err}", file=sys.stderr)
        sys.exit(2)
    if results["gate"] != "pass":
        sys.exit(1)
