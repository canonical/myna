"""dictation_service.py — publish the lab's look on `org.myna.Dictation`.

The lab renders the ribbon exactly as the Shell does, but in its own window.
This closes the last gap: it also *owns the bus name the extension watches*,
so a live GNOME Shell session shows the real HUD — real pill, real label,
real icon, real placement — driven by the lab's sliders instead of by
speech. Turn the switch on, drag the level, and the pill at the bottom of
the screen moves with it.

That makes the lab a stand-in for `myna-desktop --dbus`, which is the only
other way to see the HUD react, and which needs a microphone, a model and a
real session to say anything at all.

Nothing here decides how the ribbon *looks* — that stays in JS, behind
`bridge.js`. This maps the lab's controls onto the four wire properties of
`specs/004-gnome-shell-indicator/contracts/dbus-interface.md`, and that
mapping is the whole content of this file.

Levels are throttled to PUBLISH_HZ, matching the contract's ~15-20 Hz
cadence (C4) rather than the lab's ~60 fps render loop, so the extension
sees the update rate it was tuned against.
"""

from __future__ import annotations

from typing import Callable

from gi.repository import Gio, GLib

BUS_NAME = "org.myna.Dictation"
OBJECT_PATH = "/org/myna/Dictation"

# The contract's interface, verbatim. Methods are included even though the
# extension never calls them (it is a read-only consumer): they are part of
# the interface, and they make the lab drivable from a terminal, e.g.
#   gdbus call --session -d org.myna.Dictation \
#       -o /org/myna/Dictation -m org.myna.Dictation.Toggle
INTERFACE_XML = """
<node>
  <interface name='org.myna.Dictation'>
    <property name='State' type='s' access='read'/>
    <property name='AudioRms' type='d' access='read'/>
    <property name='AudioPeak' type='d' access='read'/>
    <property name='ErrorMessage' type='s' access='read'/>
    <method name='Start'>
      <arg type='b' name='ok' direction='out'/>
      <arg type='s' name='error' direction='out'/>
    </method>
    <method name='Stop'/>
    <method name='Toggle'/>
  </interface>
</node>
"""

PUBLISH_HZ = 20

# Which wire State each ribbon phase belongs to — the inverse of the
# extension's own `hudLogic.ribbonPhaseForStateKey`. That mapping is
# many-to-one (loading, recording and active all request `flow`), so the
# inverse has to pick one; `recording` is chosen because it is the state a
# person watching the ribbon flow is actually in. `unfold` is the reveal a
# fresh session plays and `relax` is internal to the ribbon, so both sit
# inside a recording session too.
PHASE_STATE = {
    "unfold": "recording",
    "flow": "recording",
    "morph": "transcribing",
    "complete": "finalizing",
}

# Severity outranks the phase: the Shell drives notice/error from the state
# itself, not from a ribbon phase (`ribbonPhaseForStateKey` returns null for
# both), so a tinted ribbon has to publish the matching state or the pill
# would stay neutral while only the lab's ribbon went amber.
SEVERITY_STATE = {"recoverable": "notice", "critical": "error"}

# Content-free reasons, per constitution V and contract C3 — never anything
# derived from a transcript. The empty one is deliberate: it exercises the
# path where states.js supplies its own default text ("No speech detected"),
# while the error reason exercises the "Error — %s" prefix. Between them the
# two ErrorMessage renderings are both visible from the lab.
NOTICE_REASON = ""
ERROR_REASON = "Microphone unavailable"

# vumeter.js's calibrated dBFS window, mirrored so the lab can invert it.
DB_FLOOR = -67.0
DB_CEILING = -14.0
# vumeter.js takes max(rms, peak * 0.55), so any peak below rms / 0.55 leaves
# RMS in charge. 1.8 keeps a plausible ~5 dB crest above RMS while staying
# under that limit, so the slider still maps exactly onto the HUD intensity
# instead of the peak term quietly taking over at the top of the range.
PEAK_OVER_RMS = 1.8


# Phases the Shell's ribbon runs by itself, with no state asking for it:
# hud.js starts every fresh session in `unfold` and advances to `flow` on
# its own after UNFOLD_MS. So publishing `recording` for `unfold` is right —
# the Shell will play the reveal when the pill appears, on its own clock,
# not because the wire said so.
SHELL_INTERNAL_PHASES = frozenset({"unfold"})

