"""Packaging invariants across the inference snaps.

These are the checks that would have caught, in seconds and without a VM, two
bugs that instead surfaced as a benchmark run dying halfway through:

- parakeet and sherpa shipped no ``pciutils``, so ``modelctl use-engine`` died
  with "executing lspci: executable file not found in $PATH", leaving the snap
  with no active engine and a daemon that exited 1 on every start (fixed
  upstream in v2.0.0-beta.6: modelctl now uses lscompute and reads /sys
  directly, so staging pciutils is no longer needed - the assertion below now
  guards against re-adding it);
- their CLI app and install hook declared no ``hardware-observe`` plug, so even
  with hardware detection present the scoring behind ``show-engine`` could not
  read ``/sys``.

Both were invisible to the existing suites: the unit tests never look at
packaging, and the spread e2e ran only the fake backend. A snapcraft.yaml is
data, so assert against it directly - the cheapest possible place to notice
that one snap has drifted from its siblings.

Scope: the *inference* snaps (the ones exposing modelctl + the UbuSTT socket).
The client and the fake backend are deliberately excluded; they have neither.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parents[2]

# dir -> snap name. Kept explicit rather than globbed: a new inference snap
# should be a deliberate addition here, not something that silently opts out.
INFERENCE_SNAPS = {
    "whisper-snap": "myna-whisper",
    "parakeet-snap": "myna-parakeet",
    "sherpa-snap": "myna-sherpa",
    "funasr-snap": "myna-funasr",
    "qwen-snap": "myna-qwen",
    "nemotron-snap": "myna-nemotron",
    "audio8-snap": "myna-audio8",
}

# The inference-snaps-cli (modelctl) release every snap must pin. One version
# across all snaps: manifest semantics (runtime `name`, model identifiers,
# status entrypoints) move with the CLI, and a drifted snap breaks silently.
MODELCTL_RELEASE = "v2.0.0-beta.12"


def _recipe(snap_dir: str) -> dict:
    path = REPO_ROOT / snap_dir / "snap" / "snapcraft.yaml"
    return yaml.safe_load(path.read_text(encoding="utf-8"))


def _daemon_app(recipe: dict) -> tuple[str, dict]:
    """The server: the one app declaring `daemon`."""
    for name, app in (recipe.get("apps") or {}).items():
        if isinstance(app, dict) and "daemon" in app:
            return name, app
    raise AssertionError(f"{recipe.get('name')}: no daemon app")


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


def test_name_is_namespaced_after_the_directory(snap) -> None:
    """Every inference snap packs as ``myna-<adapter>``.

    The mapping above is hand-maintained, so on its own it would happily
    record a snap that dropped out of the namespace. Deriving the expected
    name from the directory makes the invariant the thing under test: the
    directory stays ``<adapter>-snap`` (it is where the adapter's sources
    live), the snap is namespaced for the store.
    """
    snap_dir, expected, _ = snap
    adapter = snap_dir.removesuffix("-snap")
    assert expected == f"myna-{adapter}", (
        f"{snap_dir} maps to {expected!r}, not 'myna-{adapter}' - inference "
        "snaps are namespaced myna-* so they do not collide in the store"
    )


def test_cli_app_is_named_after_the_adapter(snap) -> None:
    """modelctl answers on ``<snap>.<adapter>``, never on the bare snap name.

    snapd exposes an app under its bare snap name only when the two match,
    and namespacing means they never do. Spread's MODELCTL, the benchmark
    configs' `cli:`, and every README invocation are spelled that way; this
    is what stops a snap renaming its app and silently breaking all three.
    """
    snap_dir, name, recipe = snap
    app_name, _ = _cli_app(recipe)
    assert app_name == snap_dir.removesuffix("-snap"), (
        f"{name}: modelctl app is {app_name!r}, so its entry point is "
        f"{name}.{app_name} - callers expect {name}.{snap_dir.removesuffix('-snap')}"
    )


def test_pins_the_shared_modelctl_release(snap) -> None:
    """Every cli part pulls the same modelctl release tarball."""
    snap_dir, name, recipe = snap
    cli = (recipe.get("parts") or {}).get("cli") or {}
    sources = cli.get("source") or []
    urls = [s[f"on {arch}"] for s in sources for arch in ("amd64", "arm64") if f"on {arch}" in s]
    assert urls, f"{name}: cli part has no per-arch source URLs"
    for url in urls:
        assert f"/download/{MODELCTL_RELEASE}/" in url, (
            f"{name}: cli part does not pin {MODELCTL_RELEASE}: {url}"
        )


def test_no_lspci_leftover(snap) -> None:
    """modelctl >= v2.0.0-beta.6 uses lscompute (/sys), not the lspci binary.

    Staging pciutils is dead weight that looks load-bearing; the friendly
    device names it provided come from the pci.ids database, not the binary.
    """
    snap_dir, name, recipe = snap
    parts = recipe.get("parts") or {}
    staged = {pkg for part in parts.values() for pkg in (part.get("stage-packages") or [])}
    assert "pciutils" not in staged, (
        f"{name}: a part still stages pciutils, but modelctl no longer shells "
        "out to lspci - drop it"
    )


def test_cli_app_can_read_hardware(snap) -> None:
    _, name, recipe = snap
    app_name, app = _cli_app(recipe)
    plugs = set(app.get("plugs") or [])
    assert "hardware-observe" in plugs, (
        f"{name}: app {app_name!r} does not plug hardware-observe, so modelctl's "
        "hardware detection cannot read /sys"
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


def test_no_app_plugs_network(snap) -> None:
    """The offline invariant, asserted rather than only asserted in prose.

    Every one of these snaps states in its own header that weights ship as
    components and there is no runtime download. qwen and nemotron nonetheless
    plugged ``network`` on their CLI app, contradicting that text and their
    siblings, with no comment saying why. ``network-bind`` on the daemon is a
    different thing: snapd's seccomp gates listen() behind it even for a Unix
    socket, so it is required and is allowed here.
    """
    _, name, recipe = snap
    for app_name, app in (recipe.get("apps") or {}).items():
        if not isinstance(app, dict):
            continue
        assert "network" not in set(app.get("plugs") or []), (
            f"{name}: app {app_name!r} plugs `network`, but this snap ships its "
            "weights as components and disclaims runtime downloads"
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


def test_socket_config_key_is_ws_unix_socket(snap) -> None:
    """`modelctl status` reports the session socket only under `ws.unix-socket`.

    Since v2.0.0-beta.12 the entrypoint for a ws+unix runtime server is built
    from that config key; the pre-beta.12 `socket.path` was snap-private and
    invisible to status. The install hook must set the new key and engine
    scripts must read it.
    """
    snap_dir, name, _ = snap
    install = (REPO_ROOT / snap_dir / "snap" / "hooks" / "install").read_text(encoding="utf-8")
    assert "ws.unix-socket" in install, (
        f"{name}: the install hook does not set ws.unix-socket, so "
        "`modelctl status` cannot report the UbuSTT entrypoint"
    )
    for server in sorted((REPO_ROOT / snap_dir / "engines").glob("*/server")):
        script = server.read_text(encoding="utf-8")
        assert "socket.path" not in script, (
            f"{name}: engines/{server.parent.name}/server still reads the retired socket.path key"
        )


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


# Snaps whose adapter leaves ORT to size its own pool, so ORT also pins it
# (T65). Empty, and that is the finding rather than an oversight: every ORT
# adapter here measured faster with a small explicit pool than with ORT's own
# sizing, pinning included (parakeet ~2x, sherpa 4.9x, funasr 12%, audio8
# 16%), so none of them pins and none of them may plug process-control.
# whisper/nemotron/qwen are not ORT at all (CTranslate2, PyTorch, and a ctypes
# libqwen_asr.so) and never pinned either.
ORT_PINNING_SNAPS: set[str] = set()

# Adapters that cap ORT's intra-op pool, and so give up pinning. Kept as an
# explicit list because the cap is the load-bearing decision: each value was
# measured (T65), and a cap that appears here without one is how funasr and
# audio8 shipped 4 threads on every machine in the first place. Anything
# leaving this set needs process-control adding to its snap.
THREAD_CAPPED_ADAPTERS = {"parakeet.py", "funasr.py", "audio8.py"}


def test_pinning_daemons_plug_process_control(snap) -> None:
    """ORT can only pin its thread pool if seccomp's argument filter is lifted.

    The default snapd template allows `sched_setaffinity 0 - -` - a *literal*
    pid 0 - but glibc's pthread_setaffinity_np always passes the target's real
    tid, even when a thread pins itself. So every pin is refused with EPERM and
    the daemon logs a wall of "pthread_setaffinity_np failed ... Operation not
    permitted" on each model load, silently running unpinned. `process-control`
    drops the filter. Caught in the field on parakeet, 2026-08-24.
    """
    snap_dir, name, recipe = snap
    daemon_name, daemon = _daemon_app(recipe)
    plugs = set(daemon.get("plugs") or [])
    if snap_dir in ORT_PINNING_SNAPS:
        assert "process-control" in plugs, (
            f"{name}: daemon app {daemon_name!r} does not plug process-control, so "
            "ORT cannot pin its thread pool - seccomp refuses every "
            "sched_setaffinity with EPERM and the snap runs unpinned"
        )
    else:
        assert "process-control" not in plugs, (
            f"{name}: daemon app {daemon_name!r} plugs process-control, but this snap "
            "does not let ORT size its own pool, so nothing here ever pins - drop the "
            "plug rather than grant a broad interface (kill/setscheduler/cgroup "
            f"writes) that has no effect, or add {snap_dir!r} to ORT_PINNING_SNAPS"
        )


def test_thread_capped_adapters_are_exactly_the_declared_ones() -> None:
    """An explicit intra_op_num_threads silently disables ORT thread pinning.

    ORT sets affinity only when it sizes the pool itself - measured 2026-09-03
    on onnxruntime 1.27.0, where intra_op_num_threads 0 issued 24
    sched_setaffinity calls and both 1 and 4 issued none. So a hardcoded count
    costs pinning *and* caps the snap below the machine it was installed on
    (funasr and audio8 shipped at 4 threads regardless of core count, T65).

    Asserted as an equality, not an emptiness: parakeet's perf pass added a cap
    and nothing noticed its process-control plug had gone inert. Adding or
    removing a cap now has to move this set, which is the same edit that has to
    move ORT_PINNING_SNAPS.
    """
    adapters = REPO_ROOT / "server" / "src" / "myna" / "testbed"
    offenders = set()
    for path in sorted(adapters.glob("*.py")):
        # Parse rather than grep: the string also appears in prose explaining
        # why it is not passed, and a comment must not fail the build.
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            # Both spellings: a keyword to the library that wraps ORT, and an
            # attribute set on a SessionOptions. parakeet uses the second, and
            # a kwarg-only walk read it as uncapped for a fortnight.
            values = []
            if isinstance(node, ast.Call):
                values = [kw.value for kw in node.keywords if kw.arg == "intra_op_num_threads"]
            elif isinstance(node, ast.Assign):
                values = [
                    node.value
                    for t in node.targets
                    if isinstance(t, ast.Attribute) and t.attr == "intra_op_num_threads"
                ]
            for value in values:
                # 0 means "ORT, size it yourself", which is the whole point and
                # is not always omittable: funasr_onnx defaults the argument to
                # 4, so leaving it out there caps the pool instead of freeing it.
                if isinstance(value, ast.Constant) and value.value == 0:
                    continue
                offenders.add(path.name)
    assert offenders == THREAD_CAPPED_ADAPTERS, (
        f"thread-capped adapters are {sorted(offenders)}, expected "
        f"{sorted(THREAD_CAPPED_ADAPTERS)}. A cap makes ORT skip affinity and holds "
        "the pool below the machine - pass 0 (or omit it, where the library default "
        "is already 0/None) unless the cap buys more than pinning does, and keep "
        "ORT_PINNING_SNAPS in step either way"
    )
