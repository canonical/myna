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
        streaming=True,
        keywords=True,
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
    assert caps.streaming is None
    assert caps.keywords is False


def test_a_server_predating_streaming_decodes_as_unknown():
    wire = capabilities_to_wire(Capabilities(models=("whisper-small",)))
    del wire["streaming"]
    assert capabilities_from_wire(wire).streaming is None


def test_wire_carries_streaming_as_a_plain_boolean():
    wire = json.loads(json.dumps(capabilities_to_wire(Capabilities(streaming=False))))
    assert wire["streaming"] is False


def test_runtime_round_trips_through_json():
    caps = Capabilities(runtime={"onnxruntime": "1.23.0", "execution_provider": "CPU"})
    wire = json.loads(json.dumps(capabilities_to_wire(caps)))
    assert wire["runtime"] == {"onnxruntime": "1.23.0", "execution_provider": "CPU"}
    assert capabilities_from_wire(wire) == caps


def test_a_server_predating_runtime_decodes_as_unknown():
    wire = capabilities_to_wire(Capabilities(models=("whisper-small",)))
    del wire["runtime"]
    assert capabilities_from_wire(wire).runtime is None


def test_a_field_from_a_newer_server_is_ignored_rather_than_fatal():
    wire = capabilities_to_wire(Capabilities(models=("whisper-small",)))
    wire["from_the_future"] = True
    assert capabilities_from_wire(wire).models == ("whisper-small",)
