"""Golden-frame tests for disposition encoding in IE115 wire protocol (T08, feature 007).

Tests that the disposition field is correctly encoded/decoded on delta events
and that backward compatibility is maintained (absent field → committed default).
"""

import pytest

from myna.core import Disposition, TranscriptionFinal
from myna.core.wire_ie115 import encode_delta, decode_delta


def test_disposition_encoding_committed():
    """Test that committed disposition is encoded in delta events."""
    event = TranscriptionFinal(
        text="Hello world",
        disposition=Disposition.COMMITTED,
        segment_index=0,
    )
    
    wire_frame = encode_delta(event, item_id="item_001")
    
    assert wire_frame["type"] == "conversation.item.input_audio_transcription.delta"
    assert wire_frame["item_id"] == "item_001"
    assert wire_frame["delta"] == "Hello world"
    assert wire_frame["disposition"] == "committed"
    assert wire_frame["segment_index"] == 0


def test_disposition_encoding_unstable():
    """Test that unstable disposition is encoded in delta events."""
    event = TranscriptionFinal(
        text="Hello wor",
        disposition=Disposition.UNSTABLE,
    )
    
    wire_frame = encode_delta(event, item_id="item_001")
    
    assert wire_frame["type"] == "conversation.item.input_audio_transcription.delta"
    assert wire_frame["item_id"] == "item_001"
    assert wire_frame["delta"] == "Hello wor"
    assert wire_frame["disposition"] == "unstable"
    assert "segment_index" not in wire_frame  # Only present for committed


def test_disposition_decoding_committed():
    """Test that committed disposition is decoded from wire frames."""
    wire_frame = {
        "type": "conversation.item.input_audio_transcription.delta",
        "item_id": "item_001",
        "content_index": 0,
        "delta": "Hello world",
        "disposition": "committed",
        "segment_index": 0,
    }
    
    event = decode_delta(wire_frame)
    
    assert isinstance(event, TranscriptionFinal)
    assert event.text == "Hello world"
    assert event.disposition == Disposition.COMMITTED
    assert event.segment_index == 0


def test_disposition_decoding_unstable():
    """Test that unstable disposition is decoded from wire frames."""
    wire_frame = {
        "type": "conversation.item.input_audio_transcription.delta",
        "item_id": "item_001",
        "content_index": 0,
        "delta": "Hello wor",
        "disposition": "unstable",
    }
    
    event = decode_delta(wire_frame)
    
    assert isinstance(event, TranscriptionFinal)
    assert event.text == "Hello wor"
    assert event.disposition == Disposition.UNSTABLE
    assert event.segment_index is None


def test_backward_compat_absent_disposition():
    """Test backward compatibility: absent disposition defaults to committed."""
    wire_frame = {
        "type": "conversation.item.input_audio_transcription.delta",
        "item_id": "item_001",
        "content_index": 0,
        "delta": "Hello world",
        # No disposition field - old wire format
    }
    
    event = decode_delta(wire_frame)
    
    assert isinstance(event, TranscriptionFinal)
    assert event.text == "Hello world"
    assert event.disposition == Disposition.COMMITTED  # Default
    assert event.segment_index is None


def test_multiple_committed_segments():
    """Test encoding multiple committed segments with increasing segment_index."""
    segments = [
        TranscriptionFinal(text="Hello ", disposition=Disposition.COMMITTED, segment_index=0),
        TranscriptionFinal(text="world. ", disposition=Disposition.COMMITTED, segment_index=1),
        TranscriptionFinal(text="How are you?", disposition=Disposition.COMMITTED, segment_index=2),
    ]
    
    for i, seg in enumerate(segments):
        frame = encode_delta(seg, item_id=f"item_{i:03d}")
        assert frame["disposition"] == "committed"
        assert frame["segment_index"] == i
        assert frame["delta"] == seg.text
