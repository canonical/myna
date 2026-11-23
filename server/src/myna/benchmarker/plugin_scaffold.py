"""``myna-bench plugin-scaffold``: write a translation plugin from Mozilla's records.

Reads the Remote Settings ``translations-models`` records (file names, urls,
sha256, sizes) and the model's ``metadata.json`` from
``mozilla/firefox-translations-models`` (the vendor FLORES COMET), and writes
``plugins/translate-<pair>.toml``, its ``.measured.json`` seed and
``models/translate-<pair>/config.yml``. ``spot_check`` is left to a human and
``download_bytes`` is the sum of the file sizes until the component is packed.
Run it on chug; nothing here is part of a shipped snap.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from myna.benchmarker import fleurs_codes

RECORDS_URL = (
    "https://firefox.settings.services.mozilla.com/v1/buckets/main/collections/"
    "translations-models/records"
)
ATTACHMENTS_URL = "https://firefox-settings-attachments.cdn.mozilla.net/"
METADATA_URL = (
    "https://raw.githubusercontent.com/mozilla/firefox-translations-models/main/"
    "models/{arch}/{pair}/metadata.json"
)
ARCHITECTURES = ("base-memory", "tiny")
VERSION_RE = re.compile(r"\d+(\.\d+)*")
PAIR_RE = re.compile(r"([a-z]{2,3}(?:-[A-Za-z]+)?):([a-z]{2,3}(?:-[A-Za-z]+)?)")

LANGUAGE_NAMES = {
    "af": "Afrikaans",
    "ar": "Arabic",
    "az": "Azerbaijani",
    "be": "Belarusian",
    "bg": "Bulgarian",
    "bn": "Bengali",
    "bs": "Bosnian",
    "ca": "Catalan",
    "cs": "Czech",
    "da": "Danish",
    "de": "German",
    "el": "Greek",
    "en": "English",
    "es": "Spanish",
    "et": "Estonian",
    "eu": "Basque",
    "fa": "Persian",
    "fi": "Finnish",
    "fr": "French",
    "gl": "Galician",
    "gu": "Gujarati",
    "he": "Hebrew",
    "hi": "Hindi",
    "hr": "Croatian",
    "hu": "Hungarian",
    "id": "Indonesian",
    "is": "Icelandic",
    "it": "Italian",
    "ja": "Japanese",
    "kn": "Kannada",
    "ko": "Korean",
    "lt": "Lithuanian",
    "lv": "Latvian",
    "ml": "Malayalam",
    "mr": "Marathi",
    "ms": "Malay",
    "mt": "Maltese",
    "nb": "Norwegian Bokmal",
    "nl": "Dutch",
    "nn": "Norwegian Nynorsk",
    "pl": "Polish",
    "pt": "Portuguese",
    "ro": "Romanian",
    "ru": "Russian",
    "sk": "Slovak",
    "sl": "Slovenian",
    "sq": "Albanian",
    "sr": "Serbian",
    "sv": "Swedish",
    "ta": "Tamil",
    "te": "Telugu",
    "th": "Thai",
    "tl": "Filipino",
    "tr": "Turkish",
    "ug": "Uyghur",
    "uk": "Ukrainian",
    "ur": "Urdu",
    "vi": "Vietnamese",
    "zh-Hans": "Simplified Chinese",
    "zh-Hant": "Traditional Chinese",
}

CONFIG = """\
# Paths are relative to this file. Beam 1: both Firefox students lose chrF at
# beam 4 (tiny collapses), and beam 1 is the fastest.
models: [{model}]
vocabs: [{src_vocab}, {trg_vocab}]
shortlist: [{lex}, false]
beam-size: 1
normalize: 1.0
word-penalty: 0
max-length-break: 128
mini-batch-words: 1024
workspace: 128
max-length-factor: 2.0
skip-cost: true
cpu-threads: 1
quiet: true
quiet-translation: true
gemm-precision: int8shiftAlphaAll
"""

SEED_MEASURED = {
    "latency_ms": 25,
    "quality": 0.5,
    "quality_source": "pending",
    "gate": "fail",
}


class ScaffoldError(Exception):
    """The pair cannot be scaffolded as asked."""


@dataclass
class Scaffold:
    name: str
    toml: str
    measured: str
    config: str
    warnings: list[str] = field(default_factory=list)


def fetch_json(url: str) -> Any:
    with urllib.request.urlopen(url, timeout=60) as reply:  # noqa: S310 (https constants)
        return json.load(reply)


def parse_pair(text: str) -> tuple[str, str]:
    found = PAIR_RE.fullmatch(text)
    if not found:
        raise ScaffoldError(f"--pair {text!r}: expected <from>:<to>, e.g. en:fa")
    src, dst = found.groups()
    if "en" not in (src, dst) or src == dst:
        raise ScaffoldError(f"--pair {text!r}: one side must be en (other pairs pivot through it)")
    return src, dst


def _version_key(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("."))


def pair_versions(records: list[dict[str, Any]], src: str, dst: str) -> dict[str, dict[str, Any]]:
    """Complete, unrestricted versions of a pair: {version: {fileType: record}}."""
    versions: dict[str, dict[str, dict[str, Any]]] = {}
    for rec in records:
        if rec.get("fromLang") != src or rec.get("toLang") != dst or rec.get("filter_expression"):
            continue
        version = rec.get("version", "")
        if VERSION_RE.fullmatch(version):
            versions.setdefault(version, {})[rec["fileType"]] = rec
    complete = {}
    for version, files in versions.items():
        vocabs = {"vocab"} <= files.keys() or {"srcvocab", "trgvocab"} <= files.keys()
        if {"model", "lex"} <= files.keys() and vocabs:
            complete[version] = files
    return complete


# The architecture named for a record no metadata.json describes: Mozilla's
# newer models are served from Remote Settings without one in the repo.
UNDESCRIBED = "mozilla"


def _matches(meta: dict[str, Any] | None, model: dict[str, Any]) -> bool:
    return (
        meta is not None
        and meta.get("hash", model["hash"]) == model["hash"]
        and meta.get("byteSize", model["size"]) == model["size"]
    )


def pick_version(
    versions: dict[str, dict[str, Any]],
    metadata_for: Callable[[str], dict[str, Any] | None],
    archs: tuple[str, ...],
    *,
    forced: bool = False,
) -> tuple[str, str, dict[str, Any], dict[str, Any]]:
    """(arch, version, files, metadata) of the version to ship.

    The newest version is what Firefox serves today, so it wins; its
    architecture and vendor COMET come from the first metadata.json that
    describes its model, else it is ``UNDESCRIBED`` with no metadata. A forced
    architecture takes the newest version of that architecture instead."""
    metas = {arch: metadata_for(arch) for arch in archs}
    ordered = sorted(versions, key=_version_key, reverse=True)
    if not forced:
        newest = ordered[0]
        model = versions[newest]["model"]["attachment"]
        for arch, meta in metas.items():
            if _matches(meta, model):
                return arch, newest, versions[newest], meta or {}
        return UNDESCRIBED, newest, versions[newest], {}
    for arch, meta in metas.items():
        for version in ordered:
            if _matches(meta, versions[version]["model"]["attachment"]):
                return arch, version, versions[version], meta or {}
    raise ScaffoldError(
        f"no record version matches the metadata.json of {', '.join(archs)}: "
        "the pair is not published in those architectures"
    )


def _file_entries(files: dict[str, Any]) -> list[dict[str, Any]]:
    order = ["model", "lex", "vocab", "srcvocab", "trgvocab"]
    entries = []
    for kind in order:
        rec = files.get(kind)
        if rec is None:
            continue
        att = rec["attachment"]
        entries.append(
            {
                "name": att["filename"],
                "sha256": att["hash"],
                "bytes": att["size"],
                "url": ATTACHMENTS_URL + att["location"],
            }
        )
    return entries


def _toml_str(text: str) -> str:
    return json.dumps(text, ensure_ascii=False)


def plugin_name(src: str, dst: str) -> str:
    """The plugin's file stem, and so its component: snapd names components in
    lower case, so ``zh-Hans`` is ``zh-hans`` there (the language tags in the
    file keep their case)."""
    return f"translate-{src}-{dst}".lower()


def render_toml(
    src: str,
    dst: str,
    arch: str,
    version: str,
    entries: list[dict[str, Any]],
    comet: float | None,
) -> str:
    pair = f"{src}-{dst}"
    slug = plugin_name(src, dst).removeprefix("translate-")
    from_name, to_name = LANGUAGE_NAMES[src], LANGUAGE_NAMES[dst]
    total = sum(e["bytes"] for e in entries)
    mb = round(total / 1e6)
    known = arch != UNDESCRIBED
    release = f"{arch} {version}" if known else f"version {version}"
    flavour = f" {arch}" if known else ""
    model_named = f'"{arch}" (version {version})' if known else f"(version {version})"
    head = f"""\
