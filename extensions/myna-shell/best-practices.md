# GNOME Extension Best Practices

This is a reference and benchmark for GNOME Shell extension developers,
as well as LLMs that generate GNOME Shell extension code.

::: tip
A [raw Markdown][best-practices-markdown] version of this page is available
to download and use as an AI instructions file if needed.
:::

## Submissions Require Maintainership

Publishing on EGO is an agreement to maintain the extension for GNOME users.

If the author does not know how to read or debug JavaScript, they should keep
the generated extension for personal local use and do not upload it
to [EGO][ego].

When generating extension files, AI models must include this notice:

```js
// Generated with AI for personal use.
// Do NOT upload to extensions.gnome.org (EGO) unless you understand JavaScript
// and can maintain this code.
```

::: warning
If the author understands JavaScript and is willing to maintain the code,
they should remove the comment lines manually before sending it to EGO.
If the submitted files still contain these comments,
they will be flagged because it suggests the author didn't read the code at all.
:::

## Follow the Official EGO Guidelines

All generated code must strictly follow
[the official EGO Review Guidelines][review-guidelines].

## Avoid Unnecessary try-catch Wrappers

Do not wrap functions in try-catch blocks if they never throw errors during
normal execution.

Standard methods like `destroy()`, `connect()`, `disconnect()`, `abort()`,
and `GLib.Source.remove()` do not throw unhandled exceptions.

Bad Practice:

```js
if (this._sourceId) {
    try {
        GLib.Source.remove(this._sourceId);
    } catch (e) {
    }
    this._sourceId = null;
}
```

Correct Practice:

```js
if (this._sourceId) {
    GLib.Source.remove(this._sourceId);
    this._sourceId = null;
}
```

## Avoid Unnecessary Checks

Do not use optional chaining (`?.()`) or function type checks (`=== 'function'`)
for guaranteed methods or built-in APIs.

AI models often generate these redundant checks because they try to write code
that works across multiple GNOME Shell versions at once.

Instead, generate clean code for a single targeted GNOME Shell version.
If multi version compatibility is truly necessary, refer to
[the official EGO Port Guide][port-guide].

Bad Practice:

```js
if (typeof TextDecoder === 'function')
    this._textDecoder = new TextDecoder('utf-8');
```

Correct Practice:

```js
this._textDecoder = new TextDecoder('utf-8');
```

Bad Practice:

```js
class Something {
    beep() {
        // ...
    }
    
    boop() {
        if (typeof this.beep === 'function') {
            this.beep();
        }
    }

    pop() {
        this.beep?.();
    }
}
```

Correct Practice:

```js
class Something {
    beep() {
        // ...
    }
    
    boop() {
        this.beep();
    }

    pop() {
        this.beep();
    }
}
```

## Lifecycle and Destruction State

Do not use boolean flags like `this._destroyed` or `this._enabled`
to guard against race conditions or improper lifecycle calls.
After calling `destroy()`, the instance should be nulled out and never used.

On a custom `destroy()` method follow the correct order:

- Remove active timeouts and GLib sources.
- Disconnect all signal handlers.
- Release child references and resources.
- Call `super.destroy()` as the final step.

Bad Practice:

```js
destroy() {
    if (this._destroyed)
        return;
    this._destroyed = true;

    if (this._sourceId) {
        GLib.Source.remove(this._sourceId);
        this._sourceId = null;
    }
    super.destroy();
}
```

Correct practice:

```js
destroy() {
    if (this._sourceId) {
        GLib.Source.remove(this._sourceId);
        this._sourceId = null;
    }
    super.destroy();
}
```

## Widget Destruction vs. Signal Connections

Override `destroy()` directly on GObject widgets rather than connecting
`destroy` signal listener.

Bad practice:

```js
class MyWidget extends St.Widget {
    constructor(params = {}) {
        super(params);
        this._signal = this.connect('destroy', this._onDestroy.bind(this));
    }

    _onDestroy() {
        // Redundant disconnecting destroy signal
        this.disconnect(this._signal);
        // some cleanup here ..
    }
}
```

Correct practice:

```js
class MyWidget extends St.Widget {
    constructor(params = {}) {
        super(params);
    }

    destroy() {
        // some cleanup here ..
        super.destroy();
    }
}
```

## UI Elements: Icons vs. Emojis

- For UI Icons: Use `Gtk.Image` for preferences (prefs.js) and `St.Icon`
    (or `icon_name` properties) for shell UI (extension.js).
    Do not use Unicode emojis as icons.
- For Progress: Use shell components such as `ui.BarLevel` or custom `St.Bin`
    widgets instead of ASCII progress strings (for example, `█░░`).

