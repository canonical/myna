import pytest

from myna.core import (
    Segment,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
    event_from_wire,
    event_to_wire,
)


@pytest.mark.parametrize(
    "event",
    [
        TranscriptionProgress(snippet="he"),
        TranscriptionProgress(),
        TranscriptionFinal(text="hello world"),
        TranscriptionFinal(
            text="hello world",
            segments=(Segment(start=0.0, end=1.2, text="hello world", score=-0.1),),
        ),
        TranscriptionDone(text="hello world"),
        TranscriptionError(code="language_not_supported", message="no model for 'xx'"),
    ],
)
def test_wire_roundtrip(event):
    assert event_from_wire(event_to_wire(event)) == event


def test_wire_shape_matches_ie114_framing():
    wire = event_to_wire(TranscriptionFinal(text="hi"))
    assert wire["event"] == "transcription.final"
    assert wire["data"]["text"] == "hi"


def test_unknown_event_rejected():
    with pytest.raises(ValueError):
        event_from_wire({"event": "transcription.replace", "data": {}})


def test_session_config_wire_roundtrip():
    from myna.core import AudioFormat, SessionConfig, session_config_from_wire, session_config_to_wire

    config = SessionConfig(
        audio_format=AudioFormat(sample_rate_hz=48_000, channels=2),
        language="en",
        prompt="dictation",
        timestamp_granularity="segment",
    )
    assert session_config_from_wire(session_config_to_wire(config)) == config
    assert session_config_from_wire({}) == SessionConfig()