# One plugin: dev/gen-plugins.py turns this into the snapcraft component,
# models/translate-{slug}/{{myna-text-model.json,NOTICE}} and catalog.json.
# config.yml stays hand-written. Contract: .kb/text-plugin-contract.md.
# Scaffolded by myna-bench plugin-scaffold: download_bytes is the file sum
# until the component is packed, spot_check is for a native speaker to add.
id = "bergamot-{arch}-{slug}"
component = "model-translate-{slug}"
snap = "myna-translation"
kind = "translate"
task = "text.translate"
class = "translate"
needs = "dictation"
language = "{src}"
to = "{dst}"
config = "config.yml"
license = "MPL-2.0"
attribution = "Mozilla and contributors, Firefox Translations ({release})"
source = "https://github.com/mozilla/firefox-translations-models"
download_bytes = {total}
entry_revision = 1
curated = true
pin = "mozilla firefox-translations {pair} {arch} {version}"
locales = ["{src}"]
summary = "{from_name} to {to_name} translation (Firefox Translations{flavour})"
description = \"\"\"\\
Mozilla's Firefox Translations {from_name} to {to_name} student model
(MPL-2.0), run by bergamot-translator. About {mb} MB.\"\"\"
notice = \"\"\"\\
model-translate-{slug}: the Firefox Translations {from_name} to {to_name} student
model {model_named}, Copyright Mozilla and contributors,
licensed under the Mozilla Public License 2.0 (LICENSE in this component).