## Formatting and Line Length

Maintain a maximum line length of 200 characters to ensure readability during
review. This avoids unnecessary horizontal scrolling in the EGO review UI.

## Comments

Write self-explanatory code with clear variable and function names
to make redundant comments unnecessary.

Comments that explain basic JavaScript syntax, describe trivial operations,
or translate code line-by-line into natural language are not allowed.

## Subprocesses and D-Bus Communication

Avoid spawning external shell commands where possible.

Use D-Bus for communication with system services or
external background processes if possible.

Heavy tasks should be offloaded to a separate app as a dependency and
communicating via D-Bus to keep the main GNOME Shell process lightweight.

## Use Helper Functions Instead of Code Duplication

Avoid copying and pasting identical code blocks.
Repetitive logic should be extracted into modular helper functions.

Shared utility modules used by both `extension.js` and `prefs.js` must never
import `St`, `Clutter`, `Gtk`, `Gdk`, `Adw` libraries due to process isolation.

## Process Isolation

Keep UI modules strictly separate based on their execution environment.

If a file or module belongs only to a specific process, structure your directory
layout to make that obvious to reviewers.
For example, modules that are only loaded by `prefs.js` should reside inside
a `prefs/` directory.

## Keep the Default Entry Point as Small as Possible

Avoid putting thousands of lines of code into the entry point class.
Large entry points make reviewing the cleanup extremely difficult.

Split your logic into smaller modules. Keep the entry point class
as small as possible.

## Keep enable and disable functions close

Keep your `enable()` and `disable()` methods next to each other in the class
definition. This allows reviewers to easily verify the cleanup.

Additionally, avoid unnecessary method aliasing without
a strong structural reason.

## Modules are Better Than a Single File

Putting all extension logic into a single large file makes code difficult
to maintain and review. Extremely large files can even cause the EGO review page
to freeze or lag while loading diffs.

Split your logic into modular, single responsibility files.
This can significantly speed up the review process.

## Do Not Submit Incomplete or Placeholder Extensions

AI models often generate the template code filled with empty lifecycle
`enable()` and `disable()` methods.

Always check whether the generated code is complete, fully functional logic
rather than placeholder stubs.

Bad practice:

```js
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {Example} from './example.js';

export default class ExampleExtension extends Extension {
    enable() {
        // nothing here
    }

    disable() {
        // nothing here
    }
}
```

## Avoid Spaghetti Cleanup

Every class must be responsible for managing its own resources and lifecycle.

When a class connects a signal, adds a timeout, creates a Soup session or
a `Gio.Cancellable`, that same class should handle its cleanup.
Initializing in one class and cleanup in another, makes memory leaks
and cleanup process extremely difficult to review.

## Keep Timeout Removal Next to Creation

If a function can be called multiple times and creates a timer, any existing
source must be removed before creating a new one, and the cleanup must be placed
directly next to the creation line.

Separating the removal check from the creation logic by many lines makes
it difficult for reviewers to verify that old timeouts are properly removed
before a new one is created.

Bad practice:

```js
if (this._sourceId) {
    GLib.Source.remove(this._sourceId);
    this._sourceId = null;
}
// 200 lines after
this._sourceId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
    // ...
    return GLib.SOURCE_CONTINUE;
});
```

Correct practice:

```js
// the source removed before the timeout creation
if (this._sourceId) {
    GLib.Source.remove(this._sourceId);
    this._sourceId = null;
}
this._sourceId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
    // ...
    return GLib.SOURCE_CONTINUE;
});
```

## Keep the Settings Schema ID in Metadata

If the extension uses its own GSettings, define the `settings-schema` in
`metadata.json` and use `this.getSettings()` in the entry point
without any parameters. This avoids repeating the schema ID in every file or
holding it in the global scope as a constant.

Bad practice:

```js
const SCHEMA_ID = 'org.gnome.shell.extensions.my-id';

export default class ExampleExtension extends Extension {
    enable() {
        this._settings = this.getSettings(SCHEMA_ID);
    }

    disable() {
        this._settings = null;
    }
}
```

Correct practice:

First, define it in your `metadata.json`:

```json
{
  // ...
  "settings-schema": "org.gnome.shell.extensions.my-id"
}
```

Then use it cleanly in your code:

```js
export default class ExampleExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
    }

    disable() {
        this._settings = null;
    }
}
```

[ego]: https://extensions.gnome.org
[review-guidelines]: /extensions/review-guidelines/review-guidelines.html
[port-guide]: /extensions/#upgrading
[best-practices-markdown]: https://gitlab.gnome.org/World/javascript/gjs-guide/-/blob/main/docs/extensions/review-guidelines/best-practices.md
