"""plugin-scaffold writes a plugin from recorded Remote Settings records."""

from __future__ import annotations

import argparse
import json
import tomllib
from pathlib import Path

import pytest

from myna.benchmarker import fleurs_codes
from myna.benchmarker import plugin_scaffold as ps

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
FIXTURE = json.loads((HERE / "data" / "remote_settings_fixture.json").read_text())
RECORDS = FIXTURE["records"]


def metadata(arch: str, directory: str):
    return FIXTURE["metadata"].get(f"{arch}/{directory}")


def make(pair: str, **kw):
    return ps.scaffold(pair, records=RECORDS, metadata=metadata, **kw)


@pytest.mark.repo_tree
def test_en_de_reproduces_the_committed_plugin_but_for_the_packed_size():
    got = tomllib.loads(make("en:de").toml)
    want = tomllib.loads((REPO / "translation-snap/plugins/translate-en-de.toml").read_text())
    assert got.pop("download_bytes") == sum(f["bytes"] for f in got["files"])
    want.pop("download_bytes")
    assert got == want


@pytest.mark.repo_tree
def test_en_de_config_and_measured_seed_match_the_committed_ones():
    got = make("en:de")
    snap = REPO / "translation-snap"
    assert got.config == (snap / "models/translate-en-de/config.yml").read_text()
    seed = json.loads(got.measured)
    assert seed["gate"] == "fail"
    assert seed["quality_source"] == "pending"


def test_picks_the_newest_version_of_the_architecture_whose_hash_matches():
    got = tomllib.loads(make("en:de").toml)
    assert got["pin"] == "mozilla firefox-translations en-de base-memory 2.1"
    assert got["gate"] == {"vendor_comet": 0.8651}
    assert [f["name"] for f in got["files"]] == [
        "model.ende.intgemm.alphas.bin",
        "lex.50.50.ende.s2t.bin",
        "vocab.ende.spm",
    ]
    assert got["files"][0]["url"].startswith(ps.ATTACHMENTS_URL + "main-workspace/")


def test_falls_back_to_tiny_when_base_memory_is_unpublished():
    result = make("en:fa")
    got = tomllib.loads(result.toml)
    assert got["id"] == "bergamot-tiny-en-fa"
    assert got["pin"].endswith("en-fa tiny 1.1")
    assert any("base-memory is not published" in w for w in result.warnings)
    assert got["summary"].startswith("English to Persian")


def test_arch_can_be_forced_and_a_missing_one_is_refused():
    assert make("en:fa", arch="tiny").name == "translate-en-fa"
    with pytest.raises(ps.ScaffoldError, match="no record version"):
        make("en:fa", arch="base-memory")


def _records(src, dst, version, digest, size=1000):
    pair = f"{src}{dst}".replace("-Hans", "").replace("-Hant", "")
    files = {
        "model": f"model.{pair}.intgemm.alphas.bin",
        "lex": f"lex.50.50.{pair}.s2t.bin",
        "vocab": f"vocab.{pair}.spm",
    }
    return [
        {
            "fromLang": src,
            "toLang": dst,
            "version": version,
            "fileType": kind,
            "filter_expression": "",
            "attachment": {
                "hash": digest if kind == "model" else f"{kind}-{version}",
                "size": size,
                "filename": name,
                "location": f"main-workspace/translations-models/{kind}-{version}.bin",
            },
        }
        for kind, name in files.items()
    ]


