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

Scope: the *inference* snaps (the ones exposing modelctl + the session socket).
The client is excluded. The fake backend has no modelctl, so it joins only the
provider-share checks.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

import pytest
import yaml

# Reads the repository outside server/ (see [tool.mutmut] in pyproject.toml).
pytestmark = pytest.mark.repo_tree

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
MODELCTL_RELEASE = "v2.0.0-beta.14"

# Every snap exposing a backend over the inference-provider content interface.
PROVIDER_SNAPS = sorted([*INFERENCE_SNAPS, "fake-snap"])
SHARE_DIR = "$SNAP_COMMON/share/provider"
SOCKET_PATH = f"{SHARE_DIR}/myna.sock"


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


def _launchers(snap_dir: str) -> list[Path]:
    """Scripts on the daemon's path to myna-server: the service script and engines."""
    root = REPO_ROOT / snap_dir
    return sorted([*root.glob("scripts/*"), *root.glob("engines/*/server")])


def _commands(script: str) -> list[str]:
    """Logical shell lines, backslash continuations joined."""
    return [" ".join(line.split()) for line in re.sub(r"\\\n", " ", script).splitlines()]


def _server_execs(path: Path) -> list[str]:
    """The `exec` lines that start myna-server."""
    return [
        c
        for c in _commands(path.read_text(encoding="utf-8"))
        if c.startswith("exec ") and ("myna-server" in c or "myna.server" in c)
    ]


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


def test_single_cpu_engine_snaps_activate_it_by_name(snap) -> None:
    """One CPU engine means hardware scoring has one possible answer.

    Selecting by name also survives a sideload, where hardware-observe is not
    auto-connected and `--auto` would leave the snap with no active engine.
    nemotron is not covered: its single engine is nvidia-gpu, where refusing to
    activate on a machine that cannot run it is the honest outcome.
    """
    snap_dir, name, _ = snap
    engines = [p.name for p in (REPO_ROOT / snap_dir / "engines").iterdir() if p.is_dir()]
    if engines != ["cpu"]:
        return
    hook = (REPO_ROOT / snap_dir / "snap" / "hooks" / "install").read_text(encoding="utf-8")
    assert "use-engine cpu" in hook, (
        f"{name}: ships only a cpu engine but the install hook does not select it "
        "by name, so a sideloaded install can end up with no active engine"
    )
    assert "use-engine --auto" not in hook, (
        f"{name}: ships only a cpu engine, so the install hook must not ask "
        "modelctl to score hardware for the one possible answer"
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


def test_user_config_lives_in_modelctl_only(snap) -> None:
    """modelctl's config is the single store for user-facing settings.

    snapd's config is a second store with different reader privileges: reading
    it needs polkit interaction, which a desktop session cannot supply without
    a terminal, and a stale `snap set` value silently shadowed what myna-config
    had written through modelctl. So engine scripts read modelctl only, and no
    snap ships a configure hook that would advertise `snap set` as the way in.
    """
    snap_dir, name, _ = snap
    hook = REPO_ROOT / snap_dir / "snap" / "hooks" / "configure"
    assert not hook.is_file(), (
        f"{name}: a configure hook implies `snap set` configures this snap, but "
        "modelctl is the only store the engine scripts read"
    )
    for server in sorted((REPO_ROOT / snap_dir / "engines").glob("*/server")):
        script = server.read_text(encoding="utf-8")
        assert "snapctl get" not in script, (
            f"{name}: engines/{server.parent.name}/server reads snapd config; "
            "read the value with `modelctl get` so there is one store"
        )


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
        "`modelctl status` cannot report the session entrypoint"
    )
    for server in sorted((REPO_ROOT / snap_dir / "engines").glob("*/server")):
        script = server.read_text(encoding="utf-8")
        assert "socket.path" not in script, (
            f"{name}: engines/{server.parent.name}/server still reads the retired socket.path key"
        )