Source: Mozilla's Firefox Translations models,
https://github.com/mozilla/firefox-translations-models (the files themselves
are served from the Remote Settings collection "translations-models" of
https://firefox.settings.services.mozilla.com). The model files are shipped
unmodified; their sha256 are:\"\"\"
notice_tail = \"\"\"\\
config.yml and myna-text-model.json are Canonical's own (AGPL-3.0-or-later).
The runtime that executes the model, bergamot-translator (MPL-2.0), and its
bundled libraries are noted in the myna-translation snap's NOTICE.\"\"\"

[[params]]
name = "language"
kind = "language"
role = "source"
required = true

[[params]]
name = "to"
kind = "language"
role = "target"
required = true

"""
    if comet is None:
        gate = (
            "# DECISIONS 7.4: Mozilla published no FLORES COMET for this pair\n"
            "# (quality not measured): the 0.40 chrF floor and the spot check apply.\n"
            "[gate]\n"
        )
    else:
        gate = (
            "# DECISIONS 7.4: vendor COMET is Mozilla's FLORES figure from the model's\n"
            f"# metadata.json (models/{arch}/{_pair_dir(entries)}/metadata.json); "
            "no section 5 floor.\n"
            f"[gate]\nvendor_comet = {comet}\n"
        )
    tail = '\n[step]\nid = "translate"\nstage = "translate"\n\n'
    tail += '[hardware]\narch = ["amd64"]\nmin_ram_mb = 512\n'
    files = "".join(
        f'\n[[files]]\nname = {_toml_str(e["name"])}\nsha256 = "{e["sha256"]}"\n'
        f"bytes = {e['bytes']}\nurl = {_toml_str(e['url'])}\n"
        for e in entries
    )
    return head + gate + tail + files


def _pair_dir(entries: list[dict[str, Any]]) -> str:
    return pair_dir(entries[0]["name"])


def pair_dir(model_name: str) -> str:
    found = re.fullmatch(r"model\.(\w+)\.intgemm.*", model_name)
    if not found:
        raise ScaffoldError(f"unexpected model file name {model_name!r}")
    return found.group(1)


def render_config(entries: list[dict[str, Any]]) -> str:
    names = {}
    for entry in entries:
        kind = entry["name"].split(".")[0]
        names[kind] = entry["name"]
    vocab = names.get("vocab")
    return CONFIG.format(
        model=names["model"],
        src_vocab=names.get("srcvocab", vocab),
        trg_vocab=names.get("trgvocab", vocab),
        lex=names["lex"],
    )


def scaffold(
    pair: str,
    *,
    records: list[dict[str, Any]],
    metadata: Callable[[str, str], dict[str, Any] | None],
    arch: str | None = None,
    no_eval: bool = False,
) -> Scaffold:
    src, dst = parse_pair(pair)
    warnings: list[str] = []
    other = dst if src == "en" else src
    for lang in (src, dst):
        if lang not in LANGUAGE_NAMES:
            raise ScaffoldError(f"{lang}: no language name; add it to LANGUAGE_NAMES")
    if not fleurs_codes.evaluable(other):
        if not no_eval:
            raise ScaffoldError(
                f"{other}: no FLEURS locale (fleurs_codes), so chrF and latency cannot be "
                "measured on a corpus slice; pass --no-eval to scaffold it anyway"
            )
        warnings.append(
            f"{other} has no FLEURS locale: the gate will lack chrF, latency and the "
            "degeneracy checks; only the vendor COMET and the spot check remain"
        )
    versions = pair_versions(records, src, dst)
    if not versions:
        raise ScaffoldError(f"{src}->{dst}: no complete unrestricted record in translations-models")
    any_files = next(iter(versions.values()))
    directory = pair_dir(any_files["model"]["attachment"]["filename"])
    archs = (arch,) if arch else ARCHITECTURES
    chosen_arch, version, files, meta = pick_version(
        versions, lambda a: metadata(a, directory), archs, forced=arch is not None
    )
    entries = _file_entries(files)
    comet = (meta.get("flores") or {}).get("comet")
    if comet is None:
        warnings.append("metadata.json has no flores.comet: labelled quality not measured")
    if chosen_arch == UNDESCRIBED:
        warnings.append(
            f"no metadata.json describes the newest model (version {version}): "
            "architecture unknown, no vendor COMET"
        )
    elif chosen_arch != ARCHITECTURES[0]:
        warnings.append(f"{ARCHITECTURES[0]} is not published for this pair: using {chosen_arch}")
    text = render_toml(src, dst, chosen_arch, version, entries, comet)
    return Scaffold(
        name=plugin_name(src, dst),
        toml=text,
        measured=json.dumps(SEED_MEASURED, indent=2) + "\n",
        config=render_config(entries),
        warnings=warnings,
    )


def write(result: Scaffold, snap_dir: Path, *, force: bool = False) -> list[Path]:
    targets = {
        snap_dir / "plugins" / f"{result.name}.toml": result.toml,
        snap_dir / "plugins" / f"{result.name}.measured.json": result.measured,
        snap_dir / "models" / result.name / "config.yml": result.config,
    }
    clash = [p for p in targets if p.exists()]
    if clash and not force:
        raise ScaffoldError(f"{clash[0]} exists; pass --force to replace it")
    for path, text in targets.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return list(targets)


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--pair", required=True, help="<from>:<to> with one side en, e.g. en:fa")
    parser.add_argument(
        "--arch",
        choices=ARCHITECTURES,
        help="model architecture (default: base-memory, else tiny)",
    )
    parser.add_argument(
        "--no-eval",
        action="store_true",
        help="allow a language FLEURS has no locale for (gate evidence will be missing)",
    )
    parser.add_argument(
        "--snap-dir",
        default="translation-snap",
        help="where plugins/ and models/ are written (default translation-snap)",
    )
    parser.add_argument("--force", action="store_true", help="replace existing files")
    parser.add_argument("--records", help="a saved records JSON instead of the network")


def cmd_plugin_scaffold(args: argparse.Namespace) -> None:
    def metadata(arch: str, directory: str) -> dict[str, Any] | None:
        try:
            found: dict[str, Any] = fetch_json(METADATA_URL.format(arch=arch, pair=directory))
            return found
        except OSError as err:
            if getattr(err, "code", None) == 404:
                return None
            raise

    try:
        if args.records:
            records = json.loads(Path(args.records).read_text(encoding="utf-8"))["data"]
        else:
            records = fetch_json(RECORDS_URL)["data"]
        result = scaffold(
            args.pair, records=records, metadata=metadata, arch=args.arch, no_eval=args.no_eval
        )
        paths = write(result, Path(args.snap_dir), force=args.force)
    except (ScaffoldError, OSError, KeyError, ValueError) as err:
        print(f"error: {err}", file=sys.stderr)
        sys.exit(2)
    for warning in result.warnings:
        print(f"warning: {warning}", file=sys.stderr)
    for path in paths:
        print(path)
    print(
        f"next: pack the component, set download_bytes, then "
        f"dev/plugin-pipeline.sh {_plugin_id(result.toml)}",
        file=sys.stderr,
    )


def _plugin_id(text: str) -> str:
    found = re.search(r'^id = "([^"]+)"', text, re.M)
    return found.group(1) if found else "<id>"
