"""The myna-text daemon on a real Unix socket."""

from __future__ import annotations

import json
import os
import shutil
import socket
import stat
import sys
import threading
from pathlib import Path

import pytest
from conftest import TAGGER_META, TRANSLATE_META, EchoModel, write_component

from myna.text import cli, daemon
from myna.text.protocol import Provider
from myna.text.server import MAX_LINE, Server
from myna.text.tagger import KINDS as TAGGER_KINDS
from myna.translation import cli as translation_cli
from myna.translation.bergamot import kinds as bergamot_kinds


def connect(path: Path) -> socket.socket:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(5)
    s.connect(os.fspath(path))
    return s


def ask(sock: socket.socket, call: object) -> dict:
    sock.sendall(json.dumps(call).encode() + b"\n")
    return json.loads(sock.makefile("rb").readline())


@pytest.fixture
def served(tmp_path):
    path = tmp_path / "share" / "text.sock"
    server = Server(path, Provider(lambda: [EchoModel()]))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield path
    server.shutdown()
    server.server_close()


def test_a_connection_can_make_several_calls_and_a_client_may_hang_up_after_one(served):
    with connect(served) as s:
        f = s.makefile("rwb")
        for text in ("a", "b"):
            f.write(
                json.dumps(
                    {
                        "op": "run",
                        "model": "echo",
                        "task": "text.cleanup",
                        "params": {},
                        "text": text,
                    }
                ).encode()
                + b"\n"
            )
            f.flush()
            assert json.loads(f.readline()) == {"text": text.upper()}
    with connect(served) as s:
        assert ask(s, {"op": "capabilities"})["protocol"] == "myna-text-v1"


def test_the_socket_is_connectable_by_every_user_and_replaces_a_stale_one(tmp_path):
    path = tmp_path / "text.sock"
    path.write_text("stale")

    server = Server(path, Provider(list))
    try:
        assert stat.S_IMODE(path.stat().st_mode) == 0o666
    finally:
        server.server_close()


def test_an_unreadable_line_is_answered_and_the_connection_goes_on(served):
    with connect(served) as s:
        s.sendall(b"not json\n")
        f = s.makefile("rb")
        assert json.loads(f.readline()) == {"error": "unreadable request"}
        s.sendall(b'{"op": "capabilities"}\n')
        assert json.loads(f.readline())["protocol"] == "myna-text-v1"


def test_a_line_over_the_limit_is_refused_and_the_connection_closed(served):
    with connect(served) as s:
        s.sendall(b"x" * (MAX_LINE + 2))
        f = s.makefile("rb")
        assert json.loads(f.readline()) == {"error": "request too long"}
        assert f.readline() == b""


def test_a_client_that_leaves_before_the_reply_does_not_hurt_the_daemon(served):
    s = connect(served)
    s.sendall(b'{"op": "capabilities"}\n')
    s.close()

    with connect(served) as t:
        assert ask(t, {"op": "capabilities"})["protocol"] == "myna-text-v1"


BINARY = Path("/snap/myna-translation/current/bin/myna-bergamot")
KINDS = {**TAGGER_KINDS, **bergamot_kinds(BINARY)}


def args_for(tmp_path, *extra):
    return daemon.build_parser("myna-text", None).parse_args(
        [
            "--socket",
            str(tmp_path / "share" / "text.sock"),
            "--components",
            str(tmp_path / "components"),
            *extra,
        ]
    )


def run_daemon(tmp_path, environ, *extra):
    stop = threading.Event()
    args = args_for(tmp_path, *extra)
    thread = threading.Thread(target=daemon.serve, args=(args, environ, stop, KINDS))
    thread.start()
    return args.socket, stop, thread


def wait_for(path: Path) -> None:
    for _ in range(200):
        if path.exists():
            return
        threading.Event().wait(0.02)
    raise AssertionError(f"{path} never appeared")


def test_the_daemon_offers_the_installed_components_and_shares_itself_as_a_text_provider(
    tmp_path,
):
    components = tmp_path / "components"
    write_component(
        components, "model-disfluency-en", TAGGER_META, ("tagger.onnx", "tokenizer.json")
    )
    write_component(components, "model-translate-en-fr", TRANSLATE_META, ("config.yml",))
    environ = {"SNAP_NAME": "myna-text", "SNAP_INSTANCE_NAME": "myna-text"}

    sock, stop, thread = run_daemon(tmp_path, environ, "--share-provider")
    try:
        wait_for(sock.parent / "provider.env")
        with connect(sock) as s:
            models = ask(s, {"op": "capabilities"})["models"]
    finally:
        stop.set()
        thread.join(10)

    assert [(m["id"], m["task"]) for m in models] == [
        ("myna-disfluency-en", "text.cleanup"),
        ("bergamot-base-memory-en-fr", "text.translate"),
    ]
    assert (sock.parent / "provider.env").read_text() == (
        "SNAP_NAME=myna-text\nSNAP_INSTANCE_NAME=myna-text\nUNIX_SOCKET=text.sock\nMYNA_ROLE=text\n"
    )
    assert not thread.is_alive()


def test_a_component_installed_after_the_daemon_started_is_offered_without_a_restart(tmp_path):
    (tmp_path / "components").mkdir()
    sock, stop, thread = run_daemon(tmp_path, {})
    try:
        wait_for(sock)
        with connect(sock) as s:
            assert ask(s, {"op": "capabilities"})["models"] == []
            write_component(
                tmp_path / "components", "model-translate-en-fr", TRANSLATE_META, ("config.yml",)
            )
            assert [m["id"] for m in ask(s, {"op": "capabilities"})["models"]] == [
                "bergamot-base-memory-en-fr"
            ]
    finally:
        stop.set()
        thread.join(10)


