"""provider.env: the provider side of the ``inference-provider`` content share.

Consumers find a backend by reading this file in each shared directory, so it
names the snap and the socket beside it. The socket appearing is the readiness
signal; this file is static identity and outlives the server process.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from pathlib import Path

FILENAME = "provider.env"


def render(snap_name: str, instance_name: str, socket_name: str) -> str:
    fields = {
        "SNAP_NAME": snap_name,
        "SNAP_INSTANCE_NAME": instance_name,
        "UNIX_SOCKET": socket_name,
    }
    for key, value in fields.items():
        if "\n" in value or "\r" in value:
            raise ValueError(f"{key} contains a line break: {value!r}")
    return "".join(f"{key}={value}\n" for key, value in fields.items())


def write(path: Path, content: str) -> None:
    """Replace ``path`` atomically, so a consumer never reads a partial file."""
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(content)
    os.chmod(tmp, 0o644)
    os.replace(tmp, path)


def share(socket: Path, environ: Mapping[str, str]) -> Path:
    """Write provider.env beside ``socket`` from the snap's identity."""
    for name in ("SNAP_NAME", "SNAP_INSTANCE_NAME"):
        if not environ.get(name):
            raise SystemExit(f"--share-provider needs {name} in the environment")
    path = socket.parent / FILENAME
    content = render(environ["SNAP_NAME"], environ["SNAP_INSTANCE_NAME"], socket.name)
    path.parent.mkdir(parents=True, exist_ok=True)
    write(path, content)
    return path
