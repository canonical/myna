"""UbuSTT server: serve an STT adapter on a Unix domain socket (T14a).

This is the process inference snaps run as their engine server: a testbed
adapter composed with the WebSocket-over-UDS transport. What the harness
measures is what the snap ships.
"""

from myna.server.cli import main

__all__ = ["main"]
