#!/usr/bin/env -S gjs -m
// dev-lab/main.js — standalone GTK4 + libadwaita tuning app for the wave
// ribbon (feature 004-gnome-shell-indicator, 2026-07-30 wave-ribbon
// redesign; research R20). NOT part of the shipped extension bundle
// (excluded from ../metadata.json and the install step — see README.md).
//
// Shares the exact same pure/paint modules the shipped `hud.js` uses
// (`../ribbon.js`, `../ribbon-paint.js`, `../accent.js`) and reuses
// `../dbus.js`'s `DictationService` UNMODIFIED for a genuinely live
// `org.myna.Dictation` connection — confirmed zero Shell/St/Clutter
// dependency (pure Gio/GLib), so it runs unchanged outside the Shell
// process. This means: tune here, `hud.js` draws pixel-identical output —
// no separate "port to the extension" step, ever.
//
// Launch:  gjs -m dev-lab/main.js        (from extensions/myna-shell/)
//
// Session start/stop stays hotkey-driven (org.myna.Dictation's Start/Stop/
// Toggle methods are still an unimplemented stub, US4/DbusTrigger — out of
// scope here); this tool is a passive dictation target plus a manual-
// override tuning harness layered on top of the live connection.

// Versions go in the import specifier, not `imports.gi.versions`: ESM
// imports are hoisted, so an assignment here runs after the modules load.
import Adw from 'gi://Adw?version=1';
import Gtk from 'gi://Gtk?version=4.0';
import GLib from 'gi://GLib';

import {DictationService} from '../dbus.js';
import {ribbonPhaseForStateKey, ribbonVisibleForSeverity} from '../hud-logic.js';
import {
    applyEnvelopeSmoothing,
    computeEnvelope,
    computeRibbonModel,
    DEFAULT_ENVELOPE_HZ,
    DEFAULT_STRAND_COUNT,
    DEFAULT_POINTS_PER_STRAND,
    UNFOLD_MS,
} from '../ribbon.js';
import {paintRibbon} from '../ribbon-paint.js';
import {SystemPreferences} from '../accent.js';

const APP_ID = 'org.myna.RibbonLab';
const CANVAS_WIDTH = 420;
const CANVAS_HEIGHT = 100;

const app = new Adw.Application({application_id: APP_ID});

