"""myna-text-v1 as the Rust client speaks it (client/myna-desktop/src/process/wire.rs)."""

from __future__ import annotations

import logging
import re
import time
from pathlib import Path

import pytest
from conftest import EchoModel

from myna.text.protocol import PROTOCOL, Provider

WIRE_RS = Path(__file__).resolve().parents[3] / "client/myna-desktop/src/process/wire.rs"


def provider(*models: EchoModel) -> Provider:
    return Provider(lambda: list(models))


def test_capabilities_name_the_protocol_and_every_field_the_client_reads():
    reply = provider(EchoModel()).handle({"op": "capabilities"})

    assert reply == {
        "protocol": "myna-text-v1",
        "models": [
            {
                "id": "echo",
                "task": "text.cleanup",
                "covers": {"language": ["en"]},
                "accepts": ["instruction"],
                "latency_ms": 5,
                "quality": 0.5,
            }
        ],
    }
    assert PROTOCOL == "myna-text-v1"


def test_warm_loads_the_named_model_and_run_answers_with_text():
    model = EchoModel()
    p = provider(model)
    p.handle({"op": "capabilities"})

    assert p.handle({"op": "warm", "model": "echo", "task": "text.cleanup", "params": {}}) == {
        "ok": True
    }
    assert model.warmed == 1
    reply = p.handle(
        {
            "op": "run",
            "model": "echo",
            "task": "text.cleanup",
            "params": {"language": "en"},
            "text": "um hi",
            "deadline_ms": 250,
        }
    )
    assert reply == {"text": "UM HI"}


@pytest.mark.parametrize(
    ("call", "error"),
    [
        ({"op": "nonsense"}, "unknown op"),
        ({}, "unknown op"),
        ([1, 2], "unknown op"),
        ({"op": "run", "model": "x", "task": "text.cleanup", "text": "a"}, "no such model"),
        ({"op": "run", "model": "echo", "task": "text.translate", "text": "a"}, "no such model"),
        (
            {"op": "warm", "model": "echo", "task": "text.cleanup", "params": {"language": "fr"}},
            "not covered",
        ),
        (
            {
                "op": "run",
                "model": "echo",
                "task": "text.cleanup",
                "params": {"style": "x"},
                "text": "a",
            },
            "not covered",
        ),
        (
            {"op": "run", "model": "echo", "task": "text.cleanup", "params": "en", "text": "a"},
            "not covered",
        ),
        ({"op": "run", "model": "echo", "task": "text.cleanup", "params": {}}, "no text"),
        (
            {"op": "run", "model": "echo", "task": "text.cleanup", "params": {}, "text": 3},
            "no text",
        ),
        (
            {"op": "run", "model": "echo", "task": "text.cleanup", "params": {}, "text": "refuse"},
            "nope",
        ),
    ],
)
def test_a_call_that_cannot_be_served_is_refused_with_a_reason(call, error):
    assert provider(EchoModel()).handle(call) == {"error": error}


def test_an_unexpected_failure_is_a_bare_error_that_never_quotes_the_text():
    reply = provider(EchoModel()).handle(
        {"op": "run", "model": "echo", "task": "text.cleanup", "params": {}, "text": "crash"}
    )

    assert reply == {"error": "RuntimeError"}


def test_free_form_params_are_taken_whatever_they_say():
    reply = provider(EchoModel()).handle(
        {
            "op": "run",
            "model": "echo",
            "task": "text.cleanup",
            "params": {"language": "en", "instruction": "be brief"},
            "text": "a",
        }
    )

    assert reply == {"text": "A"}


def test_a_model_can_be_run_without_listing_capabilities_first():
    reply = provider(EchoModel()).handle(
        {"op": "run", "model": "echo", "task": "text.cleanup", "params": {}, "text": "a"}
    )

    assert reply == {"text": "A"}


def test_capabilities_pick_up_a_model_installed_later_and_keep_the_warm_ones():
    later: list[EchoModel] = []
    p = Provider(lambda: [EchoModel("first"), *later])
    p.handle({"op": "capabilities"})
    p.handle({"op": "warm", "model": "first", "task": "text.cleanup"})
    later.append(EchoModel("late", "text.translate"))

    ids = [m["id"] for m in p.handle({"op": "capabilities"})["models"]]

    assert ids == ["first", "late"]
    # The object that was warmed is still the one held.
    assert p._models[("first", "text.cleanup")].warmed == 1


def test_a_removed_model_is_closed_and_no_longer_listed():
    model = EchoModel()
    present = [model]
    p = Provider(lambda: list(present))
    p.handle({"op": "capabilities"})
    present.clear()

    assert p.handle({"op": "capabilities"})["models"] == []
    assert model.closed == 1


def test_close_closes_every_model():
    model = EchoModel()
    p = provider(model)
    p.handle({"op": "capabilities"})

    p.close()

    assert model.closed == 1
    assert p.handle({"op": "capabilities"})["models"][0]["id"] == "echo"


