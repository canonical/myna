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
        ("myna.benchmarker._run", "cmd_plan", "plan"),
        ("myna.benchmarker._bench", "cmd_bench", "bench"),
        ("myna.benchmarker.corpus_english", "cmd_download", "download-corpus"),
        ("myna.benchmarker.corpus_chinese", "cmd_download_zh", "download-corpus-zh"),
        ("myna.benchmarker._corpus", "cmd_make", "make-corpus"),
        ("myna.benchmarker._summarize", "cmd_summarize", "summarize"),
        ("myna.benchmarker.guard", "cmd_check", "check"),
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


def test_plan_dispatches_to_the_planner(monkeypatch, dispatched):
    run_cli(monkeypatch, "plan")
    assert dispatched["command"] == "plan"


def test_bench_dispatches_to_the_clip_scorer(monkeypatch, dispatched):
    run_cli(monkeypatch, "bench", "--socket", "/tmp/s", "--manifest", "m.json", "--label", "x")
    assert dispatched["command"] == "bench"


def test_check_dispatches_to_the_environment_guard(monkeypatch, dispatched):
    run_cli(monkeypatch, "check")
    assert dispatched["command"] == "check"


def test_download_zh_dispatches_to_the_fleurs_builder(monkeypatch, dispatched):
    run_cli(monkeypatch, "download-corpus-zh")
    assert dispatched["command"] == "download-corpus-zh"


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
    assert args.skip_env_check is False
    assert args.only is None
    assert args.label_suffix == ""


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
        "--only",
        "myna-whisper",
        "--only",
        "myna-parakeet",
        "--label-suffix",
        "maxstack",
        "--skip-env-check",
    )
    args = dispatched["args"]
    assert args.config == "custom.yaml"
    assert args.out == "custom.jsonl"
    assert args.keep_results is True
    assert args.no_resources is True
    assert args.budget == 45.0
    assert args.only == ["myna-whisper", "myna-parakeet"]
    assert args.label_suffix == "maxstack"
    assert args.skip_env_check is True


def test_plan_takes_the_same_target_selection_as_run(monkeypatch, dispatched):
    """A plan that could not be narrowed the way the run is would describe a
    different sweep from the one about to happen."""
    run_cli(monkeypatch, "plan", "--config", "c.yaml", "--only", "myna-sherpa", "--budget", "10")
    args = dispatched["args"]
    assert (args.config, args.only, args.budget) == ("c.yaml", ["myna-sherpa"], 10.0)


# ─── download-corpus ─────────────────────────────────────────────────────────


def test_download_defaults_to_an_archive_ordered_dev_clean_tier(monkeypatch, dispatched):
    run_cli(monkeypatch, "download-corpus")
    args = dispatched["args"]
    assert args.out == "corpus/english"
    assert args.subset == "dev-clean"
    assert args.n == 12
    assert args.cache == ".cache/librispeech"
    assert args.select == "archive"
    assert args.manifest_name == "manifest.json"
    assert args.long_form_minutes is None
    assert args.skip_complete is False


def test_download_accepts_the_balanced_long_form_tier_the_sweep_uses(monkeypatch, dispatched):
    run_cli(
        monkeypatch,
        "download-corpus",
        "--select",
        "balanced",
        "-n",
        "80",
        "--manifest-name",
        "manifest-balanced.json",
        "--long-form-minutes",
        "5",
    )
    args = dispatched["args"]
    assert (args.select, args.n) == ("balanced", 80)
    assert args.manifest_name == "manifest-balanced.json"
    assert args.long_form_minutes == 5.0


def test_download_rejects_a_selection_strategy_that_does_not_exist(monkeypatch, dispatched):
    with pytest.raises(SystemExit):
        run_cli(monkeypatch, "download-corpus", "--select", "random")


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


def test_summarize_defaults_to_ranking_by_wer_across_one_corpus(monkeypatch, dispatched):
    run_cli(monkeypatch, "summarize")
    assert dispatched["args"].sort == "wer"
    assert dispatched["args"].corpus is None


def test_summarize_rejects_a_sort_key_with_no_column(monkeypatch, dispatched):
    with pytest.raises(SystemExit):
        run_cli(monkeypatch, "summarize", "--sort", "vibes")


# ─── bench ───────────────────────────────────────────────────────────────────


def test_bench_requires_a_socket_manifest_and_label(monkeypatch, dispatched):
    """Nothing here can be guessed: the socket does not say which engine or
    model served the request, so an unlabelled row is an unattributable one."""
    with pytest.raises(SystemExit):
        run_cli(monkeypatch, "bench", "--socket", "/tmp/s")


def test_bench_defaults_to_a_warm_batch_fed_sweep(monkeypatch, dispatched):
    run_cli(monkeypatch, "bench", "--socket", "/tmp/s", "--manifest", "m.json", "--label", "x")
    args = dispatched["args"]
    assert args.streaming is False
    assert args.cold is False
    assert args.realtime is False
    assert args.clip == []
    assert args.out == "results.jsonl"


# ─── check ───────────────────────────────────────────────────────────────────


def test_check_defaults_to_the_full_in_process_profile(monkeypatch, dispatched):
    run_cli(monkeypatch, "check")
    args = dispatched["args"]
    assert args.model == "parakeet"
    assert args.sweep is False
    assert args.force is False


def test_check_sweep_narrows_to_the_snap_relevant_subset(monkeypatch, dispatched):
    run_cli(monkeypatch, "check", "--sweep", "--model", "whisper")
    args = dispatched["args"]
    assert args.sweep is True
    assert args.model == "whisper"
