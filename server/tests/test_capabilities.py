"""Capabilities wire codec (T24): the discovery document must round-trip
through JSON-shaped dicts so any transport can carry it unchanged."""

import json

from myna.core import (
    AudioFormat,
    Capabilities,
    capabilities_from_wire,
    capabilities_to_wire,
)


def test_round_trip_preserves_all_fields():
    caps = Capabilities(
        models=("whisper-small",),
        languages=("*",),
        input_formats=(AudioFormat(16_000, 1, 2),),
        punctuation=True,
        translation=False,
    )
    assert capabilities_from_wire(capabilities_to_wire(caps)) == caps


def test_wire_form_is_json_serializable_and_nested_format_decodes():
    caps = Capabilities(input_formats=(AudioFormat(8_000, 2, 2), AudioFormat()))
    wire = json.loads(json.dumps(capabilities_to_wire(caps)))  # through real JSON
    restored = capabilities_from_wire(wire)
    assert restored == caps
    assert isinstance(restored.input_formats[0], AudioFormat)
    assert restored.input_formats[0].sample_rate_hz == 8_000


def test_defaults_are_conservative():
    caps = Capabilities()
    assert caps.languages == ("*",)
    assert caps.input_formats == (AudioFormat(),)
    assert caps.punctuation is False
    assert caps.translation is False
