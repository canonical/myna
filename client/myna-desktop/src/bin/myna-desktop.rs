//! `myna-desktop` — the push-to-talk dictation app (plan T21/T22).
//!
//! Composes activation, the IBus text injector, and an activity indicator over a
//! live `myna-audio` PipeWire capture source and the `myna-orchestrator` session
//! into a [`DesktopController`].
//!
//! ## Activation
//!
//! Dictation must inject into *another* app, so activation must not depend on
//! terminal focus. Two transports, and **the daemon picks between them itself**
//! ([`Activation::resolve`]) — which one is correct is a property of how the
//! binary was packaged, not a preference a user can hold:
//!
//! - **GlobalShortcuts portal** — the sandboxed-native trigger, and the default
//!   whenever `$SNAP` is set. GNOME only grants a portal app identity to a
//!   packaged app, so this is available exactly when packaged.
//! - **Control socket** — the default unpackaged. `myna-desktop` listens on a
//!   control socket and a GNOME custom shortcut bound to `myna-desktop --toggle`
//!   pokes it. Run `myna-desktop --install-shortcut '<Super>t'` once to bind it.
//!
//! Both are press-to-toggle: tap to start, tap again to stop. `--portal` /
//! `--control` force one; `--hold` switches the portal to hold-to-talk;
//! `--stdin` is terminal debug (injects back into the terminal). The
//! experimental GTK activity overlay was removed (T150); the indicator is
//! either the myna-shell overlay (feature 004) or headless notifications.
//!
//! ```text
//!   myna-server --adapter whisper --socket /tmp/myna.sock &
//!   myna-desktop --install-shortcut '<Super>t'      # once, unpackaged: bind a key
//!   myna-desktop --socket /tmp/myna.sock --language en   # the daemon
//!   # focus a text field, tap the shortcut, speak, tap again → text is injected
//! ```
//!
//! ## Things that are resolved, not asked
//!
//! Three switches used to be the user's problem and are now the daemon's:
//! the indicator bus (`com.canonical.Myna.Dictation` is always served, falling back to
//! notifications by itself), the activation transport (above), and streaming
//! preedit ([`resolve_preedit`]). Each still has an explicit override for
//! debugging, but a correct setup requires none of them.
//!
//! What *is* configurable is resolved in one place, [`Resolved`], with one
//! order: a command-line flag, then the user's GSettings value
//! (`com.canonical.Myna.Dictation`), then the system default a `snap set myna …` wrote,
//! then the built-in. The user's own store outranks the system one on purpose -
//! snapd configuration is per snap, not per user, so an admin setting a
//! language must not overrule an account that chose its own.

use std::path::PathBuf;
use std::process::ExitCode;

use myna_audio::{CaptureSource, PipeWireBackend};
use myna_core::{AudioFormat, SessionConfig};
use myna_desktop::backend::BackendSocket;
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::dbus::serve::{ServeError, ZbusBus};
use myna_desktop::dbus::{DictationService, SharedBus};
use myna_desktop::indicator::dbus::{DbusIndicator, Readiness, ReadinessTee};
use myna_desktop::indicator::dynamic::DynamicIndicator;
use myna_desktop::indicator::notify::NotifyIndicator;
use myna_desktop::inject::lazy::{IbusConnect, LazyInjector};
use myna_desktop::shortcut::control::{default_socket_path, send_toggle, ControlTrigger};
use myna_desktop::shortcut::portal::{ActivationMode, GlobalShortcutTrigger, TriggerError};
use myna_desktop::shortcut::retry::{BindFailure, Rebind, RetryingTrigger};
use myna_desktop::shortcut::Trigger;
use myna_desktop::{DesktopController, Indicator, Live};
use myna_orchestrator::{
    run_dictation, BackendError, OrchestratorEvent, StdinTrigger, StopHandle, WsUnixBackend,
};
use tokio::sync::mpsc;

const USAGE: &str = "\
myna-desktop — push-to-talk dictation (T21/T22)

USAGE:
    myna-desktop --socket <path> [options]      # run the dictation daemon
    myna-desktop --toggle                       # start/stop the running daemon
    myna-desktop --install-shortcut <accel>     # bind a GNOME shortcut (e.g. '<Super>t')

Focus a text field, tap the shortcut to start, speak, tap again to stop. The
committed transcript is injected via IBus into that field.

The daemon always serves com.canonical.Myna.Dictation for the GNOME Shell extension,
picks its activation transport from how it was packaged, and decides streaming
preedit from your persisted mode preference. A correct setup needs none of the
overrides below.

