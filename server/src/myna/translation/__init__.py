"""myna-translation: the translation provider behind the ``myna-translation`` snap.

It serves ``myna-text-v1`` like :mod:`myna.text`, through ``myna-bergamot``, and
imports no ONNX Runtime: the two snaps share the protocol, the component scan
and the server loop, and nothing else.
"""
