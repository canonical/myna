#!/usr/bin/env python3
"""Run every available adapter through a realistic session and report coverage.

Usage (from repo root):
    cd server
    uv run python ../dev/adapter_coverage.py [--adapter whisper sherpa ...]

Each adapter loads its real model weights, receives a fixture WAV via the
standard Harness, and coverage data is written to .coverage.adapter-<name>.
After all adapters have run, the script:
  1. Merges adapter runs with the existing test-suite data (.coverage.tests)
     if present, otherwise runs pytest first.
  2. Prints a per-file report showing test-only vs merged coverage so
     use-case-only and never-executed lines stand out.

Adapters skipped by default (no model available locally): nemotron, qwen-c.
"""

from __future__ import annotations

import argparse
import asyncio
import subprocess
import sys
import os
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SERVER = REPO_ROOT / "server"
FIXTURES = SERVER / "fixtures"
MANIFEST = FIXTURES / "manifest.json"

# Fixture clip used for every adapter — short (2.6 s), English, synthetic TTS.
DEFAULT_CLIP_ID = "quiet-pangram"
# Second clip gives longer audio with more varied sentences.
EXTRA_CLIP_ID = "quiet-weather"

ADAPTERS_DEFAULT = ["whisper", "sherpa", "parakeet", "funasr"]


# ---------------------------------------------------------------------------
# Adapter factory — mirrors server/src/myna/server/cli.py build_adapter logic
# but resolves defaults that the CLI leaves to the user.
# ---------------------------------------------------------------------------


def _build_adapter(name: str, model: str | None):
    """Instantiate the named adapter with its default (or supplied) model."""
    if name == "whisper":
        from myna.testbed.whisper import FasterWhisperAdapter
        return FasterWhisperAdapter(model or "tiny")

    if name == "sherpa":
        from myna.testbed.sherpa import SherpaAdapter, _default_model_dir
        return SherpaAdapter(model_dir=model or _default_model_dir())

    if name == "parakeet":
        from myna.testbed.parakeet import ParakeetAdapter, _default_model_dir
        return ParakeetAdapter(model_dir=model or _default_model_dir())

    if name == "funasr":
        from myna.testbed.funasr import FunasrAdapter, _default_model_dir
        return FunasrAdapter(model_dir=model or _default_model_dir())

    raise ValueError(f"unknown adapter: {name!r}")


# ---------------------------------------------------------------------------
# Session runner
# ---------------------------------------------------------------------------


async def run_adapter(adapter_name: str, model: str | None, clip_ids: list[str]) -> bool:
    """Run the adapter against each clip and print event counts. Returns True on success."""
    from myna.core import LoopbackClient, SessionConfig
    from myna.testbed import Harness, load_manifest

    clips = {c.id: c for c in load_manifest(MANIFEST)}
    missing = [cid for cid in clip_ids if cid not in clips]
    if missing:
        print(f"  [WARN] clips not in manifest: {missing}", flush=True)
        clip_ids = [cid for cid in clip_ids if cid in clips]
    if not clip_ids:
        print("  [SKIP] no valid clips", flush=True)
        return False

    try:
        adapter = _build_adapter(adapter_name, model)
    except Exception as exc:
        print(f"  [SKIP] adapter build failed: {exc}", flush=True)
        return False

    ok = True
    for clip_id in clip_ids:
        clip = clips[clip_id]
        print(f"  clip={clip_id!r} ({clip.duration_seconds:.1f}s) … ", end="", flush=True)
        source = clip.open_source()
        config = SessionConfig(audio_format=source.format, language=clip.language)
        try:
            record = await Harness().run(
                client=LoopbackClient(adapter),
                candidate=adapter.candidate,
                source=source,
                config=config,
            )
            kinds = [te.event.type for te in record.events]
            n_final = kinds.count("transcription.final")
            n_err = kinds.count("transcription.error")
            terminal = kinds[-1] if kinds else "?"
            rtf = f"{record.metrics.rtf:.2f}x" if record.metrics.rtf else "?RTF"
            status = "OK" if terminal == "transcription.done" and not n_err else "FAIL"
            print(f"{status} finals={n_final} errors={n_err} {rtf}", flush=True)
            if n_err:
                ok = False
        except Exception as exc:
            print(f"EXCEPTION: {exc}", flush=True)
            ok = False

    # Unload to free memory before the next adapter runs.
    if hasattr(adapter, "unload"):
        try:
            await adapter.unload()
        except Exception:
            pass

    return ok


