#!/usr/bin/python3 -I
"""Build a self-contained HTML contact sheet of the e2e screenshots.

    contact-sheet.py --current DIR [--baseline DIR] --out FILE.html [--title T]

DIR holds `<suite>/<NN>-<checkpoint>.png` (artifacts/screenshots of a run).
Each checkpoint is shown current beside baseline, images embedded as data
URIs so the page is one file. A checkpoint on one side only is flagged
(NEW or MISSING), as is one whose bytes are identical (same) or differ
(changed). Pixels are never asserted: this is a page for a human, and the
suites assert machine state. A missing or empty baseline dir is the first run:
every checkpoint is NEW.

Stdlib only; run with `python3 -I`.
"""

import argparse
import base64
import hashlib
import html
import sys
from pathlib import Path


def checkpoints(root):
    """Map `suite/NN-name` to the PNG's path under `root`."""
    if root is None or not Path(root).is_dir():
        return {}
    root = Path(root)
    return {p.relative_to(root).with_suffix("").as_posix(): p for p in sorted(root.glob("*/*.png"))}


def classify(current, baseline):
    """new / missing / same / changed for one checkpoint's two files."""
    if current is None:
        return "missing"
    if baseline is None:
        return "new"
    same = (
        hashlib.sha256(current.read_bytes()).digest()
        == hashlib.sha256(baseline.read_bytes()).digest()
    )
    return "same" if same else "changed"


def compare(current_dir, baseline_dir):
    """Rows `(name, status, current_path, baseline_path)` in checkpoint order."""
    now, then = checkpoints(current_dir), checkpoints(baseline_dir)
    return [
        (name, classify(now.get(name), then.get(name)), now.get(name), then.get(name))
        for name in sorted(now.keys() | then.keys())
    ]


def image(path):
    if path is None:
        return '<div class="none">no screenshot</div>'
    data = base64.b64encode(path.read_bytes()).decode()
    return f'<img loading="lazy" src="data:image/png;base64,{data}" alt="{path.name}">'


STYLE = """
:root { color-scheme: light dark; --bg: #fff; --fg: #1d1d1d; --line: #ccc;
  --new: #0a6; --missing: #c30; --changed: #a60; --same: #777; }
@media (prefers-color-scheme: dark) { :root { --bg: #1b1b1b; --fg: #eee; --line: #444; } }
body { background: var(--bg); color: var(--fg); font: 14px/1.4 system-ui, sans-serif;
  margin: 16px; }
h1 { font-size: 20px; }
h2 { font-size: 16px; margin-top: 28px; border-bottom: 1px solid var(--line); }
.row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin: 8px 0 20px; }
.row img, .none { width: 100%; border: 1px solid var(--line); }
.none { padding: 40px 0; text-align: center; color: var(--missing); }
.badge { font-size: 12px; padding: 1px 6px; border-radius: 3px; color: #fff; }
.new { background: var(--new); } .missing { background: var(--missing); }
.changed { background: var(--changed); } .same { background: var(--same); }
@media (max-width: 700px) { .row { grid-template-columns: 1fr; } }
"""


def render(rows, title, has_baseline):
    counts = {k: sum(1 for r in rows if r[1] == k) for k in ("new", "missing", "changed", "same")}
    parts = [
        "<!doctype html><html lang=en><meta charset=utf-8>",
        "<meta name=viewport content='width=device-width,initial-scale=1'>",
        f"<title>{html.escape(title)}</title><style>{STYLE}</style><h1>{html.escape(title)}</h1>",
    ]
    if not has_baseline:
        parts.append("<p>No baseline yet: every checkpoint is new.</p>")
    summary = ", ".join(f"{n} {k}" for k, n in counts.items() if n)
    summary = html.escape(summary or "no checkpoints")
    parts.append(f"<p>{summary}. Left: this run. Right: the last approved run.</p>")
    for name, status, now, then in rows:
        label = html.escape(name)
        parts.append(f'<h2 id="{label}"><span class="badge {status}">{status}</span> {label}</h2>')
        parts.append(f'<div class="row"><div>{image(now)}</div><div>{image(then)}</div></div>')
    return "\n".join(parts)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--current", required=True)
    parser.add_argument("--baseline")
    parser.add_argument("--out", required=True)
    parser.add_argument("--title", default="Myna e2e screenshots")
    args = parser.parse_args(argv)
    rows = compare(args.current, args.baseline)
    Path(args.out).write_text(render(rows, args.title, bool(checkpoints(args.baseline))))
    bad = [r[0] for r in rows if r[1] in ("missing", "changed")]
    print(f"{len(rows)} checkpoints, {len(bad)} changed or missing -> {args.out}")


if __name__ == "__main__":
    main(sys.argv[1:])
