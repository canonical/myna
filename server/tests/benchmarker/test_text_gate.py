"""Per-plugin gate config, plugin lookup and the socket system."""

from __future__ import annotations

import json
import socket
import threading
from pathlib import Path

import pytest

from myna.benchmarker.text_gate import (
    GateConfig,
    GateError,
    _call,
    find_plugin,
    load_plugin,
    pair_of,
    socket_system,
)

REPO = Path(__file__).resolve().parents[3]


def test_defaults_are_the_decided_gate():
    g = GateConfig()
    assert (g.comet_min, g.chrf_floor, g.chrf_ratio, g.p95_max_ms) == (0.80, 0.40, 0.85, 400.0)
    assert (g.empty_max, g.repeat_max, g.ratio_min, g.ratio_max, g.copy_max) == (
        0.0,
        0.01,
        0.5,
        2.0,
        0.05,
    )
    assert g.chrf_limit() == 0.40 and not g.reverse


def test_chrf_limit_prefers_the_explicit_minimum_then_the_vendor_ratio():
    assert GateConfig(chrf_min=0.58, vendor_chrf=0.9).chrf_limit() == 0.58
    assert GateConfig(vendor_chrf=0.60).chrf_limit() == pytest.approx(0.51)
    assert GateConfig(chrf_floor=0.3).chrf_limit() == 0.3


def test_from_plugin_reads_the_gate_table_and_the_direction():
    plugin = {
        "language": "de",
        "to": "en",
        "gate": {"vendor_comet": 0.9, "p95_max_ms": 300, "spot_check_waiver": "x"},
    }
    g = GateConfig.from_plugin(plugin)
    assert g.vendor_comet == 0.9 and g.p95_max_ms == 300.0 and g.reverse
    assert not GateConfig.from_plugin({"language": "en", "to": "de"}).reverse
    assert not GateConfig.from_plugin({"language": "en", "to": "en"}).reverse
    assert GateConfig.from_plugin({}) == GateConfig()


@pytest.mark.parametrize(
    ("gate", "message"),
    [
        ({"vendor_comat": 0.9}, "unknown gate keys: vendor_comat"),
        ({"vendor_comet": "high"}, "must be a number"),
        ({"vendor_comet": True}, "must be a number"),
        ({"spot_check_waiver": " "}, "^gate.spot_check_waiver must say why$"),
        ({"b_key": 1, "a_key": 2}, "^unknown gate keys: a_key, b_key$"),
        ({"reverse": 1}, "unknown gate keys: reverse"),
    ],
)
def test_a_bad_gate_table_is_refused(gate, message):
    with pytest.raises(GateError, match=message):
        GateConfig.from_plugin({"gate": gate})


def test_pair_of_names_the_foreign_language_and_refuses_other_shapes():
    def plugin(src, dst, task="text.translate"):
        return {"id": "p", "task": task, "language": src, "to": dst}

    assert pair_of(plugin("en", "de")) == "de"
    assert pair_of(plugin("de", "en")) == "de"
    with pytest.raises(GateError, match="English to X"):
        pair_of(plugin("fr", "de"))
    with pytest.raises(GateError, match="English to X"):
        pair_of(plugin("en", "en"))
    with pytest.raises(GateError, match="^p: not a translation"):
        pair_of(plugin("en", "de", "text.cleanup"))


@pytest.mark.repo_tree
def test_find_plugin_by_id_stem_or_path_and_load_it(tmp_path):
    found = find_plugin(REPO, "bergamot-base-memory-en-fr")
    assert found.name == "translate-en-fr.toml"
    assert find_plugin(REPO, "translate-en-fr") == found
    assert find_plugin(REPO, str(found)) == found
    plugin = load_plugin(found)
    assert plugin["name"] == "translate-en-fr" and plugin["path"] == str(found)
    with pytest.raises(GateError, match="no plugin 'nope'"):
        find_plugin(REPO, "nope")


class Provider:
    """A myna-text-v1 server on a Unix socket, one request per connection."""

    def __init__(self, path, models, replies=None):
        self.models, self.replies, self.seen = models, replies or {}, []
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.bind(str(path))
        self.sock.listen()
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            with conn:
                request = json.loads(conn.makefile("rb").readline())
                self.seen.append(request)
                reply = self.replies.get(request["op"])
                if request["op"] == "capabilities":
                    reply = {"protocol": "myna-text-v1"}
                    if self.models is not None:
                        reply["models"] = self.models
                elif callable(reply):
                    reply = reply(request)
                conn.sendall(json.dumps(reply or {}).encode() + b"\n")


