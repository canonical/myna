# Preface

Read this before adding desktop-specific behaviour to any client process, writing a backend for a new desktop, or changing a contract in `myna-platform`.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Myna's processes reach the desktop through one porting layer, shaped like Cobalt's Starboard. `myna-platform` is the contract crate: each module is a desktop-neutral operation set with its capability structs and error types, and nothing in it binds GTK, zbus, gio or a desktop service. Each desktop supplies a backend per module, in the crate of the process that runs it: the daemon's in `myna-desktop`, the HUD's in `myna-hud`, Myna Settings' in `myna-config`. The core of a process names a contract, never GNOME, IBus, Mutter, xfconf or X11.

The layer is being introduced in steps: the contracts exist and `text_input` is the one already used by its process; the other modules gain their backends and callers in later work, and until then the GNOME code paths they describe still run directly.

# Important

- Keep `myna-platform` free of internal and toolkit dependencies. A contract that needs a toolkit type is the wrong contract: pass plain data, a callback or a stream.
- Query capabilities, never assume them. A backend reports what it can do (`TextInputCapabilities`); the caller decides policy from that. A backend never decides policy, and wording for the user (headlines, translations) stays with the caller.
- Unknown sessions get `Profile::Generic`, never a desktop-specific profile.
- Every backend of a module, mocks included, runs that module's conformance suite. A gap a fixture cannot cover is asserted as such in its test, not skipped.

# Architecture

## Modules

- `session` - `Session { kind, desktop }` detected from an environment snapshot, and the `Profile` chosen from it.
- `text_input` - `Injector` hands out one `Target` per utterance; the target commits, shows preedit, reports the character before the cursor and focus loss, and releases. `TextInputCapabilities { preedit, surrounding_text, secure_field_detection }` is answered without a live field. Backends: IBus (`myna-desktop/src/inject/ibus.rs`), the mock.
- `activation` - read, bind and clear Myna's toggle accelerator, list the shortcuts holding a key (reserved ones flagged), release a conflict, watch for changes. `Accelerator` is GTK accelerator syntax, which both GSettings media-keys and xfconf `xfce4-keyboard-shortcuts` store, with spelling-insensitive comparison.
- `appearance` - accent, reduced motion and high contrast as plain readings, watched through a callback that says whether readings are already current. Palette and fallback policy stay in the HUD.
- `components` - the desktop pieces Myna needs outside its own processes (GNOME's shell extension; Xfce's status surface autostart entry and IBus as the active input method), each with a purpose and a status, and enabling one as far as Myna can without authorization.
- `status_surface` - pure placement: bottom-centre of the chosen monitor's work area, the monitor chosen by focus then pointer then primary, and the work area an X11 host derives from EWMH struts. It matches the GNOME extension's `place.js`; hosting the window is the backend's.
- `Subscription` - what a watch returns; dropping it ends the watch.

## Profile selection

A process resolves its `Profile` once, at its composition root, with `Profile::select(&SessionEnv::from_process())`. `XDG_CURRENT_DESKTOP` is split on `:` and its first entry Myna knows, case-insensitively, names the desktop. The session kind is Wayland when `WAYLAND_DISPLAY` names a live socket, X11 when only `DISPLAY` is set, and `XDG_SESSION_TYPE` only when neither is, as for a user service started before the session exported them. GNOME on either kind is `Gnome`; Xfce is `Xfce` except on Wayland, where its X11 status surface host cannot run, so it is `Generic`; anything else is `Generic`. `MYNA_PLATFORM=gnome|xfce|generic` overrides detection for tests, and a value naming no profile is an error rather than a fallback.

## Conformance

The `conformance` feature carries the suites (Starboard's NPLB). A suite takes a fixture that sets up a fresh backend and a scripted field, runs every check, panics on a violation and returns a `Report` of checks passed, checks whose field-level assertions the fixture could not observe, and checks the backend's capabilities make inapplicable. The text input suite covers commit delivery, no commit or preedit after focus loss, focus streams taken late, a superseded target, release then reacquire, secure-field refusal where detection is `Supported`, preedit cleared by commit, and preedit inert where unsupported. `myna-desktop/tests/text_input_conformance.rs` runs it against `MockInjector`, which cannot show a field, so its field-level checks are pinned as unobserved.