@pytest.mark.parametrize("snap_dir", PROVIDER_SNAPS)
def test_exposes_the_session_socket(snap_dir: str) -> None:
    """Confined clients reach the backend over the inference-provider content share."""
    slot = (_recipe(snap_dir).get("slots") or {}).get("provider")
    assert isinstance(slot, dict), f"{snap_dir}: no provider slot; confined clients cannot connect"
    assert slot.get("interface") == "content"
    assert slot.get("content") == "inference-provider", (
        f"{snap_dir}: provider slot has the wrong content id"
    )
    # write, not read: connect() on a socket through the bind mount needs rw.
    assert slot.get("source") == {"write": [SHARE_DIR]}, (
        f"{snap_dir}: provider slot must share exactly {SHARE_DIR} writable"
    )


def test_hooks_put_the_socket_in_the_share(snap) -> None:
    """The socket lives in the shared directory, where myna-server writes provider.env.

    post-refresh sets it unconditionally: revisions from before the share carry
    the old path in package scope, so a presence guard would keep it.
    """
    snap_dir, name, _ = snap
    hooks = REPO_ROOT / snap_dir / "snap" / "hooks"
    expected = f'modelctl set --package ws.unix-socket="{SOCKET_PATH}"'
    for hook in ("install", "post-refresh"):
        commands = _commands((hooks / hook).read_text(encoding="utf-8"))
        assert expected in commands, f"{name}: {hook} hook does not run `{expected}`"
    post_refresh = (hooks / "post-refresh").read_text(encoding="utf-8")
    assert "modelctl get ws.unix-socket" not in post_refresh, (
        f"{name}: post-refresh guards ws.unix-socket, so a refresh keeps the pre-share path"
    )


@pytest.mark.parametrize("snap_dir", PROVIDER_SNAPS)
def test_launchers_share_the_provider(snap_dir: str) -> None:
    """Without --share-provider no provider.env is written and no consumer finds us."""
    execs = [(p, c) for p in _launchers(snap_dir) for c in _server_execs(p)]
    assert execs, f"{snap_dir}: no launcher starts myna-server"
    for path, command in execs:
        assert "--share-provider" in command.split(), (
            f"{path.relative_to(REPO_ROOT)}: myna-server is started without --share-provider"
        )


def test_fake_backend_serves_in_the_share() -> None:
    [command] = _server_execs(REPO_ROOT / "fake-snap" / "scripts" / "server.sh")
    assert f'--socket "{SOCKET_PATH}"' in command


@pytest.mark.parametrize("snap_dir", PROVIDER_SNAPS)
def test_modelctl_does_not_write_provider_env(snap_dir: str) -> None:
    """modelctl's writer omits UNIX_SOCKET and would clobber myna-server's file."""
    for path in _launchers(snap_dir):
        for command in _commands(path.read_text(encoding="utf-8")):
            if "modelctl run" not in command:
                continue
            options = command.partition(" -- ")[0]
            assert "--share-provider" not in options, (
                f"{path.relative_to(REPO_ROOT)}: `modelctl run --share-provider` "
                "overwrites provider.env without UNIX_SOCKET"
            )