def test_the_newest_model_wins_even_when_no_metadata_describes_it():
    records = _records("ru", "en", "1.1", "old") + _records("ru", "en", "2.1", "new")
    described = {"hash": "old", "byteSize": 1000, "flores": {"comet": 0.85}}

    def meta(arch, directory):
        return described if arch == "tiny" else None

    result = ps.scaffold("ru:en", records=records, metadata=meta)
    got = tomllib.loads(result.toml)
    assert got["id"] == "bergamot-mozilla-ru-en"
    assert got["pin"] == "mozilla firefox-translations ru-en mozilla 2.1"
    assert got["attribution"].endswith("(version 2.1)")
    assert got["summary"] == "Russian to English translation (Firefox Translations)"
    assert got["gate"] == {}
    assert any("no metadata.json describes" in w for w in result.warnings)
    forced = tomllib.loads(ps.scaffold("ru:en", records=records, metadata=meta, arch="tiny").toml)
    assert forced["pin"].endswith("ru-en tiny 1.1")
    assert forced["gate"] == {"vendor_comet": 0.85}
    described["hash"] = "new"
    newest = tomllib.loads(ps.scaffold("ru:en", records=records, metadata=meta).toml)
    assert newest["id"] == "bergamot-tiny-ru-en"
    assert newest["pin"].endswith("ru-en tiny 2.1")


def test_a_script_tag_is_lower_case_in_the_names_snapd_sees():
    records = _records("en", "zh-Hans", "1.0", "m")
    result = ps.scaffold("en:zh-Hans", records=records, metadata=lambda a, d: None)
    got = tomllib.loads(result.toml)
    assert result.name == "translate-en-zh-hans"
    assert (got["id"], got["component"]) == ("bergamot-mozilla-en-zh-hans", "model-translate-en-zh-hans")
    assert (got["language"], got["to"]) == ("en", "zh-Hans")
    assert got["notice"].startswith("model-translate-en-zh-hans:")


def test_split_vocabs_go_into_the_config():
    config = make("en:ja", no_eval=False).config
    assert "vocabs: [srcvocab.enja.spm, trgvocab.enja.spm]" in config


def test_a_language_without_a_fleurs_locale_needs_no_eval():
    records = [dict(r, toLang="nn") for r in RECORDS if r["toLang"] == "de"]
    with pytest.raises(ps.ScaffoldError, match="--no-eval"):
        ps.scaffold("en:nn", records=records, metadata=metadata)
    result = ps.scaffold("en:nn", records=records, metadata=metadata, no_eval=True)
    assert any("no FLEURS locale" in w for w in result.warnings)


def test_a_missing_comet_is_labelled_not_measured():
    def bare(arch, directory):
        meta = metadata(arch, directory)
        return None if meta is None else {k: v for k, v in meta.items() if k != "flores"}

    result = ps.scaffold("en:de", records=RECORDS, metadata=bare)
    assert "vendor_comet" not in tomllib.loads(result.toml)["gate"]
    assert any("quality not measured" in w for w in result.warnings)


@pytest.mark.parametrize("pair", ["fr", "en:en", "de:fr", "en-de", "en:de:fr"])
def test_bad_pairs_are_refused(pair):
    with pytest.raises(ps.ScaffoldError):
        make(pair)


def test_an_unknown_pair_is_refused():
    with pytest.raises(ps.ScaffoldError, match="no complete"):
        make("en:fr")


def test_restricted_and_incomplete_versions_are_ignored():
    recs = [dict(r, filter_expression="env.channel == 'nightly'") for r in RECORDS]
    assert ps.pair_versions(recs, "en", "de") == {}
    no_lex = [r for r in RECORDS if r["fileType"] != "lex"]
    assert ps.pair_versions(no_lex, "en", "de") == {}


def test_every_language_a_scaffold_can_meet_has_a_name():
    codes = set(fleurs_codes.MOZILLA_TO_FLEURS) | set(fleurs_codes.NO_EVAL)
    assert codes <= set(ps.LANGUAGE_NAMES)


def test_write_refuses_to_clobber_and_force_replaces(tmp_path):
    result = make("en:de")
    paths = ps.write(result, tmp_path)
    assert {p.name for p in paths} == {
        "translate-en-de.toml",
        "translate-en-de.measured.json",
        "config.yml",
    }
    with pytest.raises(ps.ScaffoldError, match="--force"):
        ps.write(result, tmp_path)
    ps.write(result, tmp_path, force=True)


