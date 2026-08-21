// gettext.js — translation binding that works in BOTH contexts the extension
// runs in, so the rest of the code can `import { gettext as _ }` unconditionally
// (like any GNOME Shell extension) without dragging a Shell dependency into the
// pure modules.
//
//   - Inside GNOME Shell: re-export the domain-bound `gettext`/`ngettext`/
//     `pgettext` from the Shell's extension.js. These resolve the catalog from
//     this extension's `gettext-domain` (metadata.json) automatically.
//   - Under plain gjs (the Shell-free contract tests, e.g. test/states.test.js):
//     the `resource:///org/gnome/shell/...` module isn't registered, so the
//     dynamic import rejects and we fall back to identity stubs — the msgid is
//     returned verbatim, which is exactly the English source the tests assert.
//
// Top-level await keeps this synchronous from an importer's point of view: a
// module that `import`s us only evaluates its own body once our binding is
// settled, so `_` is never the wrong function at first use.

async function loadShellGettext() {
    try {
        return await import('resource:///org/gnome/shell/extensions/extension.js');
    } catch {
        // Not running inside GNOME Shell (pure gjs test harness): fall back to
        // the identity stubs below so strings pass through as their English source.
        return null;
    }
}

const shell = await loadShellGettext();

export const gettext = shell?.gettext ?? ((s) => s);
export const ngettext = shell?.ngettext ?? ((s, p, n) => (n === 1 ? s : p));
export const pgettext = shell?.pgettext ?? ((_context, s) => s);

// Mark-only alias (the conventional `N_`): flags a string for xgettext
// extraction without translating it now. Use it for msgids built at
// module-import time (e.g. static tables), then translate at call time with
// `gettext` — the Shell's domain-bound `gettext` may only run once the
// extension is registered, which import-time code precedes.
export const N_ = (s) => s;