# ---------------------------------------------------------------------------
# Coverage helpers
# ---------------------------------------------------------------------------


def _cov_bin() -> str:
    cov = SERVER / ".venv" / "bin" / "coverage"
    return str(cov) if cov.exists() else "coverage"


def run_baseline_if_needed() -> None:
    """Run pytest with coverage if .coverage.tests doesn't exist yet."""
    data_file = SERVER / ".coverage.tests"
    if data_file.exists():
        print(f"[cov] using existing {data_file.name}", flush=True)
        return
    print("[cov] .coverage.tests not found — running pytest … ", flush=True)
    cov = _cov_bin()
    r = subprocess.run(
        [cov, "run", "--data-file=.coverage.tests", "--branch",
         "--source=myna", "-m", "pytest", "-q", "--no-header"],
        cwd=SERVER,
        capture_output=False,
    )
    if r.returncode != 0:
        print("[cov] pytest failed — continuing without test baseline", flush=True)


def merge_and_report(adapter_names: list[str]) -> None:
    """Combine all .coverage.* data files and print a per-file report."""
    cov = _cov_bin()
    data_files: list[str] = []
    tests_file = SERVER / ".coverage.tests"
    if tests_file.exists():
        data_files.append(".coverage.tests")
    for name in adapter_names:
        pat = list(SERVER.glob(f".coverage.adapter-{name}.*"))
        if pat:
            data_files.extend(str(p.relative_to(SERVER)) for p in pat)
        else:
            # non-parallel-mode file
            single = SERVER / f".coverage.adapter-{name}"
            if single.exists():
                data_files.append(f".coverage.adapter-{name}")

    if not data_files:
        print("[cov] no coverage data files to merge", flush=True)
        return

    # Protect the test baseline: coverage combine deletes inputs by default
    # and --keep is unreliable across versions. Copy it aside and restore.
    import shutil
    tests_backup: Path | None = None
    if tests_file.exists():
        tests_backup = SERVER / ".coverage.tests.bak"
        shutil.copy2(tests_file, tests_backup)

    print(f"\n[cov] merging: {', '.join(data_files)}", flush=True)
    r = subprocess.run(
        [cov, "combine", "--keep"] + data_files,
        cwd=SERVER, capture_output=True, text=True,
    )

    # Restore baseline regardless of combine outcome
    if tests_backup and tests_backup.exists():
        shutil.copy2(tests_backup, tests_file)
        tests_backup.unlink()

    if r.returncode != 0:
        print(f"[cov] combine error: {r.stderr}", flush=True)
        return

    # Export Cobertura for populations script
    subprocess.run(
        [cov, "xml", "-o", "coverage-merged.cobertura.xml"],
        cwd=SERVER, capture_output=True,
    )

    # Human-readable terminal report
    print("\n" + "=" * 88, flush=True)
    print("MERGED COVERAGE (tests + adapter sessions)", flush=True)
    print("=" * 88, flush=True)
    subprocess.run(
        [cov, "report", "--sort=cover",
         "--include=*/testbed/*.py,*/testbed/streaming/*.py,*/server/*.py,*/core/*.py"],
        cwd=SERVER,
    )

    # If we also have test-only data, show delta
    if tests_file.exists():
        print("\n" + "=" * 88, flush=True)
        print("TEST-ONLY COVERAGE (for comparison)", flush=True)
        print("=" * 88, flush=True)
        subprocess.run(
            [cov, "report", "--data-file=.coverage.tests", "--sort=cover",
             "--include=*/testbed/*.py,*/testbed/streaming/*.py,*/server/*.py,*/core/*.py"],
            cwd=SERVER,
        )

    print("\n[cov] wrote server/coverage-merged.cobertura.xml", flush=True)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--adapter", nargs="+", default=ADAPTERS_DEFAULT,
        metavar="NAME",
        help=f"adapters to run (default: {' '.join(ADAPTERS_DEFAULT)})",
    )
    parser.add_argument(
        "--model", default=None,
        help="model override (applied to all adapters; use for quick tests)",
    )
    parser.add_argument(
        "--clips", nargs="+", default=[DEFAULT_CLIP_ID, EXTRA_CLIP_ID],
        metavar="CLIP_ID",
        help="fixture clip IDs to feed to each adapter",
    )
    parser.add_argument(
        "--skip-baseline", action="store_true",
        help="skip running pytest even if .coverage.tests is absent",
    )
    args = parser.parse_args()

    os.chdir(SERVER)
    sys.path.insert(0, str(SERVER / "src"))

    if not MANIFEST.exists():
        print(f"ERROR: manifest not found at {MANIFEST}")
        print("Run: cd server && uv run python ../dev/generate_fixtures.py")
        return 1

    if not args.skip_baseline:
        run_baseline_if_needed()

    cov = _cov_bin()
    ran: list[str] = []

    for adapter_name in args.adapter:
        print(f"\n{'─' * 60}", flush=True)
        print(f"Adapter: {adapter_name}", flush=True)
        data_file = SERVER / f".coverage.adapter-{adapter_name}"
        # Remove stale data from previous runs
        for stale in SERVER.glob(f".coverage.adapter-{adapter_name}*"):
            stale.unlink()

        # Run adapter session under coverage (parallel-mode so this process
        # and the adapter's threads all contribute to the same .coverage file)
        env = os.environ.copy()
        env["COVERAGE_FILE"] = str(data_file)
        env["COVERAGE_PROCESS_START"] = str(SERVER / "pyproject.toml")

        # We invoke ourselves recursively with coverage run so the adapter
        # code (loaded as a library) is instrumented.
        cmd = [
            cov, "run",
            f"--data-file={data_file}",
            "--branch", "--source=myna",
            "--parallel-mode",
            f"--context=adapter:{adapter_name}",
            __file__,
            "--adapter", adapter_name,
            "--clips", *args.clips,
            "--skip-baseline",  # inner run: don't nest pytest
            "--_inner",         # signal: just run the session, no merge
        ]
        if args.model:
            cmd += ["--model", args.model]

        r = subprocess.run(cmd, env=env)
        if r.returncode == 0:
            ran.append(adapter_name)
        else:
            print(f"  [WARN] adapter {adapter_name!r} run exited {r.returncode}", flush=True)

    print(f"\n{'─' * 60}", flush=True)
    merge_and_report(ran)
    return 0


def _inner_main() -> int:
    """Invoked by coverage run — just run the adapter session."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--adapter", nargs="+", required=True)
    parser.add_argument("--model", default=None)
    parser.add_argument("--clips", nargs="+", default=[DEFAULT_CLIP_ID])
    parser.add_argument("--skip-baseline", action="store_true")
    parser.add_argument("--_inner", action="store_true")
    args = parser.parse_args()

    os.chdir(SERVER)
    sys.path.insert(0, str(SERVER / "src"))

    adapter_name = args.adapter[0]
    ok = asyncio.run(run_adapter(adapter_name, args.model, args.clips))
    return 0 if ok else 1


if __name__ == "__main__":
    # Detect inner vs outer invocation.
    if "--_inner" in sys.argv:
        raise SystemExit(_inner_main())
    raise SystemExit(main())
