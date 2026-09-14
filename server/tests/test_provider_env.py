"""Tests for provider.env, the provider side of the inference-provider share."""

import stat

import pytest

from myna.server import provider_env


def test_render_is_exactly_the_three_contract_lines():
    assert provider_env.render("myna-whisper", "myna-whisper_dev", "myna.sock") == (
        "SNAP_NAME=myna-whisper\nSNAP_INSTANCE_NAME=myna-whisper_dev\nUNIX_SOCKET=myna.sock\n"
    )


@pytest.mark.parametrize(
    "args",
    [
        ("myna\nOPENAI_BASE_URL=http://x", "i", "myna.sock"),
        ("n", "i\r", "myna.sock"),
        ("n", "i", "my\nna.sock"),
    ],
)
def test_render_rejects_a_value_that_would_inject_a_line(args):
    with pytest.raises(ValueError, match="line break"):
        provider_env.render(*args)


def test_write_replaces_the_file_atomically(tmp_path):
    path = tmp_path / "provider.env"
    path.write_text("SNAP_NAME=stale\nEXTRA=1\n")

    provider_env.write(path, "SNAP_NAME=fresh\n")

    assert path.read_text() == "SNAP_NAME=fresh\n"
    assert [p.name for p in tmp_path.iterdir()] == ["provider.env"]
    assert stat.S_IMODE(path.stat().st_mode) == 0o644


def test_share_writes_beside_the_socket_from_the_snap_environment(tmp_path):
    socket = tmp_path / "share" / "provider" / "myna.sock"

    written = provider_env.share(
        socket, {"SNAP_NAME": "myna-parakeet", "SNAP_INSTANCE_NAME": "myna-parakeet"}
    )

    assert written == socket.parent / "provider.env"
    assert written.read_text() == (
        "SNAP_NAME=myna-parakeet\nSNAP_INSTANCE_NAME=myna-parakeet\nUNIX_SOCKET=myna.sock\n"
    )


def test_share_rewrites_into_an_existing_share_directory_on_restart(tmp_path):
    socket = tmp_path / "provider" / "myna.sock"
    socket.parent.mkdir()
    provider_env.share(socket, {"SNAP_NAME": "old", "SNAP_INSTANCE_NAME": "old"})

    provider_env.share(socket, {"SNAP_NAME": "new", "SNAP_INSTANCE_NAME": "new_x"})

    assert (socket.parent / "provider.env").read_text() == (
        "SNAP_NAME=new\nSNAP_INSTANCE_NAME=new_x\nUNIX_SOCKET=myna.sock\n"
    )


@pytest.mark.parametrize("missing", ["SNAP_NAME", "SNAP_INSTANCE_NAME"])
def test_share_refuses_to_guess_a_missing_snap_identity(tmp_path, missing):
    environ = {"SNAP_NAME": "myna-whisper", "SNAP_INSTANCE_NAME": "myna-whisper"}
    environ[missing] = ""

    with pytest.raises(SystemExit, match=missing):
        provider_env.share(tmp_path / "myna.sock", environ)

    with pytest.raises(SystemExit, match=missing):
        provider_env.share(tmp_path / "myna.sock", {k: v for k, v in environ.items() if v})

    assert list(tmp_path.iterdir()) == []