def test_streaming_toggle_is_a_config_key_not_a_hardcoded_flag(snap) -> None:
    """`--streaming` baked into an engine script is not a user-facing choice.

    Emission mode is a shipped configuration (`modelctl set streaming=`), so
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


def test_punctuation_toggle_is_a_config_key_not_a_hardcoded_flag(snap) -> None:
    """Same argument as the streaming toggle above, for the other output-shaping
    choice a shipped snap makes.

    A snap that restores punctuation must let an operator turn it off, because
    the raw transducer output is the baseline the restoration is measured
    against - and one that hardcodes `--sherpa-no-punct` would commit lowercase
    text with nothing in the config surface saying why. Snaps whose model
    punctuates natively mention neither flag, which is also fine.
    """
    snap_dir, name, _ = snap
    for server in sorted((REPO_ROOT / snap_dir / "engines").glob("*/server")):
        script = server.read_text(encoding="utf-8")
        if "--sherpa-punct-model" not in script and "--sherpa-no-punct" not in script:
            continue
        assert "punct_args" in script and "modelctl get punctuation" in script, (
            f"{name}: engines/{server.parent.name}/server hardcodes its punctuation "
            "choice; read the `punctuation` config key instead so the unpunctuated "
            "baseline stays measurable on the same build"
        )


def test_models_declare_realtime_transcription(snap) -> None:
    """modelctl filters and reports models by capability; ours serve the realtime API."""
    snap_dir, name, _ = snap
    manifests = sorted((REPO_ROOT / snap_dir / "models").glob("*/model.yaml"))
    assert manifests, f"{name}: no model manifests"
    for path in manifests:
        manifest = yaml.safe_load(path.read_text(encoding="utf-8"))
        assert "realtime-transcription" in (manifest.get("capabilities") or []), (
            f"{name}: models/{path.parent.name} does not declare realtime-transcription"
        )


def test_whisper_quantization_describes_the_packaged_artifact() -> None:
    """Model metadata describes disk weights, not a runtime compute policy."""
    model_dir = REPO_ROOT / "whisper-snap" / "models"
    expected_compute_type = {
        "tiny": "int8",
        "base": "float32",
        "small": "float32",
    }

    for name, compute_type in expected_compute_type.items():
        manifest = yaml.safe_load((model_dir / name / "model.yaml").read_text(encoding="utf-8"))
        assert manifest["quantization"] == "float16"
        assert manifest["format"] == "CTranslate2"
        assert f"MODEL_COMPUTE_TYPE={compute_type}" in manifest["environment"]


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


# Every snap directory, not only the inference ones: the license text must
# reach every package we distribute, the client and the fake backend included.
ALL_SNAPS = sorted(
    p.name for p in REPO_ROOT.glob("*-snap") if (p / "snap" / "snapcraft.yaml").exists()
)

PROJECT_LICENSE = "AGPL-3.0-or-later"


@pytest.mark.parametrize("snap_dir", ALL_SNAPS)
def test_declares_the_project_license(snap_dir: str) -> None:
    """The store shows `license`, and it must match the tree's LICENSE."""
    recipe = _recipe(snap_dir)
    assert recipe.get("license") == PROJECT_LICENSE, (
        f"{snap_dir}: snapcraft.yaml declares license={recipe.get('license')!r}; "
        f"the tree is {PROJECT_LICENSE}"
    )


@pytest.mark.parametrize("snap_dir", ALL_SNAPS)
def test_ships_the_license_text(snap_dir: str) -> None:
    """AGPL section 6: a binary distribution carries the license text.

    ``dev/stage-licenses.sh`` copies LICENSE (and the snap's NOTICE, when it
    has one) into ``<snap>/licenses/``, and a ``licenses`` part dumps that
    directory under ``usr/share/doc/<snap>/``. The store's `license` field
    alone does not put the text in the package.
    """
    recipe = _recipe(snap_dir)
    part = (recipe.get("parts") or {}).get("licenses")
    assert part, f"{snap_dir}: no `licenses` part"
    assert part.get("plugin") == "dump" and part.get("source") == "licenses", (
        f"{snap_dir}: the licenses part must dump the staged licenses/ directory"
    )
    assert part.get("organize") == {"*": f"usr/share/doc/{recipe['name']}/"}, (
        f"{snap_dir}: licenses must land under usr/share/doc/{recipe['name']}/"
    )
    prepare = (REPO_ROOT / snap_dir / "dev" / "prepare.sh").read_text(encoding="utf-8")
    assert "dev/stage-licenses.sh" in prepare, (
        f"{snap_dir}/dev/prepare.sh does not stage licenses/ (dev/stage-licenses.sh)"
    )


def test_every_component_is_attributed_in_the_notice(snap) -> None:
    """Model weights ship under their own licenses (CC-BY-4.0, MIT, ...).

    Attribution travels with the snap as NOTICE, staged next to LICENSE. A
    component that NOTICE does not name is a redistribution with no
    attribution, which is exactly what CC-BY forbids.
    """
    snap_dir, name, recipe = snap
    notice_path = REPO_ROOT / snap_dir / "NOTICE"
    assert notice_path.exists(), f"{name}: no {snap_dir}/NOTICE"
    notice = notice_path.read_text(encoding="utf-8")
    for component in recipe.get("components") or {}:
        assert component in notice, f"{name}: component {component!r} is not attributed in NOTICE"
