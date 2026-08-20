"""`myna-bench` argument parsing and subcommand dispatch.

The zipapp is the only interface community testers touch, and its subcommand
handlers are imported lazily so a `summarize` on a laptop never needs the run
path's dependencies. These tests pin the parser surface (defaults, required
arguments, choices) and that each subcommand reaches its handler with the
arguments that handler reads.
"""

from __future__ import annotations

import pytest

from myna.benchmarker.__main__ import main


@pytest.fixture
def dispatched(monkeypatch):
    """Capture which handler ran, with what args, without running it."""
    seen: dict[str, object] = {}

    def capture(name):
        def handler(args):
            seen["command"] = name
            seen["args"] = args

        return handler

    for module, attr, name in [
        ("myna.benchmarker._run", "cmd_run", "run"),
        ("myna.benchmarker._corpus", "cmd_download", "download-corpus"),
        ("myna.benchmarker._corpus", "cmd_make", "make-corpus"),
        ("myna.benchmarker._summarize", "cmd_summarize", "summarize"),
    ]:
        monkeypatch.setattr(f"{module}.{attr}", capture(name))
    return seen


def run_cli(monkeypatch, *argv):
    monkeypatch.setattr("sys.argv", ["myna-bench", *argv])
    main()


# ─── dispatch ────────────────────────────────────────────────────────────────


def test_run_dispatches_to_the_sweep_runner(monkeypatch, dispatched):
    run_cli(monkeypatch, "run")
    assert dispatched["command"] == "run"


def test_download_corpus_dispatches_to_the_downloader(monkeypatch, dispatched):
    run_cli(monkeypatch, "download-corpus")
    assert dispatched["command"] == "download-corpus"


def test_make_corpus_dispatches_to_the_manifest_builder(monkeypatch, dispatched):
    run_cli(monkeypatch, "make-corpus", "--dir", "clips")
    assert dispatched["command"] == "make-corpus"


def test_summarize_dispatches_to_the_aggregator(monkeypatch, dispatched):
    run_cli(monkeypatch, "summarize")
    assert dispatched["command"] == "summarize"


def test_a_command_is_required(monkeypatch, dispatched):
    with pytest.raises(SystemExit):
        run_cli(monkeypatch)


def test_an_unknown_command_is_rejected(monkeypatch, dispatched):
    with pytest.raises(SystemExit):
        run_cli(monkeypatch, "frobnicate")


# ─── run ─────────────────────────────────────────────────────────────────────


def test_run_defaults_to_bench_yaml_in_the_working_directory(monkeypatch, dispatched):
    run_cli(monkeypatch, "run")
    args = dispatched["args"]
    assert args.config == "bench.yaml"
    assert args.out is None
    assert args.budget is None
    assert args.keep_results is False
    assert args.no_resources is False


def test_run_accepts_every_sweep_override(monkeypatch, dispatched):
    run_cli(
        monkeypatch,
        "run",
        "--config",
        "custom.yaml",
        "--out",
        "custom.jsonl",
        "--keep-results",
        "--no-resources",
        "--budget",
        "45",
    )
    args = dispatched["args"]
    assert args.config == "custom.yaml"
    assert args.out == "custom.jsonl"
    assert args.keep_results is True
    assert args.no_resources is True
    assert args.budget == 45.0


# ─── download-corpus ─────────────────────────────────────────────────────────


def test_download_defaults_to_twenty_dev_clean_clips(monkeypatch, dispatched):
    run_cli(monkeypatch, "download-corpus")
    args = dispatched["args"]
    assert args.out == "corpus"
    assert args.subset == "dev-clean"
    assert args.n == 20
    assert args.cache == ".cache/librispeech"


def test_download_accepts_the_other_librispeech_splits(monkeypatch, dispatched):
    run_cli(monkeypatch, "download-corpus", "--subset", "test-other", "-n", "5")
    assert dispatched["args"].subset == "test-other"
    assert dispatched["args"].n == 5


def test_download_rejects_a_split_that_does_not_exist(monkeypatch, dispatched):
    with pytest.raises(SystemExit):
        run_cli(monkeypatch, "download-corpus", "--subset", "train-clean-360")


# ─── make-corpus ─────────────────────────────────────────────────────────────


def test_make_requires_a_source_directory(monkeypatch, dispatched):
    with pytest.raises(SystemExit):
        run_cli(monkeypatch, "make-corpus")


def test_make_defaults_to_english_quiet_clips_in_place(monkeypatch, dispatched):
    run_cli(monkeypatch, "make-corpus", "--dir", "clips")
    args = dispatched["args"]
    assert args.dir == "clips"
    assert args.out is None
    assert args.language == "en"
    assert args.category == "quiet"


def test_make_accepts_a_language_and_category_override(monkeypatch, dispatched):
    run_cli(
        monkeypatch,
        "make-corpus",
        "--dir",
        "clips",
        "--out",
        "built",
        "--language",
        "de",
        "--category",
        "noise",
    )
    args = dispatched["args"]
    assert (args.out, args.language, args.category) == ("built", "de", "noise")


# ─── summarize ───────────────────────────────────────────────────────────────


def test_summarize_defaults_to_results_jsonl_without_the_category_table(monkeypatch, dispatched):
    run_cli(monkeypatch, "summarize")
    assert dispatched["args"].infile == "results.jsonl"
    assert dispatched["args"].by_category is False


def test_summarize_reads_the_results_file_from_in(monkeypatch, dispatched):
    run_cli(monkeypatch, "summarize", "--in", "other.jsonl", "--by-category")
    assert dispatched["args"].infile == "other.jsonl"
    assert dispatched["args"].by_category is True