def _model(task="text.translate"):
    return {"id": "m", "task": task, "covers": {"language": ["en"], "to": ["de"]}}


def test_socket_system_warms_then_translates_with_the_models_params(tmp_path):
    path = tmp_path / "p.sock"
    provider = Provider(
        path, [_model()], {"warm": {"ok": True}, "run": lambda r: {"text": r["text"].upper()}}
    )
    system = socket_system(str(path), "m")
    assert system.name == "socket:m" and system.tidy is None
    assert system.translate("hallo") == "HALLO"
    warm, run = provider.seen[1], provider.seen[2]
    assert warm == {
        "op": "warm",
        "model": "m",
        "task": "text.translate",
        "params": {"language": "en", "to": "de"},
    }
    assert run["params"] == {"language": "en", "to": "de"} and run["deadline_ms"] == 5000


def test_socket_system_for_a_cleanup_model_offers_tidy(tmp_path):
    path = tmp_path / "p.sock"
    Provider(path, [_model("text.cleanup")], {"warm": {}, "run": lambda r: {"text": r["text"]}})
    system = socket_system(str(path), "m")
    assert system.translate is None and system.tidy("x") == "x"
    assert system.name == "socket:m"


def test_socket_system_refuses_what_the_provider_does_not_offer_or_will_not_do(tmp_path):
    path = tmp_path / "a.sock"
    Provider(path, [_model()], {"warm": {"error": "no memory"}})
    with pytest.raises(GateError, match="does not offer model 'other'"):
        socket_system(str(path), "other")
    with pytest.raises(GateError, match="warm refused: no memory"):
        socket_system(str(path), "m")
    path2 = tmp_path / "b.sock"
    Provider(path2, [_model()], {"warm": {}, "run": {"error": "deadline"}})
    system = socket_system(str(path2), "m")
    with pytest.raises(GateError, match="run refused: deadline"):
        system.translate("x")
    path4 = tmp_path / "d.sock"
    Provider(path4, [_model()], {"warm": {}, "run": {"nope": 1}})
    with pytest.raises(GateError, match=r"run refused: \{'nope': 1\}"):
        socket_system(str(path4), "m").translate("x")
    path5 = tmp_path / "e.sock"
    Provider(path5, None)
    with pytest.raises(GateError, match="does not offer model 'm'"):
        socket_system(str(path5), "m")
    path3 = tmp_path / "c.sock"
    Provider(path3, [_model("text.rewrite")], {"warm": {}})
    with pytest.raises(GateError, match="no text suite"):
        socket_system(str(path3), "m")


def test_a_provider_that_never_answers_times_out(tmp_path):
    path = tmp_path / "silent.sock"
    server = socket.socket(socket.AF_UNIX)
    server.bind(str(path))
    server.listen()
    with pytest.raises(TimeoutError):
        _call(str(path), {"op": "capabilities"}, timeout=0.2)
    server.close()


def test_find_plugin_searches_every_snap_directory_of_a_tree(tmp_path):
    for snap, stem, ident in (("a-snap", "one", "id-one"), ("b-snap", "two", "id-two")):
        (tmp_path / snap / "plugins").mkdir(parents=True)
        (tmp_path / snap / "plugins" / f"{stem}.toml").write_text(f'id = "{ident}"\n', encoding="utf-8")
    (tmp_path / "c-snap" / "plugins").mkdir(parents=True)
    (tmp_path / "c-snap" / "plugins" / "bad.toml").write_text('id = "id-bad"\n', encoding="utf-8")
    assert find_plugin(tmp_path, "id-two") == tmp_path / "b-snap/plugins/two.toml"
    assert find_plugin(tmp_path, "one") == tmp_path / "a-snap/plugins/one.toml"
    direct = tmp_path / "a-snap/plugins/one.toml"
    assert find_plugin(tmp_path / "nowhere", str(direct)) == direct
    with pytest.raises(GateError, match="no plugin 'zzz'"):
        find_plugin(tmp_path, "zzz")
    with pytest.raises(GateError, match="no plugin 'ghost.toml'"):
        find_plugin(tmp_path, "ghost.toml")
    plugin = load_plugin(direct)
    assert plugin == {"id": "id-one", "name": "one", "path": str(direct)}