# `hudLogic.ribbonPhaseForStateKey`, mirrored: what the Shell's ribbon
# actually does with each state that can be published. Used only to explain
# the round trip in the UI — the publisher itself never consults it.
#
# The mapping is lossy in both directions, and this is the side that shows
# it: several states collapse onto `flow`, so a phase can move the lab's
# ribbon without moving the Shell's. Every phase currently round-trips or is
# Shell-driven, but a phase added to ribbon.js that no state requests would
# not, and the lab labels that rather than leaving it looking like a broken
# link. (`relax` was exactly such a phase, and was removed 2026-08-24.)
STATE_PHASE = {
    "loading": "flow",
    "recording": "flow",
    "active": "flow",
    "transcribing": "morph",
    "finalizing": "complete",
    # notice/error force no phase (the severity carries them instead), and
    # idle hides the ribbon; in all three the ribbon keeps what it had.
    "notice": None,
    "error": None,
    "idle": None,
}


def shell_phase(phase: str, severity_tint: str | None = None,
                session_active: bool = True) -> str | None:
    """The phase the Shell's ribbon will run for this look.

    Round-trips the lab's phase out through the wire and back through the
    extension's own state → phase mapping, so a phase the Shell cannot
    reach can be labelled as such instead of looking like a broken link.

    :returns: a `RibbonPhase` value, or None when the state leaves the
        Shell's ribbon phase untouched.
    """
    state, _ = wire_state(phase, severity_tint, session_active)
    return STATE_PHASE.get(state)


def wire_state(phase: str, severity_tint: str | None,
               session_active: bool = True) -> tuple[str, str]:
    """The lab's look as a `(State, ErrorMessage)` pair.

    :param phase: a `RibbonPhase` value, as chosen in the lab.
    :param severity_tint: `'recoverable'`, `'critical'` or None.
    :param session_active: False once `Stop`/`Toggle` ended the session —
        the daemon is still running, it is simply not dictating, which is
        the case that should clear the pill entirely.
    :returns: the two string properties to publish.
    """
    if not session_active:
        return "idle", ""
    if severity_tint in SEVERITY_STATE:
        state = SEVERITY_STATE[severity_tint]
        return state, NOTICE_REASON if state == "notice" else ERROR_REASON
    # Unknown phases degrade to `active`, the same additive tolerance the
    # contract asks of clients (C8) — a phase added to ribbon.js later shows
    # up as a neutral live state rather than breaking the publisher.
    return PHASE_STATE.get(phase, "active"), ""


def envelope_to_levels(envelope: float) -> tuple[float, float]:
    """Invert `vumeter.boostLevel` so the slider drives the HUD 1:1.

    The lab's slider is the *smoothed envelope* — what the ribbon consumes —
    but the wire carries raw RMS and peak, which the extension pushes back
    through `levelsToIntensity`. Publishing the slider value directly would
    put the ribbon in the lab and the ribbon in the Shell at visibly
    different amplitudes for the same setting. Inverting the calibration
    here is what makes the two agree.

    :param envelope: the smoothed envelope in [0, 1].
    :returns: `(rms, peak)`, both in [0, 1].
    """
    level = min(1.0, max(0.0, envelope))
    if level <= 0.0:
        return 0.0, 0.0
    db = DB_FLOOR + level * (DB_CEILING - DB_FLOOR)
    rms = min(1.0, 10.0 ** (db / 20.0))
    return rms, min(1.0, rms * PEAK_OVER_RMS)


