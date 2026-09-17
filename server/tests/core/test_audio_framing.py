"""Sample-frame alignment of PCM byte streams, for any frame width."""

from __future__ import annotations

import logging

import pytest

from myna.core.audio import AudioFormat, PcmFramer


def test_a_frame_is_a_sample_for_every_channel():
    assert AudioFormat(channels=2, sample_width_bytes=3).frame_bytes == 6
    assert AudioFormat().bytes_per_second == 32_000


def test_stereo_frames_split_anywhere_are_carried_whole():
    framer = PcmFramer(4)
    stream = bytes(range(24))
    out = [framer.feed(stream[a:b]) for a, b in ((0, 1), (1, 6), (6, 7), (7, 21), (21, 24))]
    assert [len(piece) for piece in out] == [0, 4, 0, 16, 4]
    assert b"".join(out) == stream


def test_flush_discards_a_partial_frame_and_says_how_much(caplog):
    framer = PcmFramer(4)
    with caplog.at_level(logging.WARNING, logger="myna.core.audio"):
        assert framer.feed(bytes(7)) == bytes(4)
        framer.flush()
    assert [r.getMessage() for r in caplog.records] == [
        "discarding 3 byte(s) of a partial PCM sample frame at the utterance boundary"
    ]
    assert framer.feed(b"abcd") == b"abcd"


def test_flush_on_a_frame_boundary_is_silent(caplog):
    framer = PcmFramer(2)
    framer.feed(bytes(4))
    with caplog.at_level(logging.WARNING):
        framer.flush()
    assert caplog.text == ""


@pytest.mark.parametrize("frame_bytes", [0, -2])
def test_a_frame_has_at_least_one_byte(frame_bytes):
    with pytest.raises(ValueError, match=f"at least one byte, got {frame_bytes}"):
        PcmFramer(frame_bytes)