OPTIONS:
    --socket <path>    Unix socket of a running myna-server
    --backend-dir <d>  directory to find the backend socket under, re-checked at
                       every press (<d>/*/ubustt.sock - how the snap wires the
                       `backend` content share). One of these two is required.
    --language <lang>  language hint sent in the session config (e.g. en)
    --target <node>    PipeWire node.name to capture from (default: system default)
    --control-socket <path>
                       control-socket path (default: $XDG_RUNTIME_DIR/myna-desktop.sock)
    --status           print what this daemon has resolved, what the running one
                       is doing, and whether the backend is reachable, then exit
    --toggle           poke the running daemon over the control socket. Control
                       activation only - a portal daemon has no control socket
                       and is driven by its portal shortcut instead.
    --install-shortcut bind a GNOME custom keybinding to --toggle (e.g.
                       '<Super>t'), then exit. Control activation only: on a
                       portal daemon it would shadow the portal's own binding,
                       so it refuses. Rebind there in Settings → Keyboard.
    --shortcut <accel> preferred trigger in portal activation (the portal's bind
                       dialog may still let you pick a different key)
    --hold             portal activation: hold-to-talk instead (hold = record)

ACTIVATION (default: portal when packaged — $SNAP set — else control socket):
    --portal           force the GlobalShortcuts portal (packaged builds only)
    --control          force the control socket (poke it with --toggle)
    --stdin            DEBUG: drive from the terminal (injects into the terminal)

OVERRIDES (for debugging; the daemon resolves all three by itself):
    --preedit          force the in-flight hypothesis into the field's preedit
    --no-preedit       force commit-only, even in streaming mode
    --no-dbus          do not serve com.canonical.Myna.Dictation (notifications only).
                       Also opts out of the one-daemon-at-a-time guard, which
                       is that name: two daemons split the hotkey from the UI

    -h, --help         show this help
";

/// How a press reaches the daemon. Which one is correct follows from how the
/// binary was packaged, so [`Args::activation`] holds `None` ("resolve it")
/// unless the user forced one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Activation {
    /// GlobalShortcuts portal — needs the app identity only a packaged build has.
    Portal,
    /// Control socket + a desktop custom shortcut bound to `--toggle`.
    Control,
    /// DEBUG: Enter on stdin; injects back into the launching terminal.
    Stdin,
}

impl Activation {
    /// The portal only serves apps the compositor can identify, which on GNOME
    /// means a packaged one — so `$SNAP` *is* the availability test, not a
    /// heuristic. Unpackaged builds get the control socket, which needs no app
    /// identity.
    fn from_packaging() -> Activation {
        if std::env::var_os("SNAP").is_some() {
            Activation::Portal
        } else {
            Activation::Control
        }
    }

    /// Parse a settings/`snap set` value. `auto` is absent by construction
    /// (`Settings` filters it), so anything unrecognised here is a typo, and
    /// answering `None` means it falls through to packaging rather than
    /// silently selecting a transport nobody asked for.
    fn from_nick(nick: &str) -> Option<Activation> {
        match nick {
            "portal" => Some(Activation::Portal),
            "control" => Some(Activation::Control),
            _ => None,
        }
    }
}

/// System-wide defaults from `snap set myna …`, handed over by the launcher.
///
/// snapd's configuration is per *snap*, not per user, so it can only ever be a
/// default: an admin (or an image build) setting `language=fr` must not
/// overrule a user who set their own. See [`Resolved`] for the order.
#[derive(Clone, Debug, Default, PartialEq)]
struct SystemDefaults {
    activation: Option<String>,
    language: Option<String>,
    hotkey: Option<String>,
}

impl SystemDefaults {
    fn from_env() -> Self {
        let read = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Self {
            activation: read("MYNA_CFG_ACTIVATION"),
            language: read("MYNA_CFG_LANGUAGE"),
            hotkey: read("MYNA_CFG_HOTKEY"),
        }
    }
}

/// Everything the daemon works out for itself, resolved once at startup.
///
/// One order throughout, most specific first:
///
/// 1. **a command-line flag** — someone is debugging, and meant it;
/// 2. **the user's GSettings value** (`com.canonical.Myna.Dictation`) — the desktop's own
///    per-user store, which is what a Settings page writes;
/// 3. **the system default** from `snap set myna …` — per machine, root-set;
/// 4. **the built-in** — packaging for activation, the tier gate for preedit.
#[derive(Debug, PartialEq)]
struct Resolved {
    activation: Activation,
    language: Option<String>,
    hotkey: Option<String>,
    preedit: bool,
}

impl Resolved {
    fn new(args: &Args, settings: &myna_core::Settings, system: &SystemDefaults) -> Self {
        let activation = args
            .activation
            .or_else(|| {
                settings
                    .activation
                    .as_deref()
                    .and_then(Activation::from_nick)
            })
            .or_else(|| system.activation.as_deref().and_then(Activation::from_nick))
            .unwrap_or_else(Activation::from_packaging);
        Self {
            activation,
            language: pick(&args.language, &settings.language, &system.language),
            hotkey: pick(&args.shortcut, &settings.hotkey, &system.hotkey),
            preedit: resolve_preedit(args.preedit, settings.streaming_mode),
        }
    }
}

/// The precedence rule itself, in one place so every knob obeys the same one.
fn pick(flag: &Option<String>, user: &Option<String>, system: &Option<String>) -> Option<String> {
    flag.clone()
        .or_else(|| user.clone())
        .or_else(|| system.clone())
}

/// The part of [`Resolved`] the daemon keeps re-reading while it runs.
///
/// Resolving once at startup made a restart the price of every settings
/// change, for every writer there is (`gsettings`, `myna-dictate`, a Settings
/// page, another snap - T54). The precedence does not move: a change re-runs
/// [`Resolved::new`] with the same flags and system defaults, so a `--language`
/// on the command line still outranks a GSettings write made an hour later, and
/// only the answer lands in these cells.
///
/// The two keys here are the two whose readers ask for them again anyway -
/// preedit at each transcript event, language at each press. `activation` and
/// `hotkey` are bound into the trigger at startup and are *not* live; a change
/// to either says so in the journal instead of pretending to apply.
#[derive(Clone)]
struct LiveSettings {
    preedit: Live<bool>,
    language: Live<Option<String>>,
}

impl LiveSettings {
    fn new(resolved: &Resolved) -> Self {
        Self {
            preedit: Live::new(resolved.preedit),
            language: Live::new(resolved.language.clone()),
        }
    }

    /// Subscribe to the settings store, writing every change through these
    /// cells. The returned watch must outlive the controller - dropping it
    /// ends the subscription.
    fn follow(&self, args: &Args, startup: &Resolved) -> Option<myna_core::SettingsWatch> {
        // Read once, not per change: snapd configuration reaches this process
        // as environment, and the launcher sets it before exec.
        let system = SystemDefaults::from_env();
        let (activation, hotkey) = (startup.activation, startup.hotkey.clone());
        let watch = myna_core::settings::watch({
            let (args, live, system) = (args.clone(), self.clone(), system.clone());
            move |settings| {
                let now = Resolved::new(&args, &settings, &system);
                live.apply(&now, &preedit_reason(args.preedit, settings.streaming_mode));
                if now.activation != activation || now.hotkey != hotkey {
                    myna_core::info_log!(
                        "settings",
                        "activation/hotkey changed ({:?}, {}) - both are bound at startup, so restart to apply",
                        now.activation,
                        now.hotkey.as_deref().unwrap_or("(portal default)")
                    );
                }
            }
        });
        if watch.is_some() {
            // The value this daemon started from was read before the
            // subscription existed. Re-read once now that it does, so a change
            // made in that window is applied instead of waiting for the next
            // one - a daemon started at login races anything the session
            // autostarts alongside it.
            let settings = myna_core::Settings::load();
            let now = Resolved::new(args, &settings, &system);
            self.apply(&now, &preedit_reason(args.preedit, settings.streaming_mode));
        } else {
            // Not a failure: the same missing schema that makes `Settings::load`
            // read defaults. Said out loud because "my change did nothing" is
            // otherwise indistinguishable from a bug in the watch.
            myna_core::info_log!(
                "settings",
                "no {} schema installed - settings are startup-only",
                myna_core::settings::SCHEMA_ID
            );
        }
        watch
    }

    /// Write the new resolution through, logging only what actually moved -
    /// GSettings notifies per key, so most changes touch neither of these.
    ///
    /// A change that resolves to the value already in force is the interesting
    /// *silent* case (a flag outranks it, or the tier gate refuses it), so it
    /// is logged too - at the debug tier, where it explains "I changed it and
    /// nothing happened" without filling a long-lived daemon's journal.
    fn apply(&self, resolved: &Resolved, reason: &str) {
        if self.preedit.get() != resolved.preedit {
            myna_core::info_log!("settings", "preedit -> {} ({reason})", resolved.preedit);
            self.preedit.set(resolved.preedit);
        } else {
            myna_core::dbg_log!("settings", "preedit stays {} ({reason})", resolved.preedit);
        }
        if self.language.get() != resolved.language {
            myna_core::info_log!(
                "settings",
                "language -> {} (live, from the next press)",
                resolved.language.as_deref().unwrap_or("(backend default)")
            );
            self.language.set(resolved.language.clone());
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Args {
    socket: Option<PathBuf>,
    backend_dir: Option<PathBuf>,
    language: Option<String>,
    target: Option<String>,
    control: Option<PathBuf>,
    shortcut: Option<String>,
    toggle: bool,
    status: bool,
    install_shortcut: Option<String>,
    /// `None` = resolve from packaging; `Some` = the user forced one.
    activation: Option<Activation>,
    hold: bool,
    /// `None` = resolve from the persisted streaming mode; `Some` = forced.
    preedit: Option<bool>,
    no_dbus: bool,
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1).peekable())
}

fn parse_args_from(
    mut it: std::iter::Peekable<impl Iterator<Item = String>>,
) -> Result<Args, String> {
    let mut a = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--socket" => a.socket = Some(PathBuf::from(next(&mut it, "--socket")?)),
            "--backend-dir" => a.backend_dir = Some(PathBuf::from(next(&mut it, "--backend-dir")?)),
            "--language" => a.language = Some(next(&mut it, "--language")?),
            "--target" => a.target = Some(next(&mut it, "--target")?),
            "--control-socket" => {
                a.control = Some(PathBuf::from(next(&mut it, "--control-socket")?))
            }
            "--shortcut" => a.shortcut = Some(next(&mut it, "--shortcut")?),
            "--toggle" => a.toggle = true,
            "--status" => a.status = true,
            "--install-shortcut" => {
                a.install_shortcut = Some(next(&mut it, "--install-shortcut")?);
            }
            "--portal" => set_activation(&mut a, Activation::Portal)?,
            "--control" => set_activation(&mut a, Activation::Control)?,
            "--stdin" => set_activation(&mut a, Activation::Stdin)?,
            "--hold" => a.hold = true,
            "--preedit" => a.preedit = Some(true),
            "--no-preedit" => a.preedit = Some(false),
            "--no-dbus" => a.no_dbus = true,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    if a.socket.is_some() && a.backend_dir.is_some() {
        return Err("--socket and --backend-dir are alternatives (pick one)".into());
    }
    Ok(a)
}

/// The activation flags are mutually exclusive: silently letting the last one
/// win would make `--portal --stdin` look like it worked.
fn set_activation(a: &mut Args, mode: Activation) -> Result<(), String> {
    match a.activation {
        Some(existing) if existing != mode => Err(format!(
            "conflicting activation flags: {existing:?} and {mode:?} (pick one)"
        )),
        _ => {
            a.activation = Some(mode);
            Ok(())
        }
    }
}

/// Streaming preedit (R9) is a consequence of the transcription mode, not a
/// separate preference: hypotheses only exist in streaming mode, so "show
/// them in the field" is decided by the same tier gate that decides streaming
/// ([`myna_core::effective_mode`]) — no server round-trip needed, and a
/// batch-only backend simply never emits `Unstable` anyway.
///
/// The injector still has the final say downstream: the controller renders a
/// preedit only where the backend has a real preedit region
/// (`Injector::supports_preedit`).
fn resolve_preedit(forced: Option<bool>, preference: myna_core::StreamingMode) -> bool {
    forced.unwrap_or_else(|| {
        myna_core::effective_mode(preference) == myna_core::StreamingMode::Streaming
    })
}

/// Why preedit came out the way it did, for the journal.
///
/// It is the one setting nobody typed, so "why are partials not showing" has
/// to be answerable from the log alone: either a flag forced it, or the
/// persisted preference met the tier gate. Kept separate from
/// [`resolve_preedit`] so that resolving - which now happens again on every
/// settings change - stays silent, and only the startup line and an actual
/// change say anything.
fn preedit_reason(forced: Option<bool>, preference: myna_core::StreamingMode) -> String {
    match forced {
        Some(forced) => format!("forced {forced} by flag"),
        None => format!(
            "streaming-mode {preference:?} resolves to {:?} on tier {}",
            myna_core::effective_mode(preference),
            myna_core::hardware_tier()
        ),
    }
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn control_path(args: &Args) -> PathBuf {
    args.control.clone().unwrap_or_else(default_socket_path)
}

/// What to say when `--toggle` cannot reach the daemon.
///
/// "is `myna-desktop --socket <path>` running?" is only true advice under
/// control activation. A portal daemon takes its trigger from the portal and
/// never opens a control socket, so a *perfectly healthy* packaged daemon
/// fails `--toggle` every single time - and the old hint sent people hunting
/// for a dead process instead of pointing at their keyboard.
///
/// Resolved locally, like `--status`: this process is not the daemon, so under
/// an unpackaged `--toggle` against a packaged daemon the two can disagree.
/// That is the same caveat `--status` already carries, and the answer is still
/// better than a fixed string that is wrong for the shipped default.
fn toggle_failure_hint(args: &Args) -> Vec<String> {
    let settings = myna_core::Settings::load();
    let resolved = Resolved::new(args, &settings, &SystemDefaults::from_env());
    toggle_hint_for(resolved.activation, resolved.hotkey.as_deref())
}

fn toggle_hint_for(activation: Activation, hotkey: Option<&str>) -> Vec<String> {
    match activation {
        Activation::Portal => vec![
            "activation is Portal: the daemon takes its trigger from the GlobalShortcuts \
             portal and opens no control socket, so --toggle cannot reach it."
                .into(),
            match hotkey {
                Some(key) => format!("press {key} instead."),
                None => "press your dictation shortcut instead (Settings → Keyboard lists \
                         it under myna)."
                    .into(),
            },
            "to poke the daemon from a script or a custom keybinding, run it with --control."
                .into(),
        ],
        Activation::Stdin => vec![
            "activation is Stdin (debug): the daemon reads Enter from its own terminal and \
             opens no control socket."
                .into(),
        ],
        Activation::Control => vec!["(is `myna-desktop --socket <path>` running?)".into()],
    }
}

impl Args {
    /// Where to look for the backend, or `None` when neither form was given.
    /// Deliberately *not* resolved here: the socket a content share supplies
    /// can appear, move and vanish under a `snap refresh` of the backend, so
    /// the daemon must not bake a path in at startup (see [`BackendSocket`]).
    fn backend(&self) -> Option<BackendSocket> {
        match (&self.socket, &self.backend_dir) {
            (Some(path), _) => Some(BackendSocket::Fixed(path.clone())),
            (_, Some(dir)) => Some(BackendSocket::Search(dir.clone())),
            _ => None,
        }
    }
}

/// Build the per-Press session factory: a fresh backend connection + live
/// capture source, run through the orchestrator (capture-at-press, ready-gated).
///
/// NOTE (T21 follow-up): negotiate `input_format` from server capabilities
/// (capabilities.query) rather than assuming the default; the shipped adapters
/// accept the default 16 kHz s16le mono, so the MVP uses it directly.
fn make_session(
    args: &Args,
    live: &LiveSettings,
    readiness: Option<Readiness>,
    pump_bus: Option<SharedBus>,
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send + 'static {
    let backend_socket = args.backend().expect("daemon requires a backend");
    let language = live.language.clone();
    let target = args.target.clone();
    move |events: mpsc::Sender<OrchestratorEvent>| {
        // Re-resolved per Press, so a backend connected (or refreshed, or
        // swapped) after the daemon started is picked up without a restart.
        let socket = match backend_socket.resolve() {
            Ok(socket) => socket,
            Err(e) => return no_backend(e),
        };
        let backend = WsUnixBackend::new(&socket);
        let mut builder = CaptureSource::builder(AudioFormat::default());
        if let Some(node) = &target {
            builder = builder.target(node.clone());
        }
        let source = builder.backend(Box::new(PipeWireBackend::new())).build();
        let stop = source.stop_handle();
        let config = SessionConfig {
            // Read here rather than captured above, for the same reason the
            // backend socket is: a value changed after login applies at the
            // next press, without a restart.
            language: language.get(),
            ..Default::default()
        };
        // --dbus: pump the capture level meter onto com.canonical.Myna.Dictation for the
        // session's lifetime (the pump ends when the source drops its stats
        // sender at session end). Grab the receiver before the source moves.
        let pump = pump_bus.clone().map(|bus| (bus, source.stats()));
        let readiness = readiness.clone();
        // Reset readiness synchronously, right here — when the session is
        // created (i.e. when the controller calls `session.start()`), not
        // lazily inside the `async move` block below. The controller
        // publishes `IndicatorState::Recording` immediately after
        // `session.start()` returns, before this future is ever polled; if
        // `ready_seen` were still `true` from the *previous* utterance at
        // that moment, `map_state` would publish `"recording"` first, only
        // flipping to `"loading"` once this future is finally polled and the
        // old `r.reset()` ran — a spurious recording→loading→recording
        // triple-flip on the wire that the GNOME Shell HUD renders as a
        // flicker on (re)start (no debounce there, by design elsewhere).
        // Resetting here instead means `ready_seen` is already `false` before
        // the controller ever reads it, so only one clean loading→recording
        // transition is published.
        if let Some(r) = &readiness {
            r.reset();
        }
        let run: SessionRun = Box::pin(async move {
            if let Some((bus, stats)) = pump {
                tokio::spawn(myna_desktop::dbus::pump::run(bus, stats));
            }
            // Tee the event stream so the publisher can split loading/recording
            // (R4) when in --dbus mode.
            match readiness {
                Some(r) => {
                    let mut sink = ReadinessTee::new(ChannelSink(events), r);
                    run_dictation(&backend, config, source, &mut sink).await
                }
                None => {
                    let mut sink = ChannelSink(events);
                    run_dictation(&backend, config, source, &mut sink).await
                }
            }
        });
        (run, stop)
    }
}

/// The session a Press gets when no backend is connected: it fails
/// immediately, which the controller reports on the indicator exactly like any
/// other backend error. Not exiting matters - "no backend yet" is the normal
/// state between `snap install myna` and the first `snap connect`, and it is a
/// state the user fixes without touching the daemon.
fn no_backend(e: myna_desktop::backend::ResolveError) -> (SessionRun, StopHandle) {
    let run: SessionRun = Box::pin(async move { Err(BackendError::Connect(e.to_string())) });
    (run, StopHandle::default())
}

/// Binds the portal shortcut, re-binding whenever the portal goes away.
struct PortalRebind {
    shortcut: Option<String>,
    mode: ActivationMode,
    /// The last attempt failed because there was no portal to talk to, so the
    /// next wait can be spent asleep on the bus telling us one arrived.
    awaiting_portal: bool,
}

#[async_trait::async_trait]
impl Rebind for PortalRebind {
    /// Nothing to poll for: park until a portal appears, with `delay` as the
    /// net. On a machine whose desktop never comes this is where the daemon
    /// spends its life, and it costs a signal match on a connection it already
    /// holds - measurably nothing.
    async fn wait_before_retry(&mut self, delay: std::time::Duration) {
        if self.awaiting_portal {
            myna_desktop::shortcut::portal::await_portal(delay).await;
        } else {
            tokio::time::sleep(delay).await;
        }
    }

    async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
        self.awaiting_portal = false;
        GlobalShortcutTrigger::bind("dictate", self.shortcut.as_deref(), self.mode)
            .await
            .map(|t| Box::new(t) as Box<dyn Trigger>)
            .map_err(|e| match e {
                // No portal to reach, and we decline to conjure one. Nothing
                // was asked of anyone, so check again in a second: the answer
                // flips when the desktop comes up and the hotkey should be
                // live then, not half a minute later.
                TriggerError::PortalNotRunning(_) => {
                    self.awaiting_portal = true;
                    BindFailure::NotYet(e.to_string())
                }
                // The request reached a portal and it could not serve us -
                // retry quickly at first, then back away.
                TriggerError::PortalUnavailable(_) => BindFailure::Unavailable(e.to_string()),
                // BindShortcuts itself came back without a grant, which on
                // GNOME means the user dismissed (or ignored) the confirm
                // sheet. Asking again straight away just re-raises it.
                TriggerError::BindRejected(_) => BindFailure::Refused(e.to_string()),
            })
    }
}

/// Binds the control socket. Retried too: `$XDG_RUNTIME_DIR` is created by
/// pam_systemd, so a daemon that starts early can find it not there yet.
struct ControlRebind {
    path: PathBuf,
}

#[async_trait::async_trait]
impl Rebind for ControlRebind {
    async fn bind(&mut self) -> Result<Box<dyn Trigger>, BindFailure> {
        // Always `Unavailable`: a socket bind has no user-facing step to
        // refuse, so every failure is "not there yet" and worth retrying fast.
        ControlTrigger::bind(&self.path)
            .map(|t| Box::new(t) as Box<dyn Trigger>)
            .map_err(|e| {
                BindFailure::Unavailable(format!(
                    "cannot bind control socket {}: {e}",
                    self.path.display()
                ))
            })
    }
}

/// Build and run the controller with the given indicator (tokio side).
///
/// Nothing here is allowed to end the process. Every boundary this composes -
/// IBus, the portal, the control socket, the backend - is a thing that comes
/// and goes independently of the daemon: IBus restarts on an input-source
/// change, `xdg-desktop-portal` restarts, the backend socket is re-created by
/// `snap refresh`, and at PAM login none of them exist yet. Treating any of
/// them as a startup precondition turned "start before the compositor" into
/// five restarts in five seconds and then a permanently failed unit, which is
/// the *normal* boot for a user daemon, not a corner case.
///
/// So: connect the injector lazily ([`LazyInjector`], at the Press that needs
/// it), retry activation forever ([`RetryingTrigger`]), resolve the backend
/// per Press ([`no_backend`]) - and let each of them report itself on the
/// indicator instead.
async fn run_controller(
    args: Args,
    resolved: Resolved,
    indicator: impl Indicator + 'static,
    readiness: Option<Readiness>,
    pump_bus: Option<SharedBus>,
) -> ExitCode {
    let live = LiveSettings::new(&resolved);
    // Held for the controller's whole life, and no longer: the subscription
    // exists to serve this controller, and dropping the handle stops it.
    let _settings_watch = live.follow(&args, &resolved);

    let builder = DesktopController::builder()
        .injector(LazyInjector::new(IbusConnect))
        .indicator(indicator)
        .session(make_session(&args, &live, readiness, pump_bus.clone()))
        .preedit(live.preedit.clone());

    let mut controller = match resolved.activation {
        // Debug only, and the one trigger whose end is a real user intent:
        // Ctrl-D means "stop", so it is not retried.
        Activation::Stdin => builder.trigger(StdinTrigger::new()).build(),
        Activation::Portal => {
            let mode = if args.hold {
                ActivationMode::Hold
            } else {
                ActivationMode::Toggle
            };
            let trigger = RetryingTrigger::new(PortalRebind {
                shortcut: resolved.hotkey.clone(),
                mode,
                awaiting_portal: false,
            });
            builder.trigger(with_status(trigger, pump_bus)).build()
        }
        Activation::Control => {
            let trigger = RetryingTrigger::new(ControlRebind {
                path: control_path(&args),
            });
            builder.trigger(with_status(trigger, pump_bus)).build()
        }
    };

    banner(&args, &resolved);
    controller.run().await;
    println!("bye");
    ExitCode::SUCCESS
}

/// Publish the "hotkey not bound yet" reason on `com.canonical.Myna.Dictation` where
/// there is a bus to publish it on.
fn with_status(trigger: RetryingTrigger, bus: Option<SharedBus>) -> RetryingTrigger {
    match bus {
        Some(bus) => trigger.status_on(bus),
        None => trigger,
    }
}

fn banner(args: &Args, resolved: &Resolved) {
    let sock = args.backend().map(|b| b.describe()).unwrap_or_default();
    match resolved.activation {
        Activation::Stdin => println!(
            "myna-desktop → {sock} — DEBUG stdin: Enter to start/stop (injects into THIS terminal)"
        ),
        Activation::Portal => {
            let verb = if args.hold { "hold" } else { "tap" };
            let key = resolved.hotkey.as_deref().unwrap_or("your chosen shortcut");
            println!("myna-desktop → {sock} — {verb} {key} to talk (portal)");
        }
        Activation::Control => {
            println!(
                "myna-desktop → {sock} — daemon ready; tap your dictation shortcut to start/stop."
            );
            println!(
                "  if you haven't bound one yet: `myna-desktop --install-shortcut '<Super>t>'"
            );
            println!(
                "  or bind a GNOME custom shortcut to: `{}`",
                toggle_command()
            );
        }
    }
}

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "myna-desktop".to_string())
}

/// The command a desktop shortcut should invoke to poke the daemon. Under
/// snap confinement `current_exe` is a *revisioned* path that goes stale on
/// refresh — use the stable `/snap/bin/<instance>.toggle` entry instead.
fn toggle_command() -> String {
    if let Ok(instance) = std::env::var("SNAP_INSTANCE_NAME") {
        if !instance.is_empty() {
            return format!("/snap/bin/{instance}.toggle");
        }
    }
    format!("{} --toggle", exe_path())
}

/// Why `--install-shortcut` must not run, where that is the case.
///
/// A custom keybinding to `--toggle` is the *control*-activation setup. Run it
/// against a portal daemon and it does two bad things at once: the binding is
/// inert (a portal daemon opens no control socket), and `gsd-media-keys`
/// serves both custom keybindings and the portal's global shortcuts, so a
/// custom binding on the same accel **shadows the portal's own** - i.e. it
/// breaks the hotkey that was working. Since portal is the packaged default,
/// following the README's `myna.install-shortcut '<Super>t'` on a snap install
/// was a reliable way to disable your own dictation key, with no feedback
/// beyond a control-socket error naming a socket that was never going to exist.
fn shortcut_install_refusal(activation: Activation, accel: &str) -> Option<String> {
    (activation == Activation::Portal).then(|| {
        format!(
            "refusing to bind {accel}: activation is Portal.\n  \
             A portal daemon takes its key from the GlobalShortcuts portal and opens no \
             control socket, so this binding would do nothing.\n  \
             Worse, GNOME serves both from gsd-media-keys, so it would shadow the portal's \
             own binding and stop the key that already works.\n  \
             Change the key in Settings → Keyboard (it is listed under myna), or pass \
             --shortcut {accel} to the daemon to offer it as the portal's preferred trigger."
        )
    })
}

/// `--install-shortcut <accel>`: bind a GNOME custom keybinding to
/// `myna-desktop --toggle`, appending to any existing custom keybindings
/// (never clobbering).
///
/// Refuses under portal activation - see [`shortcut_install_refusal`].
fn install_shortcut(args: &Args, accel: &str) -> ExitCode {
    use std::process::Command;

    let settings = myna_core::Settings::load();
    let resolved = Resolved::new(args, &settings, &SystemDefaults::from_env());
    if let Some(refusal) = shortcut_install_refusal(resolved.activation, accel) {
        eprintln!("{refusal}");
        return ExitCode::FAILURE;
    }

    const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/";
    let kb_schema = format!("{SCHEMA}.custom-keybinding:{PATH}");
    let command = toggle_command();

    let gset = |args: &[&str]| {
        Command::new("gsettings")
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    // Read the current list and append our path if absent (don't clobber).
    let current = Command::new("gsettings")
        .args(["get", SCHEMA, "custom-keybindings"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let current = current.trim();
    let want = format!("'{PATH}'");
    let new_list = if current.contains(&want) {
        current.to_string()
    } else if current.is_empty() || current == "@as []" || current == "[]" {
        format!("['{PATH}']")
    } else {
        // insert after the opening '['
        current.replacen('[', &format!("[{want}, "), 1)
    };

    let ok = gset(&["set", SCHEMA, "custom-keybindings", &new_list])
        && gset(&["set", &kb_schema, "name", "myna dictation"])
        && gset(&["set", &kb_schema, "command", &command])
        && gset(&["set", &kb_schema, "binding", accel]);

    if ok {
        println!("bound {accel} → `{command}`");
        println!("start the daemon (`myna-desktop --socket <path>`), focus a field, tap {accel}.");
        ExitCode::SUCCESS
    } else {
        eprintln!("failed to set the GNOME keybinding via gsettings.");
        eprintln!("bind one manually: Settings → Keyboard → Custom Shortcuts → `{command}`");
        ExitCode::FAILURE
    }
}

/// `--status`: one screen answering "what state is dictation in, and why".
///
/// Every line of this was previously somewhere else - the persisted values in
/// `gsettings`, what they resolved to and why in a journal line printed once at
/// startup, the live state on the bus, the backend socket nowhere at all - so
/// answering the question meant knowing all four places and how they compose.
/// The composition is what this prints: not just the value in force, but where
/// it came from, because "I set that and nothing happened" is the question
/// being asked most of the time.
fn print_status(args: &Args) -> ExitCode {
    let store = myna_core::settings::Store::open();
    let settings = myna_core::Settings::load();
    let system = SystemDefaults::from_env();
    let resolved = Resolved::new(args, &settings, &system);

    println!(
        "settings   {} ({})",
        myna_core::settings::SCHEMA_ID,
        match store {
            Some(_) => "schema installed",
            // The daemon reads defaults in this state, so say so here rather
            // than printing those defaults as if they were someone's choice.
            None => "schema NOT installed - every value below is the built-in default",
        }
    );
    let row = |key: &str, persisted: String, in_force: String, from: &str| {
        println!("  {key:<15} {persisted:<12} -> {in_force:<22} [{from}]");
    };
    row(
        "activation",
        opt(&settings.activation),
        // Packaging is the built-in, and it is the one value that can differ
        // between this invocation and the daemon it is reporting on - an
        // unpackaged `--status` against a running snap resolves Control while
        // the daemon holds Portal. Naming the reason makes that legible
        // instead of looking like a contradiction.
        match (
            resolved.activation,
            args.activation
                .or(nick(&settings.activation))
                .or(nick(&system.activation)),
        ) {
            (activation, None) if std::env::var_os("SNAP").is_some() => {
                format!("{activation:?} (packaged)")
            }
            (activation, None) => format!("{activation:?} (unpackaged)"),
            (activation, Some(_)) => format!("{activation:?}"),
        },
        source(
            args.activation.is_some(),
            settings.activation.is_some(),
            system.activation.is_some(),
        ),
    );
    // The shipped default makes `myna.toggle` inert, and nothing used to say
    // so - the only feedback was a control-socket error naming a socket the
    // daemon was never going to open. Say it where someone debugging looks.
    if resolved.activation == Activation::Portal {
        println!(
            "  {:<15} no control socket in this mode - `--toggle` does nothing; press {}",
            "",
            resolved.hotkey.as_deref().unwrap_or("your shortcut")
        );
    }
    row(
        "language",
        opt(&settings.language),
        resolved
            .language
            .clone()
            .unwrap_or_else(|| "(backend default)".into()),
        source(
            args.language.is_some(),
            settings.language.is_some(),
            system.language.is_some(),
        ),
    );
    row(
        "hotkey",
        opt(&settings.hotkey),
        resolved
            .hotkey
            .clone()
            .unwrap_or_else(|| "(portal default)".into()),
        source(
            args.shortcut.is_some(),
            settings.hotkey.is_some(),
            system.hotkey.is_some(),
        ),
    );
    row(
        "streaming-mode",
        format!("{:?}", settings.streaming_mode).to_lowercase(),
        format!("preedit {}", resolved.preedit),
        source(args.preedit.is_some(), store.is_some(), false),
    );
    println!(
        "  {:<15} {}",
        "",
        preedit_reason(args.preedit, settings.streaming_mode)
    );

    println!("\nbackend");
    match args.backend() {
        None => println!(
            "  {:<15} (none - pass --socket or --backend-dir)",
            "configured"
        ),
        Some(backend) => {
            println!("  {:<15} {}", "configured", backend.describe());
            match backend.resolve() {
                Ok(path) => println!("  {:<15} {}", "resolves to", path.display()),
                Err(e) => {
                    println!("  {:<15} NOT reachable: {e}", "resolves to");
                    // The share is a bind mount inside the snap's namespace,
                    // so an unconfined `--status` cannot see the socket a
                    // perfectly healthy packaged daemon is using. Say that,
                    // rather than reporting someone else's working setup as
                    // broken.
                    if std::env::var_os("SNAP").is_none()
                        && backend.describe().starts_with("/var/snap/")
                    {
                        println!(
                            "  {:<15} (a content share is only visible inside the snap - \
                             `myna.status` reports what the daemon sees)",
                            ""
                        );
                    }
                }
            }
        }
    }

    println!("\ndaemon     {}", myna_desktop::dbus::BUS_NAME);
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(myna_desktop::dbus::status::read()) {
        Ok(status) => {
            println!("  {:<15} {}", "state", status.state);
            println!(
                "  {:<15} {}",
                "status",
                if status.status_message.is_empty() {
                    "(none)".into()
                } else {
                    status.status_message
                }
            );
        }
        // Not running is the common case for someone debugging, and it is an
        // answer, not a failure - so it reads as one and still exits 0.
        Err(e) => println!("  {:<15} not reachable ({e})", "state"),
    }
    ExitCode::SUCCESS
}

/// Which plane a resolved value came from - the same order as [`Resolved`].
fn source(flag: bool, user: bool, system: bool) -> &'static str {
    if flag {
        "flag"
    } else if user {
        "gsettings"
    } else if system {
        "snap set"
    } else {
        "built-in"
    }
}

/// A settings/`snap set` activation nick, where it names one. Used only to ask
/// "did anything *choose* this, or is it packaging?".
fn nick(value: &Option<String>) -> Option<Activation> {
    value.as_deref().and_then(Activation::from_nick)
}

fn opt(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "(unset)".into())
}

fn main() -> ExitCode {
    // StatusMessage is translated by the publisher before it crosses D-Bus,
    // so the HUD and Shell a11y announcer consume the same final label.
    let mut domain = gettextrs::TextDomain::new("myna-desktop");
    if let Ok(dir) = std::env::var("MYNA_DESKTOP_LOCALEDIR") {
        domain = domain.push(dir);
    }
    let _ = domain.init();

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // Non-daemon subcommands first (no IBus / server needed).
    if let Some(accel) = &args.install_shortcut {
        return install_shortcut(&args, accel);
    }
    if args.status {
        return print_status(&args);
    }
    if args.toggle {
        let path = control_path(&args);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        return match rt.block_on(send_toggle(&path)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "cannot reach the myna-desktop daemon at {}: {e}",
                    path.display()
                );
                for line in toggle_failure_hint(&args) {
                    eprintln!("  {line}");
                }
                ExitCode::FAILURE
            }
        };
    }

    if args.backend().is_none() {
        eprintln!("--socket or --backend-dir is required to run the daemon\n\n{USAGE}");
        return ExitCode::FAILURE;
    }

    // Everything resolvable, resolved once: flags, then the user's GSettings,
    // then `snap set myna …`, then the built-in.
    let settings = myna_core::Settings::load();
    let resolved = Resolved::new(&args, &settings, &SystemDefaults::from_env());
    // `--hold` is a portal concept (the portal reports press and release; the
    // control socket only ever delivers a single poke). Rejected against the
    // *resolved* transport rather than ignored, so "hold-to-talk silently does
    // nothing" is not a mode a user can end up in.
    if args.hold && resolved.activation != Activation::Portal {
        eprintln!("--hold only applies to portal activation (add --portal)");
        return ExitCode::FAILURE;
    }
    myna_core::info_log!(
        "settings",
        "activation {:?}, language {}, hotkey {}, preedit {} ({})",
        resolved.activation,
        resolved.language.as_deref().unwrap_or("(backend default)"),
        resolved.hotkey.as_deref().unwrap_or("(portal default)"),
        resolved.preedit,
        preedit_reason(args.preedit, settings.streaming_mode)
    );

    run_headless(args, resolved)
}

/// Default path: a plain tokio runtime + desktop-notification feedback (no
/// focus-perturbing window).
fn run_headless(args: Args, resolved: Resolved) -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.no_dbus {
        rt.block_on(run_controller(
            args,
            resolved,
            NotifyIndicator::new(),
            None,
            None,
        ))
    } else {
        rt.block_on(run_headless_dbus(args, resolved))
    }
}

/// The default indicator path: serve `com.canonical.Myna.Dictation` and publish through
/// it (feature 004 — the HUD consumes it). Falls back to desktop
/// notifications when the session bus is unreachable or the name can't be
/// owned, and dynamically suppresses the notification fallback while any HUD
/// client is registered via `RegisterClient` (the `myna-hud`
/// `com.canonical.Myna.Hud` singletons). `--no-dbus` forces the notification
/// path.
async fn run_headless_dbus(args: Args, resolved: Resolved) -> ExitCode {
    match ZbusBus::serve().await {
        Ok(bus) => {
            let clients = bus.client_registry();
            let readiness = Readiness::new();
            let service = DictationService::new(bus);
            let dbus_indicator = DbusIndicator::new(service.bus(), readiness.clone());
            let notify = NotifyIndicator::new();
            let indicator = DynamicIndicator::new(dbus_indicator, notify, clients);
            let pump_bus = service.bus();
            eprintln!("serving com.canonical.Myna.Dictation on the session bus");
            run_controller(args, resolved, indicator, Some(readiness), Some(pump_bus)).await
        }
        Err(ServeError::AlreadyRunning { owner_pid }) => {
            let who = owner_pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default();
            eprintln!(
                "cannot acquire com.canonical.Myna.Dictation — another myna-desktop already owns it{who}"
            );
            eprintln!("  falling back to desktop notifications for this instance");
            eprintln!("  (the first owner keeps the hotkey; this instance will not receive presses while it lives — stop it first, or use --no-dbus for an intentional second instance)");
            run_controller(args, resolved, NotifyIndicator::new(), None, None).await
        }
        Err(ServeError::Bus(e)) => {
            eprintln!("cannot serve com.canonical.Myna.Dictation ({e}); falling back");
            eprintln!(
                "  (a 'GUID mismatch' means DBUS_SESSION_BUS_ADDRESS is stale - e.g. a tmux/screen"
            );
            eprintln!("   server surviving logout; fix with: export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus)");
            run_controller(args, resolved, NotifyIndicator::new(), None, None).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::iter::Peekable<impl Iterator<Item = String>> {
        v.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
            .peekable()
    }

    #[test]
    fn install_shortcut_requires_accel() {
        // Missing accel must fail — no default.
        let result = parse_args_from(args(&["--install-shortcut"]));
        assert!(
            result.is_err(),
            "--install-shortcut without accel should fail"
        );
        assert!(result
            .unwrap_err()
            .contains("--install-shortcut needs a value"));
    }

    #[test]
    fn install_shortcut_with_accel_succeeds() {
        let result = parse_args_from(args(&["--install-shortcut", "<Super>t"]));
        assert!(
            result.is_ok(),
            "--install-shortcut with accel should succeed"
        );
        assert_eq!(
            result.unwrap().install_shortcut,
            Some("<Super>t".to_string())
        );
    }

    #[test]
    fn install_shortcut_accel_not_consumed_as_next_flag() {
        // Accel that looks like a flag (starts with '-') should still be accepted
        // because --install-shortcut always consumes the next arg.
        let result = parse_args_from(args(&["--install-shortcut", "--not-a-flag"]));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().install_shortcut,
            Some("--not-a-flag".to_string())
        );
    }

    #[test]
    fn portal_shortcut_defaults_to_none() {
        // Without --shortcut, portal mode lets the portal dialog pick the key.
        let result = parse_args_from(args(&["--portal", "--socket", "/tmp/x.sock"]));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().shortcut, None);
    }

    // ── resolved-not-asked switches ─────────────────────────────────────────

    #[test]
    fn activation_defaults_to_unforced() {
        // No activation flag must stay `None` so `Resolved` — not the parser —
        // decides, and the same argv resolves differently packaged vs not.
        let a = parse_args_from(args(&["--socket", "/tmp/x.sock"])).unwrap();
        assert_eq!(a.activation, None);
    }

    /// Settings as they read on a machine where nobody set anything.
    fn unset() -> myna_core::Settings {
        myna_core::Settings::default()
    }

    fn resolved(args: &Args, settings: &myna_core::Settings, system: &SystemDefaults) -> Resolved {
        Resolved::new(args, settings, system)
    }

    #[test]
    fn activation_resolves_from_packaging() {
        // $SNAP is the portal's availability test: GNOME grants a portal app
        // identity only to a packaged app.
        let a = Args::default();
        assert_eq!(
            resolved(&a, &unset(), &SystemDefaults::default()).activation,
            {
                if std::env::var_os("SNAP").is_some() {
                    Activation::Portal
                } else {
                    Activation::Control
                }
            }
        );
        // An explicit flag always wins over the packaging default.
        let forced = Args {
            activation: Some(Activation::Stdin),
            ..Default::default()
        };
        assert_eq!(
            resolved(&forced, &unset(), &SystemDefaults::default()).activation,
            Activation::Stdin
        );
    }

    // ── flag > user setting > `snap set` > built-in ─────────────────────────

    #[test]
    fn the_users_setting_beats_the_system_default() {
        // `snap set myna language=fr` is one admin choosing for every account
        // on the machine; a user who set their own language must keep it.
        let settings = myna_core::Settings {
            language: Some("en".into()),
            ..Default::default()
        };
        let system = SystemDefaults {
            language: Some("fr".into()),
            ..Default::default()
        };
        let r = resolved(&Args::default(), &settings, &system);
        assert_eq!(r.language.as_deref(), Some("en"));
    }

    #[test]
    fn the_system_default_applies_where_the_user_set_nothing() {
        // …and it has to, or `snap set` on an image build would do nothing.
        let system = SystemDefaults {
            language: Some("fr".into()),
            hotkey: Some("<Super>d".into()),
            activation: Some("control".into()),
        };
        let r = resolved(&Args::default(), &unset(), &system);
        assert_eq!(r.language.as_deref(), Some("fr"));
        assert_eq!(r.hotkey.as_deref(), Some("<Super>d"));
        assert_eq!(r.activation, Activation::Control);
    }

    #[test]
    fn a_flag_beats_both() {
        let a = Args {
            language: Some("de".into()),
            shortcut: Some("<Super>x".into()),
            activation: Some(Activation::Stdin),
            ..Default::default()
        };
        let settings = myna_core::Settings {
            language: Some("en".into()),
            hotkey: Some("<Super>t".into()),
            activation: Some("portal".into()),
            ..Default::default()
        };
        let system = SystemDefaults {
            language: Some("fr".into()),
            hotkey: Some("<Super>d".into()),
            activation: Some("control".into()),
        };
        let r = resolved(&a, &settings, &system);
        assert_eq!(r.language.as_deref(), Some("de"));
        assert_eq!(r.hotkey.as_deref(), Some("<Super>x"));
        assert_eq!(r.activation, Activation::Stdin);
    }

    #[test]
    fn an_unparseable_activation_falls_through_to_packaging() {
        // A typo in `snap set myna activation=portl` must not silently select a
        // transport: the hotkey doing nothing is the worst failure this daemon
        // has, and packaging is the answer that is always right.
        let settings = myna_core::Settings {
            activation: Some("portl".into()),
            ..Default::default()
        };
        assert_eq!(
            resolved(&Args::default(), &settings, &SystemDefaults::default()).activation,
            Activation::from_packaging()
        );
    }

    #[test]
    fn conflicting_activation_flags_are_rejected() {
        // Last-one-wins would make `--portal --stdin` look like it worked.
        let err =
            parse_args_from(args(&["--portal", "--stdin", "--socket", "/tmp/x.sock"])).unwrap_err();
        assert!(err.contains("conflicting activation flags"), "{err}");
        // Repeating the same flag is harmless, not a conflict.
        assert!(
            parse_args_from(args(&["--portal", "--portal", "--socket", "/tmp/x.sock"])).is_ok()
        );
    }

    #[test]
    fn hold_requires_portal_activation() {
        // hold-to-talk needs press *and* release; the control socket only ever
        // delivers a single poke. The check moved out of the parser when
        // settings gained an activation key - it is the *resolved* transport
        // that decides, and the parser cannot see settings.
        let control = parse_args_from(args(&["--control", "--hold", "--socket", "/tmp/x.sock"]))
            .expect("parsing no longer rejects this");
        let r = resolved(&control, &unset(), &SystemDefaults::default());
        assert!(control.hold && r.activation != Activation::Portal);
        let portal = parse_args_from(args(&["--portal", "--hold", "--socket", "/tmp/x.sock"]))
            .expect("portal + hold parses");
        let r = resolved(&portal, &unset(), &SystemDefaults::default());
        assert!(portal.hold && r.activation == Activation::Portal);
    }

    #[test]
    fn dbus_is_the_default_and_no_dbus_opts_out() {
        // The bus is served unless explicitly refused: it degrades to
        // notifications on its own, so there is nothing for a user to choose.
        assert!(
            !parse_args_from(args(&["--socket", "/tmp/x.sock"]))
                .unwrap()
                .no_dbus
        );
        assert!(
            parse_args_from(args(&["--no-dbus", "--socket", "/tmp/x.sock"]))
                .unwrap()
                .no_dbus
        );
        // The old `--dbus` spelling is gone, not silently accepted.
        assert!(parse_args_from(args(&["--dbus", "--socket", "/tmp/x.sock"])).is_err());
    }

    /// `--status` reports *which* plane won, so the attribution has to be the
    /// same order `Resolved` applies - a status line that says "gsettings"
    /// where a flag actually won is worse than no status line.
    #[test]
    fn the_reported_source_follows_the_precedence() {
        assert_eq!(source(true, true, true), "flag");
        assert_eq!(source(false, true, true), "gsettings");
        assert_eq!(source(false, false, true), "snap set");
        assert_eq!(source(false, false, false), "built-in");
    }

    #[test]
    fn status_is_a_subcommand_not_a_daemon_flag() {
        // It must parse without --socket/--backend-dir: the state worth
        // reporting includes "no backend configured".
        assert!(parse_args_from(args(&["--status"])).unwrap().status);
        assert!(!parse_args_from(args(&["--toggle"])).unwrap().status);
    }

    #[test]
    fn preedit_is_tri_state() {
        // Unset means "resolve from the persisted mode", which is distinct from
        // an explicit off — `--no-preedit` must survive a streaming tier.
        assert_eq!(
            parse_args_from(args(&["--socket", "/tmp/x.sock"]))
                .unwrap()
                .preedit,
            None
        );
        assert_eq!(
            parse_args_from(args(&["--preedit", "--socket", "/tmp/x.sock"]))
                .unwrap()
                .preedit,
            Some(true)
        );
        assert_eq!(
            parse_args_from(args(&["--no-preedit", "--socket", "/tmp/x.sock"]))
                .unwrap()
                .preedit,
            Some(false)
        );
    }

    #[test]
    fn forced_preedit_skips_the_tier_gate() {
        // Overrides must not consult settings/tier state at all — that is what
        // makes them usable for debugging on any machine.
        assert!(resolve_preedit(Some(true), myna_core::StreamingMode::Batch));
        assert!(!resolve_preedit(
            Some(false),
            myna_core::StreamingMode::Streaming
        ));
    }

    #[test]
    fn portal_shortcut_explicit() {
        let result = parse_args_from(args(&[
            "--portal",
            "--shortcut",
            "<Super>t",
            "--socket",
            "/tmp/x.sock",
        ]));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().shortcut, Some("<Super>t".to_string()));
    }

    // Regression (manual test report, 2026-07-31): the HUD pill visibly
    // "blinked" on (re)start — recording→loading→recording published in
    // quick succession because `readiness.reset()` used to run lazily inside
    // the session's `async move` block, only once that future was first
    // polled. The controller publishes `IndicatorState::Recording`
    // *synchronously* right after `session.start()` returns (before the
    // future is ever polled), so a `ready_seen` left `true` from the
    // *previous* utterance made that first publish read `"recording"`
    // instead of the correct cold-start `"loading"`.
    //
    // `make_session`'s returned factory must reset `readiness` synchronously,
    // the moment it's called — provably before the returned `run` future is
    // ever polled/awaited.
    #[test]
    fn make_session_resets_readiness_synchronously_before_run_is_polled() {
        let args = Args {
            socket: Some(PathBuf::from("/tmp/myna-desktop-test-unused.sock")),
            ..Default::default()
        };
        let readiness = Readiness::new();
        // Simulate a previous utterance that reached Ready — the exact stale
        // state that triggered the flicker.
        readiness.note_ready();
        assert!(
            readiness.ready_seen(),
            "test setup: readiness should start warm"
        );

        let resolved = Resolved::new(
            &args,
            &myna_core::Settings::default(),
            &SystemDefaults::default(),
        );
        let mut factory = make_session(
            &args,
            &LiveSettings::new(&resolved),
            Some(readiness.clone()),
            None,
        );
        let (events_tx, _events_rx) = mpsc::channel(1);
        // Calling the factory is synchronous; `run` below is deliberately
        // never polled/awaited, proving the reset can't be hiding inside it.
        let (_run, _stop) = factory(events_tx);

        assert!(
            !readiness.ready_seen(),
            "readiness must already be reset by the time session.start() \
             returns, before the controller's next set_state(Recording) \
             call — not lazily inside the unpolled session future"
        );
    }

    // A portal daemon never opens a control socket, so `--toggle` failing is
    // its *healthy* behaviour. The hint has to say that instead of blaming a
    // missing process, and it has to name the key that does work.
    #[test]
    fn portal_toggle_hint_points_at_the_hotkey_not_a_missing_daemon() {
        let hint = toggle_hint_for(Activation::Portal, Some("<Super>t")).join(" ");
        assert!(
            hint.contains("<Super>t"),
            "should name the bound key: {hint}"
        );
        assert!(
            !hint.contains("--socket"),
            "must not send the user looking for a dead daemon: {hint}"
        );
    }

    // Unbound is the shipped state (the daemon offers no preferred trigger),
    // so the no-hotkey branch still has to give somewhere to look.
    #[test]
    fn portal_toggle_hint_without_a_hotkey_says_where_to_find_one() {
        let hint = toggle_hint_for(Activation::Portal, None).join(" ");
        assert!(
            hint.contains("Keyboard"),
            "should point at Settings: {hint}"
        );
    }

    // Control activation is the one case the old advice was right for.
    #[test]
    fn control_toggle_hint_still_asks_whether_the_daemon_is_running() {
        let hint = toggle_hint_for(Activation::Control, None).join(" ");
        assert!(hint.contains("--socket"), "{hint}");
    }

    // The regression that started this: `myna.install-shortcut '<Super>t'` on a
    // snap (portal by default) installed an inert custom keybinding that
    // *shadowed* the portal's working one, silently killing dictation.
    #[test]
    fn install_shortcut_refuses_under_portal_activation() {
        let refusal = shortcut_install_refusal(Activation::Portal, "<Super>t")
            .expect("portal activation must refuse");
        assert!(refusal.contains("<Super>t"), "{refusal}");
        assert!(
            refusal.contains("shadow"),
            "must say why it is destructive, not just that it is useless: {refusal}"
        );
    }

    // Control activation is what the flag is *for*; it must stay usable.
    #[test]
    fn install_shortcut_allowed_under_control_activation() {
        assert!(shortcut_install_refusal(Activation::Control, "<Super>t").is_none());
    }
}
