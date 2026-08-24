#!/usr/bin/env python3
"""dbus_headless.py — check the lab's D-Bus publisher against the contract.

    python3 dbus_headless.py

The publisher exists to drive a live GNOME Shell, which needs a session, a
compositor and a person looking at the screen. This checks everything about
it that does *not* need those: that the name is claimed, that a plain
`Gio.DBusProxy` — built exactly the way `dbus.js` builds it — sees the
right State/ErrorMessage/levels, that transitions actually arrive as
`PropertiesChanged`, and that the level inversion round-trips through
`vumeter.js`'s calibration.

It runs on a private bus of its own (re-execing under `dbus-run-session` if
needed), so it never touches the real session bus and can never collide with
a running `myna-desktop`.

Exits non-zero on failure.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys

from gi.repository import Gio, GLib

from dictation_service import (
    BUS_NAME,
    OBJECT_PATH,
    SHELL_INTERNAL_PHASES,
    DictationPublisher,
    envelope_to_levels,
    shell_phase,
    wire_state,
)

failures = 0


def check(name: str, condition: bool, detail: str = "") -> bool:
    global failures
    if condition:
        print(f"ok   {name}" + (f" ({detail})" if detail else ""))
    else:
        failures += 1
        print(f"FAIL {name}" + (f" ({detail})" if detail else ""))
    return condition


def boost_level(level: float) -> float:
    """`vumeter.js`'s boostLevel, transcribed, to verify the inverse.

    Duplicated here on purpose, and *only* here: this is the one place the
    duplication is the point, since a check that reused the inverse's own
    constants could not detect them drifting from vumeter.js.
    """
    import math
    if level <= 0:
        return 0.0
    db = 20 * math.log10(level)
    return min(1.0, max(0.0, (db - (-67.0)) / (-14.0 - (-67.0))))


def settle(ms: int = 200) -> None:
    """Run the main loop long enough for `ms` of publishing to be delivered."""
    loop = GLib.MainLoop()
    GLib.timeout_add(ms, lambda: (loop.quit(), False)[1])
    loop.run()


def call(proxy, method: str):
    """Call a method and pump the loop until the reply lands.

    Asynchronous rather than `call_sync` because the publisher lives in this
    same process and dispatches on this same main loop: a blocking call would
    stall the loop that has to produce the reply, and time out after 25s.
    """
    result = {}
    loop = GLib.MainLoop()

    def on_reply(p, res):
        result["value"] = p.call_finish(res)
        loop.quit()

    proxy.call(method, None, Gio.DBusCallFlags.NONE, -1, None, on_reply)
    GLib.timeout_add(2000, lambda: (loop.quit(), False)[1])
    loop.run()
    # Let the next tick publish whatever the method changed.
    settle()
    return result.get("value")


def check_mapping() -> None:
    """The pure phase/severity → wire mapping."""
    for phase, expected in [("flow", "recording"), ("unfold", "recording"),
                            ("morph", "transcribing"),
                            ("complete", "finalizing")]:
        state, reason = wire_state(phase, None)
        check(f"phase {phase} publishes {expected}", state == expected, state)
        check(f"phase {phase} carries no reason", reason == "", repr(reason))

    state, reason = wire_state("flow", "recoverable")
    check("a recoverable tint publishes notice", state == "notice", state)
    state, reason = wire_state("flow", "critical")
    check("a critical tint publishes error", state == "error", state)
    check("the error reason is content-free and non-empty", reason != "", reason)

    state, _ = wire_state("morph", "recoverable")
    check("severity outranks the phase", state == "notice", state)

    state, _ = wire_state("flow", None, session_active=False)
    check("a stopped session publishes idle", state == "idle", state)

    state, _ = wire_state("some-future-phase", None)
    check("an unknown phase degrades to active", state == "active", state)


def check_round_trip() -> None:
    """What the Shell ends up rendering for each phase the lab can select.

    The wire is lossy, so some phases cannot survive the trip. That is fine,
    but it has to be *known*: the lab labels those rows so a phase that moves
    its own ribbon and not the Shell's reads as a documented limit rather
    than a broken bridge.
    """
    for phase in ["flow", "morph", "complete"]:
        check(f"{phase} survives the round trip",
              shell_phase(phase, None) == phase, str(shell_phase(phase, None)))

    check("unfold comes back as flow", shell_phase("unfold", None) == "flow")
    check("unfold is marked as Shell-driven", "unfold" in SHELL_INTERNAL_PHASES)

    # A phase no state requests cannot round-trip, whatever it publishes.
    # None exists today (relax, which did, was removed from ribbon.js), so
    # this checks the labelling stays honest for one added later.
    check("an unreachable phase is reported as not round-tripping",
          shell_phase("some-future-phase", None) == "flow",
          str(shell_phase("some-future-phase", None)))
    check("an unreachable phase is not claimed to be Shell-driven",
          "some-future-phase" not in SHELL_INTERNAL_PHASES)

    for tint in ["recoverable", "critical"]:
        check(f"a {tint} severity leaves the Shell's phase alone",
              shell_phase("flow", tint) is None)
    check("a stopped session leaves no phase",
          shell_phase("flow", None, session_active=False) is None)


def check_levels() -> None:
    """The envelope inversion must round-trip through vumeter's curve."""
    check("a zero envelope is exactly silent",
          envelope_to_levels(0.0) == (0.0, 0.0))
    worst = 0.0
    for envelope in [0.05, 0.2, 0.5, 0.75, 1.0]:
        rms, peak = envelope_to_levels(envelope)
        combined = boost_level(max(rms, peak * 0.55))
        worst = max(worst, abs(combined - envelope))
        check(f"envelope {envelope} survives the round trip",
              abs(combined - envelope) < 1e-9, f"got {combined:.6f}")
    check("the weighted peak never overtakes the RMS", worst < 1e-9,
          f"worst error {worst:.2e}")
    rms, peak = envelope_to_levels(1.0)
    check("a full envelope stays inside the wire range",
          0.0 <= rms <= 1.0 and 0.0 <= peak <= 1.0, f"rms={rms:.3f} peak={peak:.3f}")


