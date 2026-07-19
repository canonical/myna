"""Myna — speech to text on Ubuntu.

Package layout:

- ``myna.core``    — shared vocabulary: audio types, transcript events, session
  config, and the transport abstraction. Everything else depends only on this.
- ``myna.testbed`` — candidate-adapter evaluation testbed: adapters wrap STT
  candidates behind the IE114-shaped service interface; the harness drives
  them and records timing/accuracy results.
- ``myna.desktop`` — interface stubs for the Ubuntu Desktop dictation client
  (UD129): session controller, text injection. Implementation comes later;
  the stubs exist so the testbed and the desktop client share one vocabulary.
"""
