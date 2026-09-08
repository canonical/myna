"""Myna benchmarker - the one tool that produces a Myna benchmark number.

Collect STT accuracy and latency results across the Myna inference snaps, on
your own machine, and share the results file with the project team.

Quick start:

    # 1. Download an evaluation corpus (~330 MB, one-time)
    myna-bench download-corpus --out ./corpus --select balanced -n 80

    # 2. Point bench.yaml at your snap files (see bench.yaml.example)
    myna-bench plan --config bench.yaml       # what would be measured

    # 3. Run the sweep (requires root - installs/removes snaps)
    sudo myna-bench run --config bench.yaml

    # 4. Inspect locally, then share results.jsonl with the team
    myna-bench summarize --in results.jsonl --by-category

Every subcommand is also reachable as ``python3 myna-bench.pyz <command>``
outside a checkout, which is how testers run it.
"""

from __future__ import annotations

import argparse


def _add_target_selection(parser: argparse.ArgumentParser) -> None:
    """Options shared by ``run`` and ``plan``, so a plan matches its run."""
    parser.add_argument(
        "--config",
        default="bench.yaml",
        help="benchmark config YAML (default: bench.yaml)",
    )
    parser.add_argument(
        "--only",
        action="append",
        help="run only this snap (repeatable)",
    )
    parser.add_argument(
        "--out",
        default=None,
        help="output JSONL path (overrides the config)",
    )
    parser.add_argument(
        "--budget",
        type=float,
        default=None,
        help="warm-sweep wall-clock budget in seconds (overrides the config)",
    )
    parser.add_argument(
        "--label-suffix",
        default="",
        help=(
            "tag every label as <snap>+<suffix>, to keep two builds of one snap apart"
            " (the summary dedups by label, so without it a rebuild shadows the run"
            " it was meant to be compared against)"
        ),
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="myna-bench",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = parser.add_subparsers(dest="command", metavar="command")
    sub.required = True

    # -- run ---------------------------------------------------------------
    p_run = sub.add_parser(
        "run",
        help="run the full snap sweep and write a results file",
        description=(
            "Install each snap in the config, sweep every model x mode x config\n"
            "combination over the corpus, write results.jsonl, purge.\n"
            "Requires root (snap install/remove). Run with sudo.\n\n"
            "The output file starts with a machine-summary record followed by\n"
            "one bench record per clip x snap x model x mode x config, plus one\n"
            "status record per label saying how that cell finished."
        ),
    )
    _add_target_selection(p_run)
    p_run.add_argument(
        "--keep-results",
        action="store_true",
        help="append to the results file instead of resetting it",
    )
    p_run.add_argument(
        "--no-resources",
        action="store_true",
        help="skip peak RAM/VRAM sampling (for cleaner latency timing)",
    )
    p_run.add_argument(
        "--skip-env-check",
        action="store_true",
        help=(
            "record numbers even when the environment guard objects"
            " (a contaminated machine produces plausible, wrong results)"
        ),
    )

    # -- plan --------------------------------------------------------------
    p_plan = sub.add_parser(
        "plan",
        help="print the matrix this config would sweep; install nothing",
        description=(
            "Read the config and each target's engine.yaml and install hook, and\n"
            "print every row the sweep would produce, plus an upper bound on its\n"
            "wall clock. Needs no root and touches no snap.\n\n"
            "It is a prediction, not a reading: which engine wins is decided by\n"
            "hardware detection on the machine at run time."
        ),
    )
    _add_target_selection(p_plan)

    # -- bench -------------------------------------------------------------
    p_bench = sub.add_parser(
        "bench",
        help="score one already-running socket (no install, no purge)",
        description=(
            "Sweep a manifest against a socket that is already serving, and append\n"
            "records to a results file. Use when a snap is installed and configured\n"
            "the way you want it and you only need the numbers."
        ),
    )
    p_bench.add_argument("clip", nargs="*", help="clip ids (default: all in the manifest)")
    p_bench.add_argument("--socket", required=True, help="path to the server's Unix socket")
    p_bench.add_argument("--manifest", required=True, help="corpus manifest to sweep")
    p_bench.add_argument("--label", required=True, help="tag for this run, e.g. whisper/cpu/tiny")
    p_bench.add_argument("--out", default="results.jsonl", help="results JSONL to append to")
    p_bench.add_argument("--category", help="only clips in this UD129 category")
    p_bench.add_argument(
        "--streaming",
        action="store_true",
        help="score the progressive metrics; must match how the server was launched",
    )
    p_bench.add_argument(
        "--cold",
        action="store_true",
        help="tag records as a cold-load sample (first request after a restart)",
    )
    p_bench.add_argument(
        "--realtime",
        action="store_true",
        help="feed audio at real-time pace instead of as fast as the socket accepts",
    )
    p_bench.add_argument("--provenance", help="JSON object merged into every record")
    p_bench.add_argument(
        "--budget-seconds",
        type=float,
        default=None,
        help="wall-clock budget; overrunning stops early and exits 2",
    )

    # -- download-corpus ---------------------------------------------------
    p_dl = sub.add_parser(
        "download-corpus",
        help="download the English LibriSpeech evaluation corpus",
        description=(
            "Download a clip set from LibriSpeech (CC-BY-4.0) and write a manifest\n"
            "ready for bench.yaml. Requires ffmpeg for FLAC decode."
        ),
    )
    p_dl.add_argument("--out", default="corpus/english", help="output directory")
    p_dl.add_argument("--cache", default=".cache/librispeech", help="tarball cache dir")
    p_dl.add_argument(
        "--tarball",
        default=None,
        help="use an already-downloaded <subset>.tar.gz instead of fetching",
    )
    p_dl.add_argument(
        "--subset",
        choices=("dev-clean", "dev-other", "test-clean", "test-other"),
        default="dev-clean",
        help=(
            "LibriSpeech split (default dev-clean). The '-other' splits are the"
            " harder, accented/low-fidelity half - give them their own --out,"
            " one split per corpus dir"
        ),
    )
    p_dl.add_argument("-n", type=int, default=12, help="number of clean clips (default 12)")
    p_dl.add_argument(
        "--select",
        choices=("archive", "balanced"),
        default="archive",
        help=(
            "clip selection: 'archive' = first N in archive order (one speaker);"
            " 'balanced' = round-robin over every speaker in the split - use this"
            " for accuracy benchmarks"
        ),
    )
    p_dl.add_argument(
        "--manifest-name",
        default="manifest.json",
        help="manifest filename inside --out; a distinct name adds a tier"
        " alongside an existing one",
    )
    p_dl.add_argument(
        "--long-form-minutes",
        type=float,
        default=None,
        help=(
            "also concatenate one whole chapter, in reading order, into a single"
            " continuous clip of at least this many minutes, category 'long-form' -"
            " for rolling-window and buffer invariants that only show up minutes"
            " into a session. Pass -n 0 for a long-form-only manifest"
        ),
    )
    p_dl.add_argument(
        "--skip-complete",
        action="store_true",
        help="exit 0 without downloading when --out already holds exactly this corpus (for CI)",
    )

    # -- download-corpus-zh ------------------------------------------------
    p_zh = sub.add_parser(
        "download-corpus-zh",
        help="download the Chinese FLEURS evaluation corpus",
        description=(
            "Download the FLEURS Mandarin test split (CC-BY-4.0) and write a\n"
            "manifest in the same schema as the English tier.\n"
            "Needs huggingface_hub, soundfile and numpy installed."
        ),
    )
    p_zh.add_argument("--out", default="corpus/chinese", help="output directory")
    p_zh.add_argument("--cache", default=".cache/fleurs", help="download cache dir")
    p_zh.add_argument("-n", type=int, default=50, help="number of clips to select")

    # -- make-corpus -------------------------------------------------------
    p_mk = sub.add_parser(
        "make-corpus",
        help="build a manifest.json from a directory of WAV files",
        description=(
            "Walk --dir for *.wav files and produce a manifest.json.\n\n"
            "Each WAV needs a matching <stem>.txt sidecar containing the exact\n"
            "reference transcript on a single line. Optionally, a <stem>.category\n"
            "file overrides the default UD129 category for that clip.\n\n"
            "Example directory layout:\n"
            "  my-clips/\n"
            "    hello.wav\n"
            "    hello.txt          # 'hello world'\n"
            "    hello.category     # 'quiet'  (optional)\n"
        ),
    )
    p_mk.add_argument("--dir", required=True, help="directory containing *.wav files")
    p_mk.add_argument("--out", default=None, help="output dir for manifest.json (default: --dir)")
    p_mk.add_argument("--language", default="en", help="BCP-47 language code for all clips")
    p_mk.add_argument(
        "--category",
        default="quiet",
        help="default UD129 category; override per-clip with <stem>.category",
    )

    # -- summarize ---------------------------------------------------------
    p_sum = sub.add_parser(
        "summarize",
        help="print an aggregate WER/latency table from a results file",
        description="Read a results.jsonl and print a comparison table across all labels.",
    )
    p_sum.add_argument("--in", dest="infile", default="results.jsonl", help="results JSONL to read")
    p_sum.add_argument(
        "--by-category", action="store_true", help="also break WER down by UD129 category"
    )
    p_sum.add_argument(
        "--sort",
        choices=("wer", "cer", "speed", "latency", "cold-load", "label"),
        default="wer",
        help="rank rows best-first by this metric (default: wer)",
    )
    p_sum.add_argument(
        "--corpus", help="corpus id to report on; required when the file holds more than one"
    )

    # -- merge -------------------------------------------------------------
    p_merge = sub.add_parser(
        "merge",
        help="fold result submissions into a tracked leaderboard file",
        description=(
            "Append one or more results.jsonl submissions to a leaderboard file.\n\n"
            "Re-submitting from the same machine replaces that machine's rows rather\n"
            "than doubling them; no other machine's rows are touched. Refuses a\n"
            "submission measured against a different corpus, and refuses two hosts\n"
            "that share a hostname - both would silently lose a submission."
        ),
    )
    p_merge.add_argument("results", nargs="+", help="results JSONL submissions to fold in")
    p_merge.add_argument(
        "--leaderboard",
        default="leaderboard.jsonl",
        help="the tracked file to update (default: leaderboard.jsonl)",
    )
    p_merge.add_argument(
        "--allow-mixed-corpora",
        action="store_true",
        help="keep submissions scored against different corpora in one file "
        "(summarize then needs --corpus)",
    )

    # -- check -------------------------------------------------------------
    p_chk = sub.add_parser(
        "check",
        help="report whether this machine is fit to benchmark on",
        description=(
            "Check governor, cgroup caps, core homogeneity, competing processes and\n"
            "load. Every check here has a demonstrated failure mode: the first pass\n"
            "of an early baseline was wrong by 18x because the shell ran under an\n"
            "800 MB cgroup cap. Reports only - fixing is the operator's call."
        ),
    )
    p_chk.add_argument("--force", action="store_true", help="exit 0 even on hard violations")
    p_chk.add_argument("--json", action="store_true", help="print violations as a JSON array")
    p_chk.add_argument(
        "--model",
        default="parakeet",
        help="which model family's memory floor and competing service to check",
    )
    p_chk.add_argument(
        "--sweep",
        action="store_true",
        help="check only what applies to a snap sweep (no cgroup or pinning checks)",
    )

    return parser


def main() -> None:
    args = build_parser().parse_args()

    if args.command == "run":
        from myna.benchmarker._run import cmd_run

        cmd_run(args)
    elif args.command == "plan":
        from myna.benchmarker._run import cmd_plan

        cmd_plan(args)
    elif args.command == "bench":
        from myna.benchmarker._bench import cmd_bench

        cmd_bench(args)
    elif args.command == "download-corpus":
        from myna.benchmarker.corpus_english import cmd_download

        cmd_download(args)
    elif args.command == "download-corpus-zh":
        from myna.benchmarker.corpus_chinese import cmd_download_zh

        cmd_download_zh(args)
    elif args.command == "make-corpus":
        from myna.benchmarker._corpus import cmd_make

        cmd_make(args)
    elif args.command == "summarize":
        from myna.benchmarker._summarize import cmd_summarize

        cmd_summarize(args)
    elif args.command == "merge":
        from myna.benchmarker._summarize import cmd_merge

        cmd_merge(args)
    elif args.command == "check":
        from myna.benchmarker.guard import cmd_check

        cmd_check(args)


if __name__ == "__main__":
    main()