app.connect('activate', () => {
    Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.PREFER_DARK);

    // ── Model state (mirrors hud.js's WaveRibbonActor, standalone) ────────
    const model = {
        rms: 0,
        peak: 0,
        lastLevelAt: 0,
        manualOverride: false,
        manualLevel: 0,
        // The SMOOTHED envelope actually driving the wave shape (~300 ms
        // one-pole low-pass, 2026-07-30 design refinement) — distinct from
        // vumeter.js's arrival-time stale-decay. Updated once per draw via
        // applyEnvelopeSmoothing so ribbon.js itself stays pure.
        smoothedEnvelope: 0,
        lastDrawAt: 0,
        phase: 'unfold',
        phaseStartedAt: GLib.get_monotonic_time(),
        startedAt: GLib.get_monotonic_time(),
        strandCount: DEFAULT_STRAND_COUNT,
        pointCount: DEFAULT_POINTS_PER_STRAND,
        envelopeHz: DEFAULT_ENVELOPE_HZ,
        simulatedSeverity: null, // null | 'recoverable' | 'critical'
    };

    function setPhase(phase) {
        if (phase === model.phase)
            return;
        model.phase = phase;
        model.phaseStartedAt = GLib.get_monotonic_time();
    }

    // Same unfold→flow auto-advance hud.js's actor does. hud.js gets this
    // per session from `WaveRibbonActor.reset()`, which HudView calls each
    // time the pill goes from hidden to shown (2026-08-20: the actor is
    // reused now, not rebuilt); dev-lab uses one long-lived model for the
    // app's whole lifetime instead, so it must explicitly re-arm this
    // whenever a new session starts after a previous one reached
    // 'morph'/'complete' — otherwise the ribbon stays pinned at its
    // post-completion amplitude forever and never reacts to a second
    // session's real audio (bug found via live testing, 2026-07-30).
    function armUnfoldAutoAdvance() {
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, UNFOLD_MS, () => {
            if (model.phase === 'unfold')
                setPhase('flow');
            return GLib.SOURCE_REMOVE;
        });
    }
    armUnfoldAutoAdvance();

    // ── Live D-Bus connection (T060) — dbus.js reused verbatim ────────────
    const service = new DictationService({
        onStateChanged: (state, _errorMessage) => {
            statusLabel.set_label(`org.myna.Dictation: ${state}`);
            // A new session (loading/recording) arriving while the ribbon is
            // still sitting in a *previous* session's post-recording phase
            // (morph/complete) means this is a fresh session starting, not
            // a continuation — replay the unfold, mirroring hud.js's
            // fresh-actor-per-session behaviour.
            const isSessionStart = state === 'loading' || state === 'recording';
            const isPostRecordingPhase = model.phase === 'morph' || model.phase === 'complete';
            if (isSessionStart && isPostRecordingPhase) {
                setPhase('unfold');
                armUnfoldAutoAdvance();
            }
            const forced = ribbonPhaseForStateKey(state);
            if (forced !== null)
                setPhase(forced);
        },
        onAvailabilityChanged: available => {
            statusLabel.set_label(available
                ? 'org.myna.Dictation: connected'
                : 'org.myna.Dictation: not running (dormant)');
        },
        onLevel: (rms, peak) => {
            if (!model.manualOverride) {
                model.rms = rms;
                model.peak = peak;
                model.lastLevelAt = GLib.get_monotonic_time();
            }
        },
    });
    service.enable();

    // ── Accent-color / reduced-motion for the standalone GTK demo ──────────
    const prefs = new SystemPreferences();
    prefs.enable();
    let manualReducedMotion = null; // null = follow the system; else override

    // ── Window chrome ──────────────────────────────────────────────────────
    const win = new Adw.ApplicationWindow({
        application: app,
        title: 'Myna Ribbon Lab',
        default_width: 600,
        default_height: 840,
    });

    const toastOverlay = new Adw.ToastOverlay();
    const toolbarView = new Adw.ToolbarView();
    toolbarView.add_top_bar(new Adw.HeaderBar());

    const content = new Gtk.Box({
        orientation: Gtk.Orientation.VERTICAL,
        spacing: 12,
        margin_top: 12,
        margin_bottom: 12,
        margin_start: 12,
        margin_end: 12,
    });

    // Ribbon canvas ---------------------------------------------------------
    const canvasFrame = new Gtk.Frame();
    const canvas = new Gtk.DrawingArea({
        content_width: CANVAS_WIDTH,
        content_height: CANVAS_HEIGHT,
    });
    canvas.set_draw_func((_area, cr, width, height) => {
        // 2026-07-30, R17a: only 'critical' hides the ribbon entirely; a
        // simulated 'recoverable' severity stays visible, tinted amber and
        // gently pulsing (mirrors hud-logic.js's ribbonVisibleForSeverity).
        if (!ribbonVisibleForSeverity(model.simulatedSeverity))
            return;
        const now = GLib.get_monotonic_time();
        const ageMs = model.lastLevelAt ? (now - model.lastLevelAt) / 1000 : 9999;
        const rms = model.manualOverride ? model.manualLevel : model.rms;
        const peak = model.manualOverride ? model.manualLevel : model.peak;
        const instantEnvelope = model.manualOverride
            ? Math.max(0, Math.min(1, model.manualLevel))
            : computeEnvelope(rms, peak, ageMs);
        const dtMs = model.lastDrawAt ? (now - model.lastDrawAt) / 1000 : 1000 / model.envelopeHz;
        model.smoothedEnvelope = applyEnvelopeSmoothing(model.smoothedEnvelope, instantEnvelope, dtMs);
        model.lastDrawAt = now;

        const reducedMotion = manualReducedMotion !== null
            ? manualReducedMotion
            : prefs.reducedMotion;
        const ribbonModel = computeRibbonModel({
            envelope: model.smoothedEnvelope,
            elapsedMs: (now - model.startedAt) / 1000,
            phase: model.phase,
            phaseElapsedMs: (now - model.phaseStartedAt) / 1000,
            reducedMotion,
            severityTint: model.simulatedSeverity,
            strandCount: model.strandCount,
            pointCount: model.pointCount,
        });
        paintRibbon(cr, width, height, ribbonModel, prefs.accentPalette);
    });
    canvasFrame.set_child(canvas);
    content.append(canvasFrame);

    // Redraw on GTK's FRAME CLOCK, not a fixed-Hz GLib timeout — the same
    // change hud.js made (there via a Clutter.Timeline bound to the actor,
    // 2026-08-20). A timer running at some rate other than the display's
    // beats against vsync and shows up as judder, so tuning the ribbon
    // against one would be tuning against motion the shipped extension
    // never produces.
    canvas.add_tick_callback(() => {
        canvas.queue_draw();
        return GLib.SOURCE_CONTINUE;
    });

    // Status line -------------------------------------------------------------
    const statusLabel = new Gtk.Label({
        label: 'org.myna.Dictation: not running (dormant)',
        halign: Gtk.Align.START,
        css_classes: ['dim-label'],
    });
    content.append(statusLabel);

    // Manual level override ----------------------------------------------------
    const overrideRow = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 8});
    const overrideSwitch = new Gtk.Switch({valign: Gtk.Align.CENTER});
    overrideSwitch.connect('notify::active', sw => {
        model.manualOverride = sw.active;
    });
    const levelScale = new Gtk.Scale({
        orientation: Gtk.Orientation.HORIZONTAL,
        adjustment: new Gtk.Adjustment({lower: 0, upper: 100, step_increment: 1}),
        hexpand: true,
        draw_value: true,
    });
    levelScale.connect('value-changed', scale => {
        model.manualLevel = scale.get_value() / 100;
    });
    overrideRow.append(new Gtk.Label({label: 'Manual level override:'}));
    overrideRow.append(overrideSwitch);
    overrideRow.append(levelScale);
    content.append(overrideRow);

    // Phase trigger buttons ---------------------------------------------------
    const phaseRow = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 6, homogeneous: true});
    for (const phase of ['unfold', 'flow', 'relax', 'morph', 'complete']) {
        const button = new Gtk.Button({label: phase});
        button.connect('clicked', () => setPhase(phase));
        phaseRow.append(button);
    }
    content.append(new Gtk.Label({label: 'Lifecycle phase', halign: Gtk.Align.START, css_classes: ['heading']}));
    content.append(phaseRow);

    // Severity simulation buttons ----------------------------------------------
    const severityRow = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 6, homogeneous: true});
    for (const sev of ['recoverable', 'critical', 'clear']) {
        const button = new Gtk.Button({label: sev});
        button.connect('clicked', () => {
            model.simulatedSeverity = sev === 'clear' ? null : sev;
            canvas.queue_draw();
        });
        severityRow.append(button);
    }
    content.append(new Gtk.Label({label: 'Severity (recoverable = amber/paused; critical = hidden)', halign: Gtk.Align.START, css_classes: ['heading']}));
    content.append(severityRow);

    // Reduced motion toggle -----------------------------------------------------
    const motionRow = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 8});
    const motionSwitch = new Gtk.Switch({valign: Gtk.Align.CENTER});
    motionSwitch.connect('notify::active', sw => {
        manualReducedMotion = sw.active;
    });
    motionRow.append(new Gtk.Label({label: 'Force reduced motion:'}));
    motionRow.append(motionSwitch);
    content.append(motionRow);

    // Tunable sliders -------------------------------------------------------
    function tunableRow(labelText, lower, upper, initial, onChange) {
        const row = new Gtk.Box({orientation: Gtk.Orientation.HORIZONTAL, spacing: 8});
        const scale = new Gtk.Scale({
            orientation: Gtk.Orientation.HORIZONTAL,
            adjustment: new Gtk.Adjustment({lower, upper, step_increment: 1, value: initial}),
            hexpand: true,
            draw_value: true,
        });
        scale.connect('value-changed', s => onChange(s.get_value()));
        row.append(new Gtk.Label({label: labelText, width_chars: 16, halign: Gtk.Align.START}));
        row.append(scale);
        return row;
    }
    content.append(new Gtk.Label({label: 'Tunables (live)', halign: Gtk.Align.START, css_classes: ['heading']}));
    content.append(tunableRow('Strand count', 1, 6, model.strandCount, v => {
        model.strandCount = Math.round(v);
    }));
    content.append(tunableRow('Points/strand', 8, 24, model.pointCount, v => {
        model.pointCount = Math.round(v);
    }));

    // Dictation text-area target (T062) ----------------------------------------
    content.append(new Gtk.Label({label: 'Dictation target', halign: Gtk.Align.START, css_classes: ['heading']}));
    const textView = new Gtk.TextView({
        wrap_mode: Gtk.WrapMode.WORD,
        top_margin: 8, bottom_margin: 8, left_margin: 8, right_margin: 8,
    });
    // Default free-form input purpose (never PASSWORD/PIN) — confirmed non-
    // secure per R20: the injector (client/myna-desktop/src/inject/ibus.rs)
    // only refuses GtkInputPurpose PASSWORD/PIN, with no app/toolkit
    // special-casing at all, so this ordinary text view is a valid IBus
    // dictation target exactly like any other app's text field.
    const scrolled = new Gtk.ScrolledWindow({
        child: textView,
        min_content_height: 120,
        hexpand: true,
        vexpand: true,
    });
    content.append(scrolled);

    const clearButton = new Gtk.Button({label: 'Clear', halign: Gtk.Align.END});
    clearButton.connect('clicked', () => {
        textView.get_buffer().set_text('', -1);
        toastOverlay.add_toast(new Adw.Toast({title: 'Cleared'}));
    });
    content.append(clearButton);

    const clamp = new Adw.Clamp({child: content, maximum_size: 560});
    toolbarView.set_content(new Gtk.ScrolledWindow({child: clamp, vexpand: true}));
    toastOverlay.set_child(toolbarView);
    win.set_content(toastOverlay);

    win.connect('close-request', () => {
        service.disable();
        prefs.disable();
        return false;
    });

    win.present();
    textView.grab_focus();
});

app.run([]);
