"""Wire-protocol versioning (T35, IE115 reconciliation).

A single version number covers the whole client↔service contract over a
transport: the opening handshake (``session.start`` / ``session.created``), the
event vocabulary (``myna.core.events``), and the ``SessionConfig`` /
``Capabilities`` wire shapes. It is deliberately *one* number, not one per
message — adding or renaming an event type, or changing a config/capabilities
field, is a change to the set, so a client and service that disagree fail fast
rather than silently misinterpret frames. (Note that even additive event types
are breaking: ``event_from_wire`` rejects unknown types, so an old client can't
ignore a new event. Bump on any such change.)

Transport-agnostic by design (CLAUDE.md: transport behind an abstraction). The
version travels *in band* in the opening handshake, not as a WebSocket
subprotocol token, so the same negotiation works over loopback, ws+unix, or any
future transport. In-process transports (loopback) can't disagree — both ends
are this library — so negotiation is a no-op there; only wire transports
exercise it.

This is the flexibility lever for "support any future model": the API surface
is versioned so it can evolve (new events, new config) without silently breaking
deployed clients/snaps. PROVISIONAL — input to IE115 (plan T35). Keep this
module's git history as the protocol changelog.
"""

from __future__ import annotations

PROTOCOL_VERSION = "1"
SUPPORTED_PROTOCOL_VERSIONS = frozenset({PROTOCOL_VERSION})


def is_supported(version: str | None) -> bool:
    """Whether a peer-declared protocol version is one we can serve.

    A missing version (``None``) is treated as compatible: clients that predate
    the version field necessarily speak the only version that existed then.
    """
    if version is None:
        return True
    return version in SUPPORTED_PROTOCOL_VERSIONS
