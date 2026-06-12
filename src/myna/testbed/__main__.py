"""Phase 0 smoke run: fake adapter over the loopback transport.

    uv run python -m myna.testbed

Prints the result record as JSON. This is the end-to-end demo of the
contract: harness -> transport -> adapter -> events -> metrics.
"""

import asyncio
import json

from myna.core import LoopbackClient
from myna.testbed import FakeAdapter, Harness, SilenceSource


async def main() -> None:
    adapter = FakeAdapter()
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=SilenceSource(duration_seconds=0.5, realtime=True),
    )
    print(json.dumps(record.to_json(), indent=2))


if __name__ == "__main__":
    asyncio.run(main())
