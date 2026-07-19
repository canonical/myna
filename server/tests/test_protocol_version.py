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


async def test_session_created_greeting_announces_protocol_version(server):
    """A version-aware client learns the version the server speaks from the
    session.created greeting (captured as the session reads events)."""
    client = WsUnixClient(server)
    session = await client.open_session(SessionConfig())
    await session.finish_audio()
    async for _event in session.events():
        pass  # drain to the terminal event; the greeting is seen first
    assert session.protocol_version == PROTOCOL_VERSION
    await session.aclose()


async def test_greeting_is_sent_before_the_client_speaks(server):
    """The server speaks first (the OpenAI-Realtime pattern): the greeting
    arrives on connect, before the client sends anything — a stock client that
    waits for session.created before its first frame must not deadlock against
    the shape-sniff."""
    ws = await unix_connect(str(server))
    try:
        greeting = json.loads(await ws.recv())  # no frame sent yet
    finally:
        await ws.close()
    assert greeting["type"] == "session.created"
    assert greeting["protocol_version"] == PROTOCOL_VERSION
    assert "session" in greeting  # IE115 server defaults, for stock clients


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
        if reply.get("type") == "session.created":  # the greeting
            reply = json.loads(await ws.recv())
    finally:
        await ws.close()
    assert reply["event"] == "transcription.error"
    assert reply["data"]["code"] == "unsupported_protocol_version"


async def test_missing_version_is_accepted(server):
    """Clients predating the version field speak the only version that existed
    then — treat a missing protocol_version as compatible: the session runs to
    a clean terminal instead of being rejected."""
    ws = await unix_connect(str(server))
    try:
        json.loads(await ws.recv())  # the greeting
        await ws.send(json.dumps({"type": "session.start", "config": {}}))
        await ws.send(json.dumps({"type": "session.finish"}))
        reply = json.loads(await ws.recv())
        while reply.get("event") not in ("transcription.done", "transcription.error"):
            reply = json.loads(await ws.recv())
    finally:
        await ws.close()
    assert reply["event"] == "transcription.done"
