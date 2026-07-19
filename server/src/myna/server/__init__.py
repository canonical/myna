"""UbuSTT server: serve an STT adapter on a Unix domain socket (T14a).

This is the process the whisper inference snap runs as its engine server
(see docs/asr-inference-snap-design.md): the faster-whisper testbed adapter
composed with the WebSocket-over-UDS transport. What the harness measures is
what the snap ships.
"""

from myna.server.cli import main

__all__ = ["main"]