def run_cli(tmp_path, monkeypatch, *argv):
    records = tmp_path / "records.json"
    records.write_text(json.dumps({"data": RECORDS}))
    monkeypatch.setattr(
        ps, "fetch_json", lambda url: metadata(*url.split("/models/")[1].split("/")[:2])
    )
    parser = argparse.ArgumentParser()
    ps.add_arguments(parser)
    args = parser.parse_args(
        ["--records", str(records), "--snap-dir", str(tmp_path / "snap"), *argv]
    )
    ps.cmd_plugin_scaffold(args)


def test_cli_writes_into_the_scratch_snap_dir(tmp_path, monkeypatch, capsys):
    run_cli(tmp_path, monkeypatch, "--pair", "en:de")
    out = capsys.readouterr()
    assert (tmp_path / "snap/plugins/translate-en-de.toml").is_file()
    assert "bergamot-base-memory-en-de" in out.err


def test_cli_exits_2_with_a_message_on_refusal(tmp_path, monkeypatch, capsys):
    with pytest.raises(SystemExit) as exit_info:
        run_cli(tmp_path, monkeypatch, "--pair", "en:fr")
    assert exit_info.value.code == 2
    assert "error:" in capsys.readouterr().err


def test_cli_treats_a_404_as_unpublished(tmp_path, monkeypatch):
    import urllib.error

    def fetch(url):
        if "/tiny/" in url:
            return metadata("tiny", "enfa")
        raise urllib.error.HTTPError(url, 404, "nf", None, None)

    records = tmp_path / "r.json"
    records.write_text(json.dumps({"data": RECORDS}))
    monkeypatch.setattr(ps, "fetch_json", fetch)
    parser = argparse.ArgumentParser()
    ps.add_arguments(parser)
    args = parser.parse_args(
        ["--records", str(records), "--snap-dir", str(tmp_path / "s"), "--pair", "en:fa"]
    )
    ps.cmd_plugin_scaffold(args)
    assert (tmp_path / "s/plugins/translate-en-fa.toml").is_file()


def test_cli_does_not_swallow_other_network_errors(tmp_path, monkeypatch, capsys):
    def fetch(url):
        raise OSError("offline")

    records = tmp_path / "r.json"
    records.write_text(json.dumps({"data": RECORDS}))
    monkeypatch.setattr(ps, "fetch_json", fetch)
    parser = argparse.ArgumentParser()
    ps.add_arguments(parser)
    args = parser.parse_args(["--records", str(records), "--pair", "en:de"])
    with pytest.raises(SystemExit) as exit_info:
        ps.cmd_plugin_scaffold(args)
    assert exit_info.value.code == 2
    assert "offline" in capsys.readouterr().err


def test_fetch_json_reads_the_reply(monkeypatch):
    import io

    seen = {}

    def urlopen(url, timeout):
        seen["url"], seen["timeout"] = url, timeout
        return io.BytesIO(b'{"data": [1]}')

    monkeypatch.setattr(ps.urllib.request, "urlopen", urlopen)
    assert ps.fetch_json("https://example.test/x") == {"data": [1]}
    assert seen == {"url": "https://example.test/x", "timeout": 60}


def test_an_unexpected_model_file_name_is_refused():
    with pytest.raises(ps.ScaffoldError, match="unexpected model file name"):
        ps.pair_dir("weights.bin")


def test_a_language_without_a_name_is_refused():
    with pytest.raises(ps.ScaffoldError, match="no language name"):
        make("en:xx")


def test_cli_fetches_the_records_when_none_are_saved(tmp_path, monkeypatch):
    def fetch(url):
        return {"data": RECORDS} if url == ps.RECORDS_URL else metadata("base-memory", "ende")

    monkeypatch.setattr(ps, "fetch_json", fetch)
    parser = argparse.ArgumentParser()
    ps.add_arguments(parser)
    ps.cmd_plugin_scaffold(parser.parse_args(["--pair", "en:de", "--snap-dir", str(tmp_path)]))
    assert (tmp_path / "plugins/translate-en-de.toml").is_file()
