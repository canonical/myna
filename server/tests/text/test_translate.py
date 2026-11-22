"""The Bergamot worker, against a real child process that stands in for it."""

from __future__ import annotations

import sys
import time

import pytest
from conftest import FAKE_BERGAMOT, TRANSLATE_META

from myna.text.protocol import Refused
from myna.text.translate import TranslateModel, Worker


@pytest.fixture
def worker(fake_argv):
    w = Worker(fake_argv)
    yield w
    w.close()


def test_a_worker_answers_one_line_per_line(worker):
    assert not worker.alive
    worker.start(10)

    assert worker.alive
    assert worker.translate("bonjour", 5) == "BONJOUR"
    assert worker.translate("deux  mots\nsur deux lignes", 5) == "DEUX MOTS SUR DEUX LIGNES"
    assert worker.translate("", 5) == ""


def test_a_late_answer_is_a_timeout_and_the_worker_is_dropped(worker):
    worker.start(10)

    started = time.monotonic()
    with pytest.raises(TimeoutError):
        worker.translate("SLEEP", 0.2)

    assert time.monotonic() - started < 1.5
    assert not worker.alive


def test_a_worker_that_dies_mid_call_is_a_broken_pipe_or_eof(worker):
    worker.start(10)

    with pytest.raises((ConnectionError, OSError)):
        worker.translate("DIE", 5)

    assert not worker.alive


def test_a_worker_that_never_says_ready_times_out_on_start(fake_argv):
    w = Worker([*fake_argv, "NOREADY"])

    with pytest.raises(TimeoutError):
        w.start(0.3)

    assert not w.alive


def test_a_worker_that_exits_before_ready_fails_to_start():
    w = Worker([sys.executable, "-c", "pass"])

    with pytest.raises(ConnectionError):
        w.start(5)


def test_a_worker_that_says_something_else_first_fails_to_start():
    w = Worker([sys.executable, "-c", "print('hello', flush=True); input()"])

    with pytest.raises(ConnectionError, match="ready"):
        w.start(5)
    assert not w.alive


def test_start_replaces_a_running_worker(worker):
    worker.start(10)
    first = worker._proc

    worker.start(10)

    assert worker._proc is not first
    assert first.poll() is not None


def test_closing_twice_is_harmless(worker):
    worker.start(10)
    worker.close()
    worker.close()


def model(tmp_path, fake_argv, **meta):
    return TranslateModel(
        tmp_path,
        {**TRANSLATE_META, **meta},
        FAKE_BERGAMOT,
        spawn=lambda argv: __import__("subprocess").Popen(
            fake_argv, stdin=-1, stdout=-1, stderr=-3, bufsize=0
        ),
    )


def test_the_model_covers_one_language_pair_and_declares_its_measured_latency(tmp_path, fake_argv):
    m = model(tmp_path, fake_argv)

    assert m.capability() == {
        "id": "bergamot-base-memory-en-fr",
        "task": "text.translate",
        "covers": {"language": ["en"], "to": ["fr"]},
        "accepts": [],
        "latency_ms": 20,
        "quality": 0.7,
    }
    assert m.covers_request("text.translate", {"language": "en", "to": "fr"})
    assert not m.covers_request("text.translate", {"language": "fr", "to": "en"})


def test_warm_starts_the_worker_and_run_translates_without_restarting_it(tmp_path, fake_argv):
    m = model(tmp_path, fake_argv)
    try:
        m.warm(10)
        pid = m._worker._proc.pid

        assert m.run("hello there", 5) == "HELLO THERE"
        assert m._worker._proc.pid == pid
    finally:
        m.close()


def test_run_starts_the_worker_when_nothing_warmed_it(tmp_path, fake_argv):
    m = model(tmp_path, fake_argv)
    try:
        assert m.run("hi", 10) == "HI"
    finally:
        m.close()


def test_blank_text_is_returned_as_it_came(tmp_path, fake_argv):
    m = model(tmp_path, fake_argv)

    assert m.run("  ", 5) == "  "
    assert not m._worker.alive


def test_a_slow_translation_is_refused_and_the_next_one_restarts_the_worker(tmp_path, fake_argv):
    m = model(tmp_path, fake_argv)
    try:
        m.warm(10)
        with pytest.raises(Refused, match="timed out"):
            m.run("SLEEP", 0.2)

        assert m.run("again", 10) == "AGAIN"
    finally:
        m.close()


def test_a_worker_that_dies_is_a_refusal_not_a_crash(tmp_path, fake_argv):
    m = model(tmp_path, fake_argv)
    try:
        m.warm(10)
        with pytest.raises(Refused, match="failed"):
            m.run("DIE", 5)
    finally:
        m.close()


def test_a_worker_that_will_not_start_is_a_refusal(tmp_path):
    def broken(argv):
        raise FileNotFoundError("myna-bergamot")

    m = TranslateModel(tmp_path, TRANSLATE_META, FAKE_BERGAMOT, spawn=broken)

    with pytest.raises(Refused, match="did not start"):
        m.warm(1)


def test_the_real_spawn_runs_the_binary_with_the_config_path(tmp_path):
    script = tmp_path / "binary"
    script.write_text(
        f"#!{sys.executable}\nimport sys\nprint('ready', flush=True)\n"
        "for line in sys.stdin:\n    print(' '.join(sys.argv[1:]), flush=True)\n"
    )
    script.chmod(0o755)
    m = TranslateModel(tmp_path, TRANSLATE_META, script)
    try:
        assert m.run("x", 10) == f"--model-config-paths {tmp_path / 'config.yml'}"
    finally:
        m.close()