class DictationPublisher:
    """Owns `org.myna.Dictation` and publishes the lab's state on it.

    Everything is recomputed from one `snapshot()` callback on a timer,
    rather than from a signal per control, so a control added to the lab
    later is published without having to be wired up here as well.
    """

    def __init__(self, snapshot: Callable[[], dict]) -> None:
        """
        :param snapshot: called each tick; must return a dict with `phase`,
            `severityTint` and `envelope` keys.
        """
        self._snapshot = snapshot
        self._conn = None
        self._owner_id = 0
        self._registration_id = 0
        self._timer_id = 0
        self._session_active = True
        self._state = "idle"
        self._error_message = ""
        self._rms = 0.0
        self._peak = 0.0
        self._status = "off"
        self.on_status_changed = lambda status: None

    @property
    def status(self) -> str:
        """One of `off`, `connecting`, `publishing`, or an error string."""
        return self._status

    @property
    def state(self) -> str:
        """The State most recently published."""
        return self._state

    def start(self) -> None:
        """Claim the bus name. Idempotent."""
        if self._owner_id != 0:
            return
        self._session_active = True
        self._set_status("connecting")
        try:
            self._conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        except GLib.Error as error:
            self._set_status(f"no session bus: {error.message}")
            return

        info = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0]
        self._registration_id = self._conn.register_object(
            OBJECT_PATH, info, self._on_method_call, self._on_get_property, None)
        # Deliberately not REPLACE: if myna-desktop is already running, the
        # honest outcome is to say so rather than to silently displace the
        # real daemon and leave the user wondering why dictation stopped
        # working.
        self._owner_id = Gio.bus_own_name_on_connection(
            self._conn, BUS_NAME, Gio.BusNameOwnerFlags.NONE,
            lambda *_a: self._on_name_acquired(),
            lambda *_a: self._on_name_lost())

    def stop(self) -> None:
        """Release the name. The extension sees name-vanished and clears."""
        if self._timer_id != 0:
            GLib.source_remove(self._timer_id)
            self._timer_id = 0
        if self._owner_id != 0:
            Gio.bus_unown_name(self._owner_id)
            self._owner_id = 0
        if self._registration_id != 0:
            self._conn.unregister_object(self._registration_id)
            self._registration_id = 0
        self._state = "idle"
        self._error_message = ""
        self._rms = self._peak = 0.0
        self._set_status("off")

    def _on_name_acquired(self) -> None:
        self._set_status("publishing")
        if self._timer_id == 0:
            self._timer_id = GLib.timeout_add(
                1000 // PUBLISH_HZ, self._publish_tick)

    def _on_name_lost(self) -> None:
        # Either the name was already taken, or it was taken from us.
        self._set_status(f"{BUS_NAME} is owned by another process")

    def _publish_tick(self) -> bool:
        look = self._snapshot()
        state, error_message = wire_state(
            look.get("phase", "flow"), look.get("severityTint"),
            self._session_active)
        changed = {}

        if state != self._state or error_message != self._error_message:
            # Both in a single PropertiesChanged so the proxy cache updates
            # atomically. The contract asks for ErrorMessage to be set
            # before State so a client reacting to the transition already
            # reads a consistent reason; one combined emission is a stronger
            # form of the same guarantee than two ordered ones, which would
            # briefly expose the old state paired with the new reason.
            changed["ErrorMessage"] = GLib.Variant("s", error_message)
            changed["State"] = GLib.Variant("s", state)
            self._state = state
            self._error_message = error_message

        # Levels only while recording, mirroring the daemon: nothing is being
        # captured during transcribing/finalizing, so the updates simply stop
        # and the extension's stale-decay eases the VU to its floor — the
        # same thing that happens at the end of a real session. The final
        # zero is published explicitly so the meter has a defined resting
        # value rather than only an implied one (E2).
        rms, peak = (envelope_to_levels(look.get("envelope", 0.0))
                     if state == "recording" else (0.0, 0.0))
        if state == "recording" or (rms, peak) != (self._rms, self._peak):
            # Not deduplicated while recording: arrival *time* is part of the
            # VU contract, since the extension resets its stale-decay on each
            # update. A held slider publishes the same number every tick, and
            # dropping those would decay the HUD to the floor after ~300 ms
            # while the lab's own ribbon stayed put.
            changed["AudioRms"] = GLib.Variant("d", rms)
            changed["AudioPeak"] = GLib.Variant("d", peak)
            self._rms, self._peak = rms, peak

        if changed:
            self._emit_properties_changed(changed)
        return GLib.SOURCE_CONTINUE

    def _emit_properties_changed(self, changed: dict) -> None:
        self._conn.emit_signal(
            None, OBJECT_PATH, "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            GLib.Variant("(sa{sv}as)", (BUS_NAME, changed, [])))

    def _on_get_property(self, _conn, _sender, _path, _iface, name,
                         *_rest):
        # Answers the GetAll a fresh Gio.DBusProxy makes on construction,
        # which is how the extension picks up a session already in progress
        # when it is enabled mid-run (contract X8).
        return {
            "State": lambda: GLib.Variant("s", self._state),
            "ErrorMessage": lambda: GLib.Variant("s", self._error_message),
            "AudioRms": lambda: GLib.Variant("d", self._rms),
            "AudioPeak": lambda: GLib.Variant("d", self._peak),
        }[name]()

    def _on_method_call(self, _conn, _sender, _path, _iface, method,
                        _params, invocation, *_rest):
        if method == "Start":
            self._session_active = True
            invocation.return_value(GLib.Variant("(bs)", (True, "")))
            return
        if method == "Stop":
            self._session_active = False
        elif method == "Toggle":
            self._session_active = not self._session_active
        invocation.return_value(None)

    def _set_status(self, status: str) -> None:
        if status == self._status:
            return
        self._status = status
        self.on_status_changed(status)