def check_wire() -> None:
    """End to end: a real proxy against the real publisher."""
    look = {"phase": "flow", "severityTint": None, "envelope": 0.5}
    publisher = DictationPublisher(lambda: dict(look))
    publisher.start()

    loop = GLib.MainLoop()
    seen = {"appeared": False, "proxy": None, "states": []}

    def on_appeared(_conn, _name, _owner):
        seen["appeared"] = True
        # Constructed the same way dbus.js does, so this exercises the path
        # the extension actually takes rather than a convenient shortcut.
        Gio.DBusProxy.new(
            Gio.bus_get_sync(Gio.BusType.SESSION, None),
            Gio.DBusProxyFlags.NONE, None, BUS_NAME, OBJECT_PATH, BUS_NAME,
            None, on_proxy_ready)

    def on_proxy_ready(_source, result):
        proxy = Gio.DBusProxy.new_finish(result)
        seen["proxy"] = proxy
        proxy.connect("g-properties-changed", on_properties_changed)

    def on_properties_changed(proxy, _changed, _invalidated):
        state = proxy.get_cached_property("State").unpack()
        if not seen["states"] or seen["states"][-1] != state:
            seen["states"].append(state)

    watch_id = Gio.bus_watch_name(
        Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.NONE,
        on_appeared, None)

    # Walk a whole session the way a real one runs, giving each step enough
    # ticks at PUBLISH_HZ to be published and delivered.
    steps = [
        (300, lambda: None),
        (200, lambda: look.update(phase="morph")),
        (200, lambda: look.update(phase="complete")),
        (200, lambda: look.update(phase="flow", severityTint="recoverable")),
        (200, lambda: look.update(severityTint="critical")),
    ]
    delay = 0
    for duration, step in steps:
        delay += duration
        GLib.timeout_add(delay, lambda s=step: (s(), False)[1])
    GLib.timeout_add(delay + 300, lambda: (loop.quit(), False)[1])
    loop.run()

    proxy = seen["proxy"]
    check("the extension's name watch fires", seen["appeared"])
    if not check("a proxy connects to the publisher", proxy is not None):
        publisher.stop()
        Gio.bus_unwatch_name(watch_id)
        return

    check("every transition arrives in order",
          seen["states"] == ["recording", "transcribing", "finalizing",
                             "notice", "error"],
          " → ".join(seen["states"]))
    check("the critical error carries its reason",
          proxy.get_cached_property("ErrorMessage").unpack() != "")
    check("levels stop once recording ends",
          proxy.get_cached_property("AudioRms").unpack() == 0.0,
          f"{proxy.get_cached_property('AudioRms').unpack():.4f}")

    # Back to recording: levels must resume and match the slider.
    look.update(phase="flow", severityTint=None, envelope=0.75)
    settle(300)
    expected_rms = envelope_to_levels(0.75)[0]
    published = proxy.get_cached_property("AudioRms").unpack()
    check("levels resume with the published envelope",
          abs(published - expected_rms) < 1e-9,
          f"{published:.4f} vs {expected_rms:.4f}")

    # The contract's methods, which the extension never calls but the
    # interface promises (C6). Called asynchronously, not with call_sync:
    # the publisher shares this process and answers on the same main loop, so
    # a blocking call would deadlock against the very object it is calling.
    call(proxy, "Stop")
    check("Stop clears the pill to idle",
          proxy.get_cached_property("State").unpack() == "idle",
          proxy.get_cached_property("State").unpack())

    ok, reason = call(proxy, "Start").unpack()
    check("Start reports success", ok is True and reason == "")

    publisher.stop()
    vanished = {"seen": False}
    Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME,
                       Gio.BusNameWatcherFlags.NONE, None,
                       lambda *_a: vanished.update(seen=True))
    settle(500)
    check("releasing the name is visible to a watcher", vanished["seen"])
    Gio.bus_unwatch_name(watch_id)


def main() -> int:
    check_mapping()
    check_round_trip()
    check_levels()
    check_wire()
    print("PASS dbus_headless.py" if failures == 0
          else f"FAIL dbus_headless.py ({failures} failed)")
    return 1 if failures else 0


if __name__ == "__main__":
    # A private bus, so the check can never disturb — or be disturbed by — a
    # real myna-desktop on the developer's own session bus.
    if os.environ.get("MYNA_LAB_PRIVATE_BUS") != "1":
        if shutil.which("dbus-run-session") is None:
            print("FAIL dbus_headless.py (dbus-run-session not installed)")
            sys.exit(1)
        sys.exit(subprocess.call(
            ["dbus-run-session", "--", sys.executable, *sys.argv],
            env={**os.environ, "MYNA_LAB_PRIVATE_BUS": "1"}))
    sys.exit(main())
