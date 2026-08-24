#!/usr/bin/env -S gjs -m
// gpu-probe.js — check that the GPU ribbon path's toolkit API is actually
// reachable from GJS, and that the generated shader is accepted by Cogl's
// snippet API (feature 004-gnome-shell-indicator, 2026-08-21 GPU pass).
//
// This is NOT a unit test (test/ribbonGlsl.test.js covers the generator
// headlessly). It exists because the GPU path depends on introspection
// details that cannot be verified without mutter's typelibs present, and
// because `gjs -m` cannot take `-c`/`-e` — a module needs a real file.
//
// It does not need a display server: it only constructs the snippet and
// inspects the effect class. Actually RENDERING requires a live Shell.
//
//     GI_TYPELIB_PATH=/opt/dev/GNOME/lib/mutter-51 \
//     LD_LIBRARY_PATH=/opt/dev/GNOME/lib/mutter-51 \
//     gjs -m test/gpu-probe.js
//
// Exit codes follow entrance-visual.sh: 0 pass, 1 fail, 77 cannot judge. A
// Clutter that predates the CoglSnippet port is a skip, not a failure -
// Shell 50 is half of metadata.json's declared range and hud.js is
// *expected* to fall back to Cairo there, so failing would mean CI could
// never run this at all. What still fails on such a Shell is
// `ribbonShaderSupported()` disagreeing with what Clutter offers: that is
// the gate itself being wrong, and it is the one thing this file can catch
// that no other suite can.

import System from 'system';

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GObject from 'gi://GObject';

import {buildRibbonShader, RIBBON_UNIFORMS} from '../ribbonGlsl.js';
import {ribbonShaderSupported} from '../ribbonShader.js';

let failures = 0;

function check(name, condition, detail = '') {
    if (condition) {
        print(`ok   ${name}${detail ? ` ${detail}` : ''}`);
    } else {
        failures++;
        print(`FAIL ${name}${detail ? ` ${detail}` : ''}`);
    }
}

// --- 1. The API the GPU path relies on exists --------------------------

check('Clutter.ShaderEffect is introspectable',
    typeof Clutter.ShaderEffect === 'function');
check('Cogl.Snippet.new is callable',
    typeof Cogl.Snippet.new === 'function');
check('Cogl.SnippetHook.FRAGMENT is defined',
    Cogl.SnippetHook.FRAGMENT !== undefined,
    `(= ${Cogl.SnippetHook.FRAGMENT})`);
// Not a check but a capability: `set_uniform_float` arrived with the same
// CoglSnippet port as the vfunc (the variadic `set_uniform` is not usable
// from GJS at all), so its absence says "this is a Shell 50" - which is a
// supported Shell, not a fault.
print('     (info) set_uniform_float: ' +
    `${typeof Clutter.ShaderEffect.prototype.set_uniform_float === 'function'}`);
// NOT `'vfunc_get_static_snippet' in Clutter.ShaderEffect.prototype`: GJS's
// resolve hook turns that `in` into a lookup of the base class's own
// implementation and THROWS "Virtual function not implemented" even where
// the vfunc exists and is perfectly overridable. Registering a subclass is
// the only honest test, so section 3 does it in a try/catch and this check
// reports what that found.

// ShellGLSLEffect was removed in gnome-shell 30f545eb00 ("Remove GLSLEffect
// — now that everything uses ClutterShaderEffect"); confirm we did not
// accidentally depend on it coming back.
let shellHasGlslEffect = false;
try {
    const Shell = (await import('gi://Shell')).default;
    shellHasGlslEffect = typeof Shell.GLSLEffect === 'function';
} catch {
    print('     (info) gi://Shell not loadable here — expected outside a Shell process');
}
print(`     (info) Shell.GLSLEffect present: ${shellHasGlslEffect} (we do not use it)`);

// --- 2. Cogl accepts the generated shader source -----------------------

const {declarations, code} = buildRibbonShader();
print(`     (info) declarations: ${declarations.length} bytes, replace body: ${code.length} bytes`);

const snippet = Cogl.Snippet.new(Cogl.SnippetHook.FRAGMENT, declarations, null);
snippet.set_replace(code);
check('Cogl accepted the generated snippet', snippet !== null);

// --- 3. The effect subclass registers and takes every uniform ----------

// `get_static_snippet` only exists from mutter 51.alpha (2d5bc0fbff,
// "clutter/shader-effect: Port to CoglSnippet"). On anything older GJS cannot
// hook the override up and registerClass throws — which is precisely the
// condition this probe exists to report, so catch it rather than dying on it.
// hud.js does the same, via ribbonShader.js's `ribbonShaderSupported()`.
let ProbeEffect = null;
try {
    class Probe extends Clutter.ShaderEffect {
        static {
            GObject.registerClass(this);
        }

        vfunc_get_static_snippet() {
            const s = Cogl.Snippet.new(Cogl.SnippetHook.FRAGMENT, declarations, null);
            s.set_replace(code);
            return s;
        }
    }
    ProbeEffect = Probe;
} catch (e) {
    print(`     (info) ${e.message}`);
    print('     (info) this Clutter predates the CoglSnippet port (mutter < 51);');
    print('     (info) hud.js falls back to the Cairo ribbon here.');
}
// Also a capability rather than a check, and for the same reason as
// set_uniform_float above: on Shell 50 the vfunc is absent by construction.
print(`     (info) get_static_snippet overridable: ${ProbeEffect !== null}`);

// The gate hud.js reads, exercised the way hud.js exercises it. The two have
// to agree in both directions: a gate that says yes where the vfunc is
// absent breaks the extension outright, and one that says no where it is
// present silently costs every user the GPU path.
check('ribbonShaderSupported() agrees with this Clutter',
    ribbonShaderSupported() === (ProbeEffect !== null),
    `(gate ${ribbonShaderSupported()}, probe ${ProbeEffect !== null})`);

if (ProbeEffect === null) {
    // Nothing left to probe, but a disagreeing gate is still a failure.
    print(failures === 0
        ? 'SKIP gpu-probe.js'
        : `FAIL gpu-probe.js (${failures})`);
    System.exit(failures === 0 ? 77 : 1);
}

const effect = new ProbeEffect();
check('the shader effect subclass instantiates', effect instanceof Clutter.ShaderEffect);

// Uniform uploads are queued on the effect and only reach a real GL program
// at paint time, so this proves the marshalling works, not that the driver
// linked them. The 4-component ceiling is ClutterShaderFloat's: exceeding it
// trips a g_return_if_fail rather than throwing, so watch for
// "clutter_value_set_shader_float: assertion 'size <= 4' failed" on stderr —
// it does not register as a failure here.
for (const {name, components} of RIBBON_UNIFORMS) {
    try {
        effect.set_uniform_float(name, components, new Array(components).fill(0));
        print(`ok   uniform ${name} (${components} components) accepted`);
    } catch (e) {
        failures++;
        print(`FAIL uniform ${name}: ${e.message}`);
    }
}

print(failures === 0 ? 'PASS gpu-probe.js' : `FAIL gpu-probe.js (${failures})`);
System.exit(failures === 0 ? 0 : 1);
