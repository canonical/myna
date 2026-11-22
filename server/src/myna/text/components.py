"""Find the model components installed beside the ``myna-text`` snap.

Each ``model-*`` component carries a ``myna-text-model.json`` naming what it
is (``kind``), what it serves and what its files are. The ids, tasks and
languages come from there, so installing a component is all it takes to offer
a model.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Callable
from pathlib import Path
from typing import Any

from myna.text.protocol import Model
from myna.text.tagger import TaggerModel
from myna.text.translate import TranslateModel

MANIFEST = "myna-text-model.json"

log = logging.getLogger("myna.text")


def find_models(root: Path, binary: Path) -> list[Model]:
    """The models of every readable component under ``root``."""
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
            model = _build(directory, meta, binary)
        except (OSError, KeyError, TypeError, ValueError) as e:
            log.warning("%s: not a usable model component (%s)", directory.name, type(e).__name__)
            continue
        if model is None:
            log.warning("%s: unknown kind %r", directory.name, meta.get("kind"))
        else:
            models.append(model)
    return models


def _build(directory: Path, meta: dict[str, Any], binary: Path) -> Model | None:
    kind = meta["kind"]
    if kind == "disfluency-tagger":
        _require(directory, "tagger.onnx", "tokenizer.json")
        return TaggerModel(directory, meta)
    if kind == "translate":
        _require(directory, str(meta["config"]))
        return TranslateModel(directory, meta, binary)
    return None


def _require(directory: Path, *names: str) -> None:
    for name in names:
        if not (directory / name).is_file():
            raise FileNotFoundError(name)


def finder(root: Path, binary: Path) -> Callable[[], list[Model]]:
    return lambda: find_models(root, binary)