@pytest.mark.parametrize(
    ("task", "params", "covered"),
    [
        # Mirrors wire.rs `a_model_covers_only_the_requests_it_lists`.
        ("text.cleanup", {}, True),
        ("text.cleanup", {"language": "en"}, True),
        ("text.cleanup", {"language": "fr"}, False),
        ("text.translate", {}, False),
        ("text.cleanup", {"language": 7}, False),
        ("text.cleanup", {"style": "x"}, False),
        ("text.cleanup", {"instruction": "be brief"}, True),
    ],
)
def test_coverage_follows_the_rule_of_the_client(task, params, covered):
    assert EchoModel().covers_request(task, params) is covered


def _rust_struct_fields(source: str, name: str) -> list[str]:
    body = re.search(rf"pub struct {name} \{{(.*?)\n\}}", source, re.S)
    assert body, f"{name} is gone from wire.rs"
    return re.findall(r"^\s+pub (\w+):", body.group(1), re.M)


@pytest.mark.repo_tree
def test_capabilities_carry_exactly_the_fields_the_client_deserialises():
    source = WIRE_RS.read_text(encoding="utf-8")
    capability = EchoModel().capability()

    assert sorted(capability) == sorted(_rust_struct_fields(source, "Capability"))
    assert sorted(provider(EchoModel()).handle({"op": "capabilities"})) == sorted(
        _rust_struct_fields(source, "Capabilities")
    )
    assert re.search(r'caps\.protocol == "myna-text-v1"', source)


@pytest.mark.repo_tree
def test_the_ops_and_fields_the_client_sends_are_the_ones_served():
    source = WIRE_RS.read_text(encoding="utf-8")
    call = re.search(r"pub enum Call<'a> \{(.*?)\n\}", source, re.S)
    assert call
    sent = {
        name.lower(): fields
        for name, fields in re.findall(r"(\w+)(?: \{([^}]*)\})?,", call.group(1))
        if name[0].isupper()
    }
    assert set(sent) == {"capabilities", "warm", "run"}
    # The daemon sends these, and the provider reads these.
    assert re.findall(r"(\w+): &", sent["run"]) == ["model", "task", "params", "text"]
    assert "deadline_ms" in call.group(1)
    assert re.findall(r"(\w+): &", sent["warm"]) == ["model", "task", "params"]


def _call(op, **extra):
    return {"op": op, "model": "echo", "task": "text.cleanup", "params": {}, **extra}


@pytest.mark.parametrize(
    ("extra", "warm_s", "run_s"),
    [
        ({"deadline_ms": 250}, 0.25, 0.25),
        ({}, 30.0, 10.0),
        ({"deadline_ms": 0}, 30.0, 10.0),
        ({"deadline_ms": -5}, 30.0, 10.0),
        ({"deadline_ms": True}, 30.0, 10.0),
        ({"deadline_ms": "250"}, 30.0, 10.0),
        ({"deadline_ms": 1}, 0.001, 0.001),
    ],
)
def test_the_model_gets_the_deadline_the_daemon_set_in_seconds(extra, warm_s, run_s):
    model = EchoModel()
    p = provider(model)

    p.handle(_call("warm", **extra))
    p.handle(_call("run", text="a", **extra))

    assert model.deadlines == [("warm", warm_s), ("run", run_s)]


def test_a_known_model_is_served_without_looking_for_components_again():
    looked = []

    def find():
        looked.append(1)
        return [EchoModel()]

    p = Provider(find)
    p.handle({"op": "capabilities"})
    p.handle(_call("warm"))
    p.handle(_call("run", text="a"))

    assert len(looked) == 1


def test_an_unknown_model_makes_it_look_once_more():
    looked = []
    p = Provider(lambda: looked.append(1) or [])

    assert p.handle(_call("run", text="a")) == {"error": "no such model"}
    assert len(looked) == 1


def test_calls_are_logged_with_the_model_and_outcome_but_never_the_text(caplog):
    p = provider(EchoModel())
    with caplog.at_level(logging.INFO, logger="myna.text"):
        p.handle(_call("warm"))
        p.handle(_call("run", text="my secret words"))
        p.handle(_call("run", text="refuse"))
        p.handle(_call("run", text="crash"))
        p.handle([1, 2])

    messages = [r.getMessage() for r in caplog.records]
    assert [m.rsplit(" in ", 1)[0] for m in messages if " -> " in m] == [
        "warm echo -> ok",
        "run echo -> text",
        "run echo -> nope",
        "run echo -> RuntimeError",
        "None - -> unknown op",
    ]
    assert all(re.fullmatch(r".* in \d+ ms|run failed", m) for m in messages)
    assert not any("secret" in m or "refuse" in m.split("->")[0] for m in messages)
    assert any(r.exc_info for r in caplog.records)
    assert "run failed" in messages


def test_the_logged_duration_is_in_milliseconds(caplog):
    class Slow(EchoModel):
        def run(self, text, deadline_s):
            time.sleep(0.06)
            return text

    with caplog.at_level(logging.INFO, logger="myna.text"):
        provider(Slow()).handle(_call("run", text="a"))

    took = int(re.search(r" in (\d+) ms", caplog.messages[-1]).group(1))
    assert 55 <= took < 1000
