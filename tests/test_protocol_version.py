"""Protocol-version negotiation over the wire (T35).

Versioning is a wire concern: loopback can't disagree (both ends are this
library), so these exercise the ws+unix transport, where a client and a
separately-deployed snap server can be on different protocol versions.
"""

import json

import pytest
from websockets.asyncio.client import unix_connect

from myna.core import (
    PROTOCOL_VERSION,
    SessionConfig,
    WsUnixClient,
    serve_unix,
)
from myna.testbed import FakeAdapter


@pytest.fixture
async def server(tmp_path):
    socket_path = tmp_path / "ubustt.sock"
    async with serve_unix(FakeAdapter(), socket_path):
        yield socket_path


async def test_session_created_echoes_protocol_version(server):
    """A compatible client learns the version the server speaks, from the
    session.created handshake reply (captured as the session reads events)."""
    client = WsUnixClient(server)
    session = await client.open_session(SessionConfig())
    await session.finish_audio()
    async for _event in session.events():
        pass  # drain to the terminal event; session.created is seen first
    assert session.protocol_version == PROTOCOL_VERSION
    await session.aclose()


async def test_unsupported_version_is_a_terminal_error(server):
    """A client declaring a version the server can't serve gets a terminal
    transcription.error, not a misinterpreted stream."""
    ws = await unix_connect(str(server))
    try:
        await ws.send(
            json.dumps(
                {
                    "type": "session.start",
                    "protocol_version": "999",
                    "config": {},
                }
            )
        )
        reply = json.loads(await ws.recv())
    finally:
        await ws.close()
    assert reply["event"] == "transcription.error"
    assert reply["data"]["code"] == "unsupported_protocol_version"


async def test_missing_version_is_accepted(server):
    """Clients predating the version field speak the only version that existed
    then — treat a missing protocol_version as compatible."""
    ws = await unix_connect(str(server))
    try:
        await ws.send(json.dumps({"type": "session.start", "config": {}}))
        reply = json.loads(await ws.recv())
    finally:
        await ws.close()
    assert reply["type"] == "session.created"
    assert reply["protocol_version"] == PROTOCOL_VERSION
