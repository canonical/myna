"""``myna-text``: the daemon of the ``myna-text`` snap."""

from __future__ import annotations

import argparse
from collections.abc import Mapping, Sequence

from myna.text import daemon
from myna.text.components import Builder
from myna.text.tagger import KINDS


def build_parser() -> argparse.ArgumentParser:
    return daemon.build_parser("myna-text", __doc__)


def _kinds(_: argparse.Namespace) -> Mapping[str, Builder]:
    return KINDS


def main(argv: Sequence[str] | None = None) -> int:
    return daemon.run(build_parser(), argv, _kinds)


if __name__ == "__main__":
    raise SystemExit(main())
