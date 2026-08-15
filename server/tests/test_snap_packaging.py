"""Packaging invariants across the inference snaps.

These are the checks that would have caught, in seconds and without a VM, two
bugs that instead surfaced as a benchmark run dying halfway through:

- parakeet and sherpa shipped no ``pciutils``, so ``modelctl use-engine`` died
  with "executing lspci: executable file not found in $PATH", leaving the snap
  with no active engine and a daemon that exited 1 on every start;
- their CLI app and install hook declared no ``hardware-observe`` plug, so even
  with lspci present the hardware detection behind ``show-engine`` could not
  read ``/sys``.

Both were invisible to the existing suites: the unit tests never look at
packaging, and the spread e2e ran only the fake backend. A snapcraft.yaml is
data, so assert against it directly - the cheapest possible place to notice
that one snap has drifted from its siblings.

Scope: the *inference* snaps (the ones exposing modelctl + the UbuSTT socket).
The client and the fake backend are deliberately excluded; they have neither.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parents[2]

# dir -> snap name. Kept explicit rather than globbed: a new inference snap
# should be a deliberate addition here, not something that silently opts out.
INFERENCE_SNAPS = {
    "whisper-snap": "whisper",
    "parakeet-snap": "parakeet",
    "sherpa-snap": "sherpa",
    "funasr-snap": "myna-funasr",
    "qwen-snap": "qwen",
    "nemotron-snap": "nemotron",
}


def _recipe(snap_dir: str) -> dict:
    path = REPO_ROOT / snap_dir / "snap" / "snapcraft.yaml"
    return yaml.safe_load(path.read_text(encoding="utf-8"))


def _cli_app(recipe: dict) -> tuple[str, dict]:
    """The modelctl app: the one that is not the daemon."""
    for name, app in (recipe.get("apps") or {}).items():
        if isinstance(app, dict) and "daemon" not in app:
            return name, app
    raise AssertionError(f"{recipe.get('name')}: no non-daemon app")


@pytest.fixture(params=sorted(INFERENCE_SNAPS), ids=sorted(INFERENCE_SNAPS))
def snap(request) -> tuple[str, str, dict]:
    snap_dir = request.param
    return snap_dir, INFERENCE_SNAPS[snap_dir], _recipe(snap_dir)


def test_name_matches_the_directory_mapping(snap) -> None:
    snap_dir, expected, recipe = snap
    assert recipe["name"] == expected, (
        f"{snap_dir} builds {recipe['name']!r}; the benchmark runner and spread "
        f"tasks resolve artifacts by name and would look for {expected!r}"
    )


def test_ships_lspci_for_hardware_detection(snap) -> None:
    """modelctl shells out to lspci; without it the CLI is dead on arrival."""
    snap_dir, name, recipe = snap
    parts = recipe.get("parts") or {}
    staged = {pkg for part in parts.values() for pkg in (part.get("stage-packages") or [])}
    assert "pciutils" in staged, (
        f"{name}: no part stages pciutils, so `{name} use-engine` fails with "
        '"executing lspci: executable file not found in $PATH"'
    )


def test_cli_app_can_read_hardware(snap) -> None:
    _, name, recipe = snap
    app_name, app = _cli_app(recipe)
    plugs = set(app.get("plugs") or [])
    assert "hardware-observe" in plugs, (
        f"{name}: app {app_name!r} does not plug hardware-observe, so modelctl's "
        "hardware detection cannot read /sys even with lspci staged"
    )


def test_install_hook_can_read_hardware(snap) -> None:
    """The hook selects the engine at install; it needs the same access."""
    _, name, recipe = snap
    hook = (recipe.get("hooks") or {}).get("install")
    assert isinstance(hook, dict), f"{name}: no install hook declared in snapcraft.yaml"
    assert "hardware-observe" in set(hook.get("plugs") or []), (
        f"{name}: the install hook does not plug hardware-observe, so engine "
        "selection at install time cannot detect the machine"
    )


def test_install_hook_activates_an_engine(snap) -> None:
    """A snap with no active engine has a daemon that exits 1 on every start.

    Either form is fine: `--auto` where there is a real choice, or an explicit
    name where the snap ships exactly one engine (auto would demand hardware
    scoring to answer a question with one possible answer).
    """
    snap_dir, name, _ = snap
    hook = (REPO_ROOT / snap_dir / "snap" / "hooks" / "install").read_text(encoding="utf-8")
    assert "use-engine" in hook, (
        f"{name}: the install hook never runs `modelctl use-engine`, so a fresh "
        "install has no active engine and `show-engine`/`status` fail"
    )


def test_declares_every_engine_it_ships(snap) -> None:
    """Every engines/<name>/ dir needs its server script, and vice versa."""
    snap_dir, name, _ = snap
    engines_dir = REPO_ROOT / snap_dir / "engines"
    assert engines_dir.is_dir(), f"{name}: no engines/ directory"
    engines = [p for p in engines_dir.iterdir() if p.is_dir()]
    assert engines, f"{name}: engines/ is empty"
    for engine in engines:
        assert (engine / "server").is_file(), f"{name}: engines/{engine.name}/server missing"
        assert (engine / "engine.yaml").is_file(), (
            f"{name}: engines/{engine.name}/engine.yaml missing"
        )


def test_hooks_dir_holds_only_hooks(snap) -> None:
    """snap/hooks/ is a namespace snapd owns, not a scratch directory.

    sherpa-snap accumulated a copy of dev/prepare.sh there. snapd ignores an
    unrecognised name, so nothing broke - it just sat looking like a hook, with
    a $SNAP-relative path computation that would have been wrong if anything
    ever ran it.
    """
    snap_dir, name, _ = snap
    known = {"install", "configure", "post-refresh", "pre-refresh", "remove", "connect-plug"}
    hooks = REPO_ROOT / snap_dir / "snap" / "hooks"
    if not hooks.is_dir():
        return
    stray = [
        p.name
        for p in hooks.iterdir()
        if p.is_file() and not any(p.name == k or p.name.startswith(f"{k}-") for k in known)
    ]
    assert not stray, f"{name}: snap/hooks/ contains non-hook files: {stray}"


def test_exposes_the_session_socket(snap) -> None:
    """Confined clients reach the backend over the ubustt-socket content share."""
    _, name, recipe = snap
    slot = (recipe.get("slots") or {}).get("ubustt-socket")
    assert isinstance(slot, dict), f"{name}: no ubustt-socket slot; confined clients cannot connect"
    assert slot.get("content") == "ubustt-socket", (
        f"{name}: ubustt-socket slot has the wrong content"
    )


def test_streaming_toggle_is_a_config_key_not_a_hardcoded_flag(snap) -> None:
    """`--streaming` baked into an engine script is not a user-facing choice.

    Emission mode is a shipped configuration (`snap set <snap> streaming=`), so
    an engine script must read it rather than hardcode it. Snaps whose adapter
    is commit-on-finalize only never mention --streaming at all, which is also
    fine - the failure this catches is a script that forces the flag on.
    """
    snap_dir, name, _ = snap
    for server in sorted((REPO_ROOT / snap_dir / "engines").glob("*/server")):
        script = server.read_text(encoding="utf-8")
        if "--streaming" not in script:
            continue
        assert "stream_args" in script, (
            f"{name}: engines/{server.parent.name}/server hardcodes --streaming; "
            "read the `streaming` config key instead so both modes are measurable"
        )
