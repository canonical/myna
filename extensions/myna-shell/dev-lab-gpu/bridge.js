#!/usr/bin/env -S gjs -m
// bridge.js — exposes the JS ribbon to a non-JS renderer as JSON.
//
// The GPU dev-lab is Python (see README.md: only PyOpenGL can reach the raw
// GL entry points a standalone GtkGLArea needs), but nothing about the
// ribbon should live there. This is the seam: Python renders, JS decides
// everything that is rendered.
//
// Both the shader source and the per-frame uniform values come from the
// very same modules the Shell extension loads — `ribbonGlsl.js` generates
// the GLSL, `ribbon.js` computes the model, `packRibbonUniforms` packs it —
// so the lab cannot drift from the shipped renderer. If it looks right
// here, it is right there.
//
//   gjs -m bridge.js --shader     one JSON object: the generated shader
//   gjs -m bridge.js --serve      a JSON line per JSON line of stdin
//
// Anything that needs Clutter, St or Cogl is deliberately NOT imported, so
// this runs under a plain gjs with no Shell present.

import Gio from 'gi://Gio';
import GioUnix from 'gi://GioUnix';
import GLib from 'gi://GLib';
import System from 'system';

import {SystemPreferences} from '../accent.js';
import {computeRibbonModel, RibbonPhase} from '../ribbon.js';
import {
    buildRibbonShader,
    packRibbonUniforms,
    RIBBON_UNIFORMS,
} from '../ribbonGlsl.js';

// The desktop's real accent colour, resolved by the extension's own
// accent.js — including R18's "did the user actually choose one?"
// distinction and the Ubuntu-orange/aubergine fallbacks. Hardcoding a
// palette here would have made the lab the one place the ribbon does *not*
// follow the user's accent, which is precisely the kind of drift this
// bridge exists to prevent.
const prefs = new SystemPreferences();
prefs.enable();

const COLOR_SCHEME_SCHEMA = 'org.gnome.desktop.interface';
const COLOR_SCHEME_KEY = 'color-scheme';

/**
 * The desktop's light/dark preference, so the lab window can follow it.
 *
 * accent.js owns accent + reduced-motion but not this, and it is read here
 * rather than added there because the shipped extension has no use for it:
 * the Shell HUD is drawn on the Shell's own dark chrome regardless.
 *
 * @returns {string} 'default', 'prefer-dark' or 'prefer-light' — falling
 *     back to 'default' whenever the schema or key is unavailable, the same
 *     never-throw contract accent.js follows (R18).
 */
function readColorScheme() {
    const source = Gio.SettingsSchemaSource.get_default();
    const schema = source?.lookup(COLOR_SCHEME_SCHEMA, true) ?? null;
    if (schema === null || !schema.has_key(COLOR_SCHEME_KEY))
        return 'default';
    return new Gio.Settings({settings_schema: schema}).get_string(COLOR_SCHEME_KEY);
}

/** Everything the lab mirrors from the desktop, refreshed per frame. */
function desktop() {
    return {
        palette: prefs.accentPalette,
        reducedMotion: prefs.reducedMotion,
        colorScheme: readColorScheme(),
    };
}

function emitShader() {
    const {declarations, code} = buildRibbonShader();
    print(JSON.stringify({
        declarations,
        code,
        uniforms: RIBBON_UNIFORMS,
        phases: Object.values(RibbonPhase),
        desktop: desktop(),
    }));
}

/**
 * Compute one frame's uniforms.
 *
 * @param {object} request - the decoded stdin line.
 * @returns {object} uniform name → floats, plus the model facts a lab
 *     wants to display but the shader does not consume.
 */
function frame(request) {
    const live = desktop();
    const {
        width = 360, height = 32,
        envelope = 0, elapsedMs = 0,
        phase = RibbonPhase.FLOW, phaseElapsedMs = 0,
        reducedMotion = false, severityTint = null,
        palette = live.palette,
    } = request;

    const model = computeRibbonModel({
        envelope, elapsedMs, phase, phaseElapsedMs, reducedMotion, severityTint,
    });
    return {
        uniforms: packRibbonUniforms(width, height, model, palette),
        // Not uniforms — just what the lab prints alongside the render, so
        // a surprising picture can be traced back to the model that
        // produced it without re-deriving anything in Python.
        info: {
            strands: model.strands.length,
            dots: model.dots ? model.dots.length : 0,
            tint: model.tint ?? null,
        },
        // Sent every frame, not just at startup, so changing the accent or
        // the light/dark preference is reflected live — the same thing the
        // Shell HUD does via its `changed::` subscriptions.
        desktop: live,
    };
}

function serve() {
    const stdin = new Gio.DataInputStream({
        base_stream: new GioUnix.InputStream({fd: 0, close_fd: false}),
    });
    // Written through an explicit stream rather than print(): stdout is a
    // pipe here, not a tty, so it is block-buffered and the lab would block
    // forever on a reply that had been written but not flushed.
    const stdout = new Gio.DataOutputStream({
        base_stream: new GioUnix.OutputStream({fd: 1, close_fd: false}),
    });
    for (;;) {
        const [line] = stdin.read_line_utf8(null);
        if (line === null)
            return; // EOF: the lab exited.
        if (line.trim() === '')
            continue;
        // SystemPreferences refreshes itself from `changed::` signals, which
        // are dispatched by the main context — and this blocking read loop
        // never runs one. Draining whatever is pending, without blocking,
        // is what makes a live accent change actually reach the ribbon.
        const context = GLib.MainContext.default();
        while (context.pending())
            context.iteration(false);
        let response;
        try {
            response = frame(JSON.parse(line));
        } catch (e) {
            // Reported rather than thrown: a bad frame should not take the
            // lab down mid-session, and the message is more useful on the
            // Python side where it can be shown in the window.
            response = {error: `${e}`};
        }
        stdout.put_string(`${JSON.stringify(response)}\n`, null);
        stdout.flush(null);
    }
}

const argv = ARGV;
if (argv.includes('--shader'))
    emitShader();
else if (argv.includes('--serve'))
    serve();
else {
    printerr('usage: gjs -m bridge.js [--shader | --serve]');
    System.exit(2);
}
