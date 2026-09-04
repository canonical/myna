"""Myna benchmarker — community testing tool.

Collect STT accuracy and latency results across the Myna inference snaps on
your own machine and share the results file with the project team.

Quick start:

    # 1. Download an evaluation corpus (~330 MB, one-time)
    python3 myna-bench.pyz download-corpus --out ./corpus

    # 2. Edit bench.yaml to list your snap files (see bench.yaml.example)

    # 3. Run the sweep (requires root — installs/removes snaps)
    sudo python3 myna-bench.pyz run --config bench.yaml

    # 4. Inspect locally
    python3 myna-bench.pyz summarize --in results.jsonl

    # 5. Share results.jsonl with the team
"""

from __future__ import annotations

import argparse


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="myna-bench",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = parser.add_subparsers(dest="command", metavar="command")
    sub.required = True

    # ------------------------------------------------------------------
    # run
    # ------------------------------------------------------------------
    p_run = sub.add_parser(
        "run",
        help="run the full snap sweep and write a results file",
        description=(
            "Install each snap in bench.yaml, sweep the corpus, write results.jsonl.\n"
            "Requires root (snap install/remove). Run with sudo.\n\n"
            "The output file starts with a machine-summary record followed by\n"
            "one bench record per clip × snap × model × mode combination."
        ),
    )
    p_run.add_argument(
        "--config",
        default="bench.yaml",
        help="benchmark config YAML (default: bench.yaml)",
    )
    p_run.add_argument(
        "--out",
        default=None,
        help="output JSONL path (overrides config; default: results.jsonl)",
    )
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
        "--budget",
        type=float,
        default=None,
        help="warm-sweep wall-clock budget in seconds (overrides config)",
    )

    # ------------------------------------------------------------------
    # download-corpus
    # ------------------------------------------------------------------
    p_dl = sub.add_parser(
        "download-corpus",
        help="download a LibriSpeech evaluation corpus",
        description=(
            "Download a balanced clip set from LibriSpeech (CC-BY-4.0) and write\n"
            "a manifest.json ready for bench.yaml. Requires ffmpeg for FLAC decode."
        ),
    )
    p_dl.add_argument(
        "--out",
        default="corpus",
        help="output directory (default: corpus)",
    )
    p_dl.add_argument(
        "--subset",
        choices=("dev-clean", "dev-other", "test-clean", "test-other"),
        default="dev-clean",
        help="LibriSpeech split (default: dev-clean; -other splits are harder/accented)",
    )
    p_dl.add_argument(
        "-n",
        type=int,
        default=20,
        help="number of clips to select (default: 20, spread across speakers)",
    )
    p_dl.add_argument(
        "--cache",
        default=".cache/librispeech",
        help="cache dir for downloaded tarballs (default: .cache/librispeech)",
    )

    # ------------------------------------------------------------------
    # make-corpus
    # ------------------------------------------------------------------
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
    p_mk.add_argument(
        "--dir",
        required=True,
        help="directory containing *.wav files",
    )
    p_mk.add_argument(
        "--out",
        default=None,
        help="output directory for manifest.json (default: same as --dir)",
    )
    p_mk.add_argument(
        "--language",
        default="en",
        help="BCP-47 language code for all clips (default: en)",
    )
    p_mk.add_argument(
        "--category",
        default="quiet",
        help="default UD129 category (default: quiet; override per-clip with <stem>.category)",
    )

    # ------------------------------------------------------------------
    # summarize
    # ------------------------------------------------------------------
    p_sum = sub.add_parser(
        "summarize",
        help="print an aggregate WER/latency table from a results file",
        description="Read a results.jsonl and print a comparison table across all labels.",
    )
    p_sum.add_argument(
        "--in",
        dest="infile",
        default="results.jsonl",
        help="results JSONL to read (default: results.jsonl)",
    )
    p_sum.add_argument(
        "--by-category",
        action="store_true",
        help="also break WER down by UD129 category",
    )

    args = parser.parse_args()

    if args.command == "run":
        from myna.benchmarker._run import cmd_run

        cmd_run(args)
    elif args.command == "download-corpus":
        from myna.benchmarker._corpus import cmd_download

        cmd_download(args)
    elif args.command == "make-corpus":
        from myna.benchmarker._corpus import cmd_make

        cmd_make(args)
    elif args.command == "summarize":
        from myna.benchmarker._summarize import cmd_summarize

        cmd_summarize(args)


if __name__ == "__main__":
    main()
