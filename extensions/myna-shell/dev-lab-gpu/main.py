#!/usr/bin/env python3
"""main.py — interactive GPU dev-lab for the wave ribbon.

    python3 main.py

A `Gtk.GLArea` running the *shipped* fragment shader, with live controls for
everything the Shell would otherwise drive from real dictation: input level,
lifecycle phase, severity tint, reduced motion.

The counterpart to `dev-lab/` (GTK4 + GJS), which does the same job for the
Cairo painter. This one has to be Python because a standalone GL area needs
raw `glCreateShader`/`glUseProgram` calls, which come from `libepoxy` and are
not introspectable — so they are reachable from PyOpenGL but not from gjs.
That language split is the *only* reason this is not JS; every decision about
what to draw still happens in JS, on the far side of `bridge.js`.
"""

from __future__ import annotations

import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")

from gi.repository import Adw, GLib, Gtk  # noqa: E402
from OpenGL import GL  # noqa: E402

from bridge import BridgeError, RibbonModel, load_shader  # noqa: E402
from dictation_service import (  # noqa: E402
    DictationPublisher,
    SHELL_INTERNAL_PHASES,
    shell_phase,
    wire_state,
)
from ribbon_gl import (  # noqa: E402
    PROFILE_ES300,
    QuadDrawer,
    ShaderError,
    build_program,
    upload_uniforms,
)

RIBBON_WIDTH = 360
RIBBON_HEIGHT = 32

# The steady-state phase; what the lab is most useful showing on startup.
DEFAULT_PHASE = "flow"


class RibbonArea(Gtk.GLArea):
    """The `Gtk.GLArea` the ribbon shader renders into."""

    def __init__(self, shader: dict, model: RibbonModel, state: "LabState") -> None:
        super().__init__()
        self._shader = shader
        self._model = model
        self._state = state
        self._program = None
        self._drawer = None
        self._error = None
        # No set_has_alpha() here: GTK3 needed it, GTK4 removed it because
        # the area renders into a texture GSK composites, so the ribbon is
        # already drawn over the window background — exactly as it is over
        # the Shell's HUD pill, which is what makes premultiplied blending
        # visible here rather than only in a live session.
        self.set_size_request(RIBBON_WIDTH, RIBBON_HEIGHT * 2)
        self.connect("realize", self._on_realize)
        self.connect("render", self._on_render)

    def _on_realize(self, _area) -> None:
        self.make_current()
        if self.get_error() is not None:
            return
        try:
            self._program = build_program(self._shader, PROFILE_ES300)
            self._drawer = QuadDrawer(PROFILE_ES300)
        except ShaderError as error:
            # Kept and shown in the window rather than raised: a shader that
            # will not compile is the single most likely thing to go wrong
            # here, and showing the driver's log is what the lab is for.
            self._error = str(error)
            print(self._error, file=sys.stderr)

    def _on_render(self, _area, _context) -> bool:
        GL.glClearColor(0.0, 0.0, 0.0, 0.0)
        GL.glClear(GL.GL_COLOR_BUFFER_BIT)
        if self._program is None:
            return True

        width = self.get_width()
        height = self.get_height()
        try:
            response = self._model.frame(
                # No palette passed: the bridge defaults it to the live
                # desktop accent, so the lab tracks the user's colour the
                # same way the Shell HUD does.
                width=width, height=height, **self._state.request(),
            )
        except BridgeError as error:
            self._error = str(error)
            return True

        GL.glUseProgram(self._program)
        # Premultiplied, matching how the shader accumulates and how the
        # Shell composites the actor.
        GL.glEnable(GL.GL_BLEND)
        GL.glBlendFunc(GL.GL_ONE, GL.GL_ONE_MINUS_SRC_ALPHA)
        upload_uniforms(self._program, self._shader["uniforms"], response["uniforms"])
        self._drawer.draw()
        self._state.last_info = response["info"]
        self._state.last_desktop = response["desktop"]
        return True


class LabState:
    """Everything the sliders control, in the shape `bridge.js` expects."""

    def __init__(self, default_phase: str) -> None:
        self.envelope = 0.7
        self.phase = default_phase
        self.severity_tint = None
        self.reduced_motion = False
        self.playing = True
        self.elapsed_ms = 0.0
        self.phase_elapsed_ms = 0.0
        self.last_info = {}
        self.last_desktop = {}

    def advance(self, delta_ms: float) -> None:
        if not self.playing:
            return
        self.elapsed_ms += delta_ms
        self.phase_elapsed_ms += delta_ms

    def set_phase(self, phase: str) -> None:
        if phase == self.phase:
            return
        self.phase = phase
        # Restarted so a phase with an entrance animation (unfold, morph)
        # actually plays it when selected, instead of appearing mid-way.
        self.phase_elapsed_ms = 0.0

    def request(self) -> dict:
        return {
            "envelope": self.envelope,
            "elapsedMs": self.elapsed_ms,
            "phase": self.phase,
            "phaseElapsedMs": self.phase_elapsed_ms,
            "reducedMotion": self.reduced_motion,
            "severityTint": self.severity_tint,
        }


