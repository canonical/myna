"""``myna-translation``: the daemon of the ``myna-translation`` snap."""

from __future__ import annotations

import argparse
import shutil
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path

from myna.text import daemon
from myna.text.components import Builder
from myna.translation.bergamot import kinds


def build_parser() -> argparse.ArgumentParser:
    parser = daemon.build_parser("myna-translation", __doc__)
    parser.add_argument(
        "--bergamot",
        type=Path,
        help="the myna-bergamot binary (default: beside this interpreter, else on PATH)",
    )
    return parser


def _bergamot(given: Path | None) -> Path:
    if given is not None:
        return given
    beside = Path(sys.executable).parent / "myna-bergamot"
    return beside if beside.is_file() else Path(shutil.which("myna-bergamot") or beside)


def _kinds(args: argparse.Namespace) -> Mapping[str, Builder]:
    return kinds(_bergamot(args.bergamot))


def main(argv: Sequence[str] | None = None) -> int:
    return daemon.run(build_parser(), argv, _kinds)


if __name__ == "__main__":
    raise SystemExit(main())
