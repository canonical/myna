"""Testbed smoke run: fake adapter through a chosen transport.

    uv run python -m myna.testbed [--transport loopback|ws]

Prints the result record as JSON. This is the end-to-end demo of the
contract: harness -> transport -> adapter -> events -> metrics. The ws
variant serves WebSocket-over-UDS on a temporary socket, the real IE114
direction of travel.
"""

import argparse
import asyncio
import json
import tempfile
from pathlib import Path

from myna.core import LoopbackClient, WsUnixClient, serve_unix
from myna.testbed import FakeAdapter, Harness, SilenceSource


async def main(transport: str) -> None:
    adapter = FakeAdapter()
    source = SilenceSource(duration_seconds=0.5, realtime=True)
    if transport == "loopback":
        record = await Harness().run(
            client=LoopbackClient(adapter), candidate=adapter.candidate, source=source
        )
    else:
        with tempfile.TemporaryDirectory() as tmp:
            socket_path = Path(tmp) / "ubustt.sock"
            async with serve_unix(adapter, socket_path):
                record = await Harness().run(
                    client=WsUnixClient(socket_path),
                    candidate=adapter.candidate,
                    source=source,
                )
    print(json.dumps(record.to_json(), indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--transport", choices=("loopback", "ws"), default="loopback")
    args = parser.parse_args()
    asyncio.run(main(args.transport))
