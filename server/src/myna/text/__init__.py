"""myna-text: the text model provider behind the ``myna-text`` snap.

It serves ``myna-text-v1``, JSON lines over a Unix socket, to the Myna daemon
(``client/myna-desktop/src/process/wire.rs`` is the other side of the wire).
The daemon chooses the model; this process lists what it has, loads what it is
told to and runs it.
"""
