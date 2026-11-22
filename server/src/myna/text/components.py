"""Find the model components installed beside a text-provider snap.

Each ``model-*`` component carries a ``myna-text-model.json`` naming what it
is (``kind``), what it serves and what its files are. The ids, tasks and
languages come from there, so installing a component is all it takes to offer
a model. A snap registers the kinds it can run: a component of another kind is
skipped and named in the log.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

from myna.text.protocol import Model

MANIFEST = "myna-text-model.json"

#: Builds the model of one component directory from its manifest; raises
#: ``OSError``, ``KeyError``, ``TypeError`` or ``ValueError`` if it is unusable.
Builder = Callable[[Path, dict[str, Any]], Model]

log = logging.getLogger("myna.text")


def find_models(root: Path, kinds: Mapping[str, Builder]) -> list[Model]:
    """The models of every readable component of a registered kind under ``root``."""
    models: list[Model] = []
    try:
        directories = sorted(p for p in root.iterdir() if p.is_dir())
    except OSError:
        return models
    for directory in directories:
        try:
            meta = json.loads((directory / MANIFEST).read_text(encoding="utf-8"))
        except FileNotFoundError:
            continue  # not a model component
        except (OSError, ValueError) as e:
            log.warning("%s: unreadable %s (%s)", directory.name, MANIFEST, type(e).__name__)
            continue
        try:
            build = kinds.get(meta["kind"])
            model = build(directory, meta) if build else None
        except (OSError, KeyError, TypeError, ValueError) as e:
            log.warning("%s: not a usable model component (%s)", directory.name, type(e).__name__)
            continue
        if model is None:
            log.warning("%s: unknown kind %r", directory.name, meta.get("kind"))
        else:
            models.append(model)
    return models


def require(directory: Path, *names: str) -> None:
    """Raise ``FileNotFoundError`` unless every named file is in ``directory``."""
    for name in names:
        if not (directory / name).is_file():
            raise FileNotFoundError(name)


def finder(root: Path, kinds: Mapping[str, Builder]) -> Callable[[], list[Model]]:
    return lambda: find_models(root, kinds)
