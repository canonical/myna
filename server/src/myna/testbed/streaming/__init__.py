"""Streaming emission machinery (feature 008-progressive-emission).

Shared by the whisper adapter's re-decode loop and the parakeet adapter's
chunk-commit: a bounded rolling window (``window.py``) and the commit-strategy
seam (``strategies.py``). Strategies decide *what to commit when*; the wire
contract (007) is unchanged — committed/unstable dispositions only.
"""
