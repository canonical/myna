"""Myna — speech to text on Ubuntu.

Package layout:

- ``myna.core``    — shared vocabulary: audio types, transcript events, session
  config, and the transport abstraction. Everything else depends only on this.
- ``myna.testbed`` — candidate-adapter evaluation testbed: adapters wrap STT
  candidates behind the IE114-shaped service interface; the harness drives
  them and records timing/accuracy results.

The Ubuntu Desktop dictation client (UD129) — session controller + text
injection — now lives in the Rust ``client/myna-desktop`` crate (feature
003-desktop-injection); the former ``myna.desktop`` interface stubs were retired
once that contract landed in Rust.
"""
