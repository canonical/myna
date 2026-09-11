#!/usr/bin/python3
"""Emit debian/copyright: the header from copyright.in, one Files stanza per
vendored crate from its Cargo.toml (authors, license), then one standalone
License stanza per licence that any crate's expression names. Deterministic
for a given vendor tree."""
import re
import sys
import tomllib
from pathlib import Path

vendor = Path(sys.argv[1])
header = Path(sys.argv[2]).read_text()

COMMON = {
    "Apache-2.0": "Apache-2.0",
    "GPL-3.0-only": "GPL-3",
    "MPL-2.0": "MPL-2.0",
    "BSD-3-Clause": "BSD-3-Clause",
    "CC0-1.0": "CC0-1.0",
}
# License file names to look for, per licence atom, when the text has to be
# embedded. First match wins.
FILES = {
    "MIT": ["LICENSE-MIT", "LICENSE-MIT.md", "LICENSE", "LICENSE.txt", "LICENSE.md"],
    "Apache-2.0 with LLVM exception": ["LICENSE-APACHE", "LICENSE"],
    "Unlicense": ["UNLICENSE", "LICENSE-UNLICENSE", "LICENSE"],
    "Unicode-3.0": ["LICENSE-UNICODE", "LICENSE"],
    "Zlib": ["LICENSE-ZLIB", "LICENSE"],
    "ISC": ["LICENSE-ISC", "LICENSE"],
    "0BSD": ["LICENSE-0BSD", "LICENSE"],
    "BSL-1.0": ["LICENSE-BOOST", "LICENSE"],
}


def dep5_expression(spdx: str) -> str:
    """`MIT/Apache-2.0` and `A OR B` in Cargo become `A or B` in DEP-5, and an
    SPDX `WITH LLVM-exception` becomes DEP-5's `with LLVM exception`."""
    spdx = spdx.replace("/", " OR ")
    spdx = re.sub(r"\s+OR\s+", " or ", spdx)
    spdx = re.sub(r"\s+AND\s+", " and ", spdx)
    spdx = re.sub(r"\s+WITH\s+(\S+)-exception", r" with \1 exception", spdx)
    return spdx


def atoms(expression: str) -> list[str]:
    return [a.strip("() ") for a in re.split(r"\s+(?:or|and)\s+", expression)]


stanzas = []
first_crate = {}
for crate in sorted(vendor.iterdir()):
    meta = tomllib.loads((crate / "Cargo.toml").read_text())["package"]
    spdx = meta.get("license")
    if not spdx:
        raise SystemExit(f"{crate}: no license expression in Cargo.toml")
    expression = dep5_expression(spdx)
    authors = meta.get("authors") or [f"The {meta['name']} developers"]
    stanzas.append(
        f"Files: vendor/{crate.name}/*\n"
        f"Copyright: {' / '.join(authors)}\n"
        f"License: {expression}\n"
    )
    for atom in atoms(expression):
        first_crate.setdefault(atom, []).append(crate)

# Licences the hand-written header already spells out get no second stanza.
defined = set(re.findall(r"^License: (.+)$", header, re.MULTILINE))
out = [header.rstrip("\n"), ""]
out.extend(stanzas)
for atom in sorted(set(first_crate) - defined):
    out.append(f"License: {atom}")
    base, _, exception = atom.partition(" with ")
    if atom in COMMON:
        out.append(f" On Debian systems the full text of the {atom} licence is in\n /usr/share/common-licenses/{COMMON[atom]}.")
    elif exception and base in COMMON:
        # Reference the base text, embed only the exception paragraph: lintian
        # rejects a copy of the full Apache-2.0 text in debian/copyright.
        crate = first_crate[atom][0]
        text = (crate / "LICENSE").read_text(errors="replace").splitlines()
        start = next(i for i, l in enumerate(text) if "exception" in l.lower() and l.startswith("---"))
        out.append(f" On Debian systems the full text of the {base} licence is in\n /usr/share/common-licenses/{COMMON[base]}.\n .")
        out.append("\n".join(" " + (l.rstrip() or ".") for l in text[start:]))
    else:
        for crate in first_crate[atom]:
            path = next((crate / n for n in FILES.get(atom, ["LICENSE"]) if (crate / n).exists()), None)
            if path:
                break
        else:
            raise SystemExit(f"no licence file found for {atom} in {[c.name for c in first_crate[atom]]}")
        out.append("\n".join(" " + (l.rstrip() or ".") for l in path.read_text(errors="replace").splitlines()))
    out.append("")
print("\n".join(out))
