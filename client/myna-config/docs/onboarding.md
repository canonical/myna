# Onboarding

The settings application ships separately from the `myna` snap, so it can be
opened on a machine where dictation is not installed at all. When that is the
case it opens a three-step wizard instead of the settings window.

## What opens it

`myna_config::onboarding` assesses three components from two observations the
application already makes at startup (`snap list` and `snap connections`) plus a
directory probe:

| Component       | Required | Satisfied when                             | Remedy       |
| --------------- | -------- | ------------------------------------------ | ------------ |
| Myna            | yes      | the `myna` snap is installed               | instructions |
| Model           | yes      | discovery reports at least one backend     | install      |
| Shell extension | no       | `myna-shell@canonical.com` is in a data dir | instructions |

The wizard opens when a **required** component is missing. "Model" is satisfied
by discovery rather than by a snap name: which snaps are backends is a property
of the socket interface they publish, not of their name.

## What the application installs, and what it does not

Only the recommended model is installed in-app, through snapd's REST API on
`/run/snapd.socket` (`POST /v2/snaps/myna-parakeet` with the
`model-parakeet-int8` component, then the change is polled for progress).
snapd asks polkit for authorization; the wizard reports a denial as a failure
rather than retrying. The component is `type: standard` rather than a default
component, so it has to be named explicitly - an install that omits it leaves a
backend with no weights.

Two components are explained rather than installed:

- **Myna** - snapd refuses to install a snap declaring a user daemon unless
  `experimental.user-daemons` is set or the snap-id is on the hardcoded
  allowlist in snapd's `overlord/snapstate/snapstate.go`. A local install has no
  snap-id at all, so an Install button would fail on every stock machine. The
  instructions carry both commands.
- **Shell extension** - not published anywhere snapd can reach; it is copied
  into `~/.local/share/gnome-shell/extensions` by hand. It is also not required:
  the daemon falls back to desktop notifications without it, and gating the flow
  on a manual copy would strand anyone who cannot perform it.

After a successful install the wizard connects `myna:backend` to the new
backend's slot and restarts the daemon, reusing the active-backend switch, so
finishing the flow leaves dictation working rather than merely installed.

## The keyboard shortcut

Under portal activation the accelerator belongs to the compositor: the portal
raises its own shortcut sheet at the daemon's first bind, and the key is changed
in the desktop's keyboard panel. Only the daemon holding the portal session can
see what was granted, so the last step reads a `Shortcut` property from
`com.canonical.Myna.Dictation` and renders key caps when the daemon publishes
one. No daemon publishes it yet, so today the step states where the shortcut
lives and opens the keyboard panel rather than naming a key it did not verify.

## Cost

The startup assessment is the same two subprocesses as a `RefreshReason::Startup`
refresh, run before any window exists, and it is handed to the wizard rather
than repeated there. A refresh after an install costs another `snap list` plus
the discovery the switch already performs.