def test_without_a_components_directory_the_daemon_will_not_guess(tmp_path):
    args = cli.build_parser().parse_args(["--socket", str(tmp_path / "text.sock")])

    with pytest.raises(SystemExit, match="SNAP_COMPONENTS"):
        daemon.serve(args, {}, threading.Event(), KINDS)


def test_components_default_to_the_snaps(tmp_path):
    assert daemon.components_dir(None, {"SNAP_COMPONENTS": "/snap/x/components/1"}) == Path(
        "/snap/x/components/1"
    )
    assert daemon.components_dir(tmp_path, {"SNAP_COMPONENTS": "/elsewhere"}) == tmp_path


def test_each_daemon_serves_its_own_kinds_and_the_translator_takes_a_binary(tmp_path):
    assert cli._kinds(cli.build_parser().parse_args(["--socket", "s"])) is TAGGER_KINDS
    parser = translation_cli.build_parser()
    args = parser.parse_args(["--socket", "s", "--bergamot", str(tmp_path / "b")])
    assert set(translation_cli._kinds(args)) == {"translate"}
    assert translation_cli._bergamot(args.bergamot) == tmp_path / "b"
    assert translation_cli._bergamot(None).name == "myna-bergamot"
    with pytest.raises(SystemExit):
        cli.build_parser().parse_args(["--socket", "s", "--bergamot", "b"])


def test_main_stops_on_sigterm(tmp_path, monkeypatch):
    import signal

    handlers = {}
    monkeypatch.setattr(signal, "signal", lambda sig, fn: handlers.setdefault(sig, fn))
    served = []
    monkeypatch.setattr(daemon, "serve", lambda args, environ, stop, kinds: served.append(stop))

    assert cli.main(["--socket", str(tmp_path / "text.sock"), "--components", str(tmp_path)]) == 0

    handlers[signal.SIGTERM](signal.SIGTERM, None)
    assert served[0].is_set()


def test_a_request_of_the_greatest_length_is_served_and_one_byte_more_is_not(served):
    head = b'{"op": "capabilities", "pad": "'
    tail = b'"}\n'
    # MAX_LINE bytes of request and then the line end.
    fits = head + b"x" * (MAX_LINE - len(head) - len(tail) + 1) + tail
    assert len(fits) == MAX_LINE + 1
    with connect(served) as s:
        s.sendall(fits)
        assert json.loads(s.makefile("rb").readline())["protocol"] == "myna-text-v1"
    with connect(served) as s:
        s.sendall(fits[:-3] + b"x" + tail)
        assert json.loads(s.makefile("rb").readline()) == {"error": "request too long"}


def test_a_request_that_is_exactly_the_limit_without_a_line_end_is_unreadable_not_too_long(served):
    with connect(served) as s:
        s.sendall(b"x" * MAX_LINE)
        s.shutdown(socket.SHUT_WR)
        assert json.loads(s.makefile("rb").readline()) == {"error": "unreadable request"}


def test_the_socket_directory_is_created_when_it_is_missing(tmp_path):
    path = tmp_path / "a" / "b" / "text.sock"

    server = Server(path, Provider(list))
    try:
        assert path.exists()
    finally:
        server.server_close()


def test_the_daemon_applies_the_socket_mode_and_scans_the_components_it_was_given(
    tmp_path, monkeypatch
):
    seen = {}
    monkeypatch.setattr(
        daemon.components, "finder", lambda root, kinds: seen.update(root=root, kinds=kinds) or list
    )
    sock, stop, thread = run_daemon(tmp_path, {}, "--socket-mode", "0600")
    try:
        wait_for(sock)
        mode = stat.S_IMODE(sock.stat().st_mode)
    finally:
        stop.set()
        thread.join(10)

    assert mode == 0o600
    assert seen == {"root": tmp_path / "components", "kinds": KINDS}


def test_the_translator_is_the_one_beside_python_else_the_one_on_the_path(tmp_path, monkeypatch):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    monkeypatch.setattr(sys, "executable", str(bin_dir / "python3"))
    monkeypatch.setattr(shutil, "which", lambda name: "/usr/bin/" + name)

    assert translation_cli._bergamot(None) == Path("/usr/bin/myna-bergamot")
    (bin_dir / "myna-bergamot").write_text("")
    assert translation_cli._bergamot(None) == bin_dir / "myna-bergamot"
    monkeypatch.setattr(shutil, "which", lambda name: None)
    (bin_dir / "myna-bergamot").unlink()
    assert translation_cli._bergamot(None) == bin_dir / "myna-bergamot"


def test_the_parser_wants_a_socket_and_defaults_the_mode_to_world_connectable():
    with pytest.raises(SystemExit):
        cli.build_parser().parse_args([])

    args = cli.build_parser().parse_args(["--socket", "/x/text.sock"])

    assert (args.socket_mode, args.share_provider, args.components) == ("0666", False, None)


def test_replies_carry_accents_as_utf8_not_escapes(tmp_path):
    class French(EchoModel):
        def run(self, text, deadline_s):
            return "Bonjour, comment allez-vous aujourd'hui ? \u00e9t\u00e9"

    path = tmp_path / "text.sock"
    server = Server(path, Provider(lambda: [French()]))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with connect(path) as s:
            s.sendall(
                json.dumps(
                    {
                        "op": "run",
                        "model": "echo",
                        "task": "text.cleanup",
                        "params": {},
                        "text": "a",
                    }
                ).encode()
                + b"\n"
            )
            line = s.makefile("rb").readline()
    finally:
        server.shutdown()
        server.server_close()

    assert "été".encode() in line
    assert b"\\u" not in line
