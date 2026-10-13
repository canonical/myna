"""Write the private embedded Lemonade configuration atomically."""

from __future__ import annotations

import json
import os
from pathlib import Path


def write_config(destination: Path, flm_binary: Path, websocket_port: int) -> None:
    """Replace ``destination`` with Myna's forced-offline lemond config."""
    if not 1 <= websocket_port <= 65535:
        raise ValueError("websocket port must be between 1 and 65535")
    config = {
        "offline": True,
        "no_fetch_executables": True,
        "auto_check_model_updates": False,
        "auto_update_models": False,
        "broadcast": False,
        "inhibit_suspend": False,
        "log_file": "disabled",
        "websocket_port": websocket_port,
        "flm": {"npu_bin": str(flm_binary)},
    }

    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(f".{destination.name}.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")
    temporary.chmod(0o600)
    temporary.replace(destination)


def main(argv: list[str] | None = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config_path", type=Path)
    parser.add_argument("flm_binary", type=Path)
    parser.add_argument("websocket_port", type=int)
    args = parser.parse_args(argv)
    write_config(args.config_path, args.flm_binary, args.websocket_port)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