class LabWindow(Adw.ApplicationWindow):
    def __init__(self, app, shader: dict, model: RibbonModel) -> None:
        super().__init__(application=app, title="Myna ribbon — GPU lab")
        self.set_default_size(560, 420)
        self._shader = shader
        # Named, not indexed: `phases` comes from RibbonPhase, whose members
        # change (relax was removed from it), and an index silently selects
        # a different default rather than failing.
        self._state = LabState(DEFAULT_PHASE if DEFAULT_PHASE in shader["phases"]
                               else shader["phases"][0])
        # Seeded from the startup snapshot so the window opens in the right
        # variant rather than flipping on the first rendered frame.
        self._state.last_desktop = shader["desktop"]
        self._state.reduced_motion = shader["desktop"]["reducedMotion"]
        self._applied_color_scheme = None
        self._apply_color_scheme(shader["desktop"]["colorScheme"])
        self._area = RibbonArea(shader, model, self._state)

        self._info = Gtk.Label(xalign=0.0, wrap=True)
        self._info.add_css_class("dim-label")

        controls = Adw.PreferencesGroup(title="Model inputs")
        controls.add(self._level_row())
        controls.add(self._phase_row())
        controls.add(self._tint_row())
        controls.add(self._switch_row(
            "Reduced motion", "A flat, static strand (accessibility path)",
            lambda active: setattr(self._state, "reduced_motion", active),
            default=self._state.reduced_motion))
        controls.add(self._switch_row(
            "Animate", "Advance the clock the shader reads",
            lambda active: setattr(self._state, "playing", active), default=True))

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                      margin_top=12, margin_bottom=12,
                      margin_start=12, margin_end=12)
        frame = Gtk.Frame()
        frame.set_child(self._area)
        box.append(frame)
        box.append(self._info)
        box.append(controls)
        box.append(self._shell_group())

        content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        content.append(Adw.HeaderBar())
        scroller = Gtk.ScrolledWindow(vexpand=True)
        scroller.set_child(box)
        content.append(scroller)
        self.set_content(content)

        self._last_frame_us = None
        self.add_tick_callback(self._on_tick)

    def _level_row(self) -> Adw.ActionRow:
        row = Adw.ActionRow(
            title="Input level",
            subtitle="The smoothed envelope the Shell derives from the mic")
        scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0.0, 1.0, 0.01)
        scale.set_value(self._state.envelope)
        scale.set_hexpand(True)
        scale.set_size_request(240, -1)
        scale.set_draw_value(True)
        scale.connect("value-changed",
                      lambda s: setattr(self._state, "envelope", s.get_value()))
        row.add_suffix(scale)
        return row

    def _phase_row(self) -> Adw.ComboRow:
        phases = self._shader["phases"]
        row = Adw.ComboRow(title="Lifecycle phase",
                           model=Gtk.StringList.new(phases))
        row.set_selected(phases.index(self._state.phase))

        def on_selected(r, _p=None):
            phase = phases[r.get_selected()]
            self._state.set_phase(phase)
            r.set_subtitle(self._phase_subtitle(phase))

        row.connect("notify::selected", on_selected)
        row.set_subtitle(self._phase_subtitle(self._state.phase))
        self._phase_combo = row
        return row

    def _phase_subtitle(self, phase: str) -> str:
        """Say what the Shell will do with this phase, if anything.

        The lab can select phases the wire cannot express — `relax` is
        implemented in ribbon.js but no dictation state ever requests it —
        and without this the only symptom is a phase that moves the lab's
        ribbon and leaves the Shell's alone, which reads as a broken bridge
        rather than as the shipped extension never using it.
        """
        state, _ = wire_state(phase, self._state.severity_tint)
        rendered = shell_phase(phase, self._state.severity_tint)
        if rendered is None:
            return f"publishes {state}; the Shell's ribbon keeps its phase"
        if rendered == phase:
            return f"publishes {state}; the Shell renders it the same way"
        if phase in SHELL_INTERNAL_PHASES:
            return (f"publishes {state}; the Shell plays {phase} itself when "
                    f"the pill appears, then settles into {rendered}")
        return (f"publishes {state}, which the Shell renders as {rendered} — "
                f"{phase} is lab-only, no dictation state requests it")

    def _tint_row(self) -> Adw.ComboRow:
        # The values are the real `Severity` strings ribbon.js compares
        # against, not display labels: this row used to pass "amber", which
        # is the *tint* name (RibbonTint.AMBER) rather than the severity, so
        # `severityTint === Severity.RECOVERABLE` never matched and the
        # option silently did nothing. Publishing on D-Bus made that visible
        # — the wire needs the severity anyway.
        labels = ["none", "recoverable (notice)", "critical (error)"]
        values = [None, "recoverable", "critical"]
        row = Adw.ComboRow(title="Severity",
                           subtitle="Amber pulse for a notice; the pill goes "
                                    "red and the ribbon hides for an error",
                           model=Gtk.StringList.new(labels))
        def on_selected(r, _p=None):
            self._state.severity_tint = values[r.get_selected()]
            # A severity outranks the phase on the wire, so the phase row's
            # explanation of what the Shell will show changes with it.
            self._phase_combo.set_subtitle(
                self._phase_subtitle(self._state.phase))

        row.connect("notify::selected", on_selected)
        return row

    def _switch_row(self, title, subtitle, on_change, default=False) -> Adw.ActionRow:
        row = Adw.ActionRow(title=title, subtitle=subtitle)
        switch = Gtk.Switch(active=default, valign=Gtk.Align.CENTER)
        switch.connect("notify::active", lambda s, _p: on_change(s.get_active()))
        row.add_suffix(switch)
        row.set_activatable_widget(switch)
        return row

    def _shell_group(self) -> Adw.PreferencesGroup:
        """Drive a live GNOME Shell HUD from these same controls."""
        group = Adw.PreferencesGroup(
            title="GNOME Shell",
            description="Claim org.myna.Dictation and publish the state "
                        "these controls describe, so the real HUD pill "
                        "follows the lab. Stop myna-desktop first — the "
                        "name is not taken by force.")
        self._publisher = DictationPublisher(self._state.request)
        self._publisher.on_status_changed = self._on_publish_status
        self._publish_row = Adw.ActionRow(
            title="Publish on the session bus",
            subtitle="off — the extension sees no daemon and stays dormant")
        switch = Gtk.Switch(active=False, valign=Gtk.Align.CENTER)
        switch.connect("notify::active", lambda s, _p: (
            self._publisher.start() if s.get_active() else self._publisher.stop()))
        self._publish_row.add_suffix(switch)
        self._publish_row.set_activatable_widget(switch)
        group.add(self._publish_row)
        # Released explicitly on close: without this the name would linger
        # until the process actually exits, and a Shell watching it would
        # keep showing a pill for a lab that is already gone.
        self.connect("close-request", lambda _w: (self._publisher.stop(), False)[1])
        return group

    def _on_publish_status(self, status: str) -> None:
        self._publish_row.set_subtitle({
            "off": "off — the extension sees no daemon and stays dormant",
            "connecting": "claiming the name…",
            "publishing": "publishing — the HUD pill now follows these controls",
        }.get(status, status))

    def _on_tick(self, _widget, clock) -> bool:
        now_us = clock.get_frame_time()
        if self._last_frame_us is not None:
            self._state.advance((now_us - self._last_frame_us) / 1000.0)
        self._last_frame_us = now_us
        self._area.queue_render()

        self._apply_color_scheme(self._state.last_desktop.get("colorScheme"))

        info = self._state.last_info
        accent = self._state.last_desktop.get("palette", {}).get("main", "?")
        error = getattr(self._area, "_error", None)
        # The published State is shown here rather than in the D-Bus row so
        # the model facts and the wire value sit side by side: if the pill
        # in the Shell disagrees with the ribbon in this window, this line
        # is where the two stories diverge.
        wire = (f" · wire {self._publisher.state}"
                if self._publisher.status == "publishing" else "")
        self._info.set_text(
            f"shader error: {error}" if error else
            f"{info.get('strands', 0)} strands · {info.get('dots', 0)} dots · "
            f"tint {info.get('tint') or 'none'} · accent {accent} · "
            f"t={self._state.elapsed_ms / 1000:.1f}s{wire}")
        return GLib.SOURCE_CONTINUE

    def _apply_color_scheme(self, scheme: str | None) -> None:
        """Follow the desktop's light/dark preference.

        Forced rather than left on Adw.ColorScheme.DEFAULT: DEFAULT asks
        libadwaita to detect the system preference itself, which goes
        through the settings portal and silently stays light wherever that
        portal is unavailable (a jhbuild session, a plain X/Wayland login
        without xdg-desktop-portal-gnome). The value read straight from
        GSettings on the JS side is authoritative, so it is applied
        directly.
        """
        if scheme is None or scheme == self._applied_color_scheme:
            return
        self._applied_color_scheme = scheme
        Adw.StyleManager.get_default().set_color_scheme({
            "prefer-dark": Adw.ColorScheme.FORCE_DARK,
            "prefer-light": Adw.ColorScheme.FORCE_LIGHT,
        }.get(scheme, Adw.ColorScheme.DEFAULT))


def main() -> int:
    try:
        shader = load_shader()
    except BridgeError as error:
        print(error, file=sys.stderr)
        return 1

    model = RibbonModel()
    app = Adw.Application(application_id="com.canonical.myna.RibbonGpuLab")
    app.connect("activate", lambda a: LabWindow(a, shader, model).present())
    try:
        return app.run([])
    finally:
        model.close()


if __name__ == "__main__":
    sys.exit(main())
