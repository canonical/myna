//! Pure presenter for the About/Diagnostics page and the refresh budgets.
//!
//! GTK-independent so it can be exercised headlessly. The report is built from
//! facts this crate produces itself - machine shape, process memory, engine and
//! model names, versions - plus error text from outside: the daemon's last
//! error detail, gnome-shell's extension error and each backend surface's
//! failure. None of those sources carries audio or dictated text, so the
//! privacy contract holds by construction; error text only goes through
//! [`redact_text`] for secrets and the user's home directory.

use std::time::Duration;

use crate::machine::{bytes, AudioDrops, DaemonReport, LastError, MachineFacts, ProcessMemory};
use crate::onboarding::{ExtensionCopy, ExtensionReport};
use crate::performance::{
    assess_clock, assess_pressure, ClockClass, ClockVerdict, PerformanceFacts, PressureWarning,
};
use myna_platform::components::{Blocker, ComponentStatus, Purpose};

/// Upper bound on subprocess spawns required to refresh a single backend
/// snapshot: `snap info`, at most four prioritized modelctl candidate probes,
/// and the four modelctl data commands (status, get, list-models,
/// list-engines).
pub const BACKEND_REFRESH_PROCESS_BUDGET: usize = 9;

const PLACEHOLDER_REDACTED: &str = "[redacted]";

/// Substrings that mark a `--flag=value` (or `flag=value`) pair as carrying a
/// secret whose value must be scrubbed.
const SENSITIVE_KEY_SUBSTRINGS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "auth",
    "credential",
    "cookie",
    "api-key",
    "apikey",
];

/// One installed snap as parsed from `snap list` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSnap {
    pub name: String,
    pub version: String,
}

/// Per-backend diagnostic summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackendDiagnostic {
    pub snap_name: String,
    pub version: String,
    pub connection: DiagnosticConnection,
    pub engine: Option<String>,
    pub model: Option<String>,
    pub services: Vec<String>,
    pub memory: Option<ProcessMemory>,
    /// One already-worded sentence per problem. Never a command line.
    pub problems: Vec<String>,
}

/// Connection state rendered in diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticConnection {
    Connected,
    MultipleConnections,
    #[default]
    NotConnected,
}

/// Aggregated inputs to the diagnostics presenter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticInput {
    pub inventory_complete: bool,
    pub installed_snaps: Vec<InstalledSnap>,
    pub machine: Option<MachineFacts>,
    pub daemon: Option<ProcessMemory>,
    /// What the running daemon publishes; `None` when it is not reachable.
    pub daemon_report: Option<DaemonReport>,
    /// Which copy of the shell extension runs; `None` when not read.
    pub extension: Option<ExtensionReport>,
    /// The desktop's other pieces, where it has no extension to report on.
    pub desktop: Vec<crate::platform::ComponentFact>,
    /// `None` until the probe has run once; the report says so rather than
    /// claiming a clock it did not measure.
    pub performance: Option<PerformanceFacts>,
    pub backends: Vec<BackendDiagnostic>,
    pub problems: Vec<String>,
}

/// Which onboarding state the About/Diagnostics page should surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingState {
    /// The Myna snap itself is not installed.
    NoMyna,
    /// The Myna snap is installed but no backend snap has been seen.
    NoBackend,
    /// A model is installed but none is connected to `myna:backend`, so
    /// dictation fails with "Model not connected".
    NoModelConnected,
    /// Nothing to onboard; at least one discovered model is connected.
    Ready,
    /// Installation state could not be determined.
    Unavailable,
}

/// One thing that will make dictation slow, already worded for the page.
/// Unlike a problem, a warning does not block the ready state: the machine is
/// set up, it is just not currently delivering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// What is wrong, one sentence.
    pub cause: String,
    /// What to do about it, one sentence.
    pub remedy: String,
}

/// Output of the diagnostics presenter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticReport {
    onboarding: OnboardingState,
    warnings: Vec<Warning>,
    body: String,
    hanging: Vec<usize>,
}

impl DiagnosticReport {
    pub fn onboarding(&self) -> OnboardingState {
        self.onboarding
    }

    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    pub fn copy_text(&self) -> String {
        self.body.clone()
    }

    /// Per line of [`Self::copy_text`], the column its wrapped continuation
    /// lines align to: a field's value column, else the line's own indent.
    pub fn continuation_columns(&self) -> &[usize] {
        &self.hanging
    }
}

/// Errors returned by [`parse_snap_list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapListError {
    Malformed { line_number: usize },
}

impl std::fmt::Display for SnapListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapListError::Malformed { line_number } => {
                write!(f, "malformed `snap list` row {line_number}")
            }
        }
    }
}

impl std::error::Error for SnapListError {}

/// Parses `snap list` output into (name, version) pairs.
///
/// The empty-message form emitted by snapd (`No snaps are installed yet…`) is
/// treated as an empty list. A well-formed table must have a header row
/// followed by rows containing at least a name and a version; any shorter row
/// is reported as [`SnapListError::Malformed`].
pub fn parse_snap_list(input: &str) -> Result<Vec<InstalledSnap>, SnapListError> {
    let mut lines = input
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());
    let Some((_, first)) = lines.next() else {
        return Ok(Vec::new());
    };
    let trimmed = first.trim();
    if trimmed.starts_with("No snaps are installed") || trimmed.starts_with("No snaps installed") {
        return Ok(Vec::new());
    }
    if !trimmed.split_whitespace().take(2).eq(["Name", "Version"]) {
        return Err(SnapListError::Malformed { line_number: 1 });
    }
    // Otherwise the first non-empty line is treated as the header and skipped.
    let mut snaps = Vec::new();
    for (number, line) in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            return Err(SnapListError::Malformed {
                line_number: number + 1,
            });
        }
        snaps.push(InstalledSnap {
            name: fields[0].to_owned(),
            version: fields[1].to_owned(),
        });
    }
    Ok(snaps)
}

/// Coalesces user-initiated refreshes. There is no periodic refresh.
pub const REFRESH_DEBOUNCE: Duration = Duration::from_millis(250);

/// Produce the diagnostics report for the given input.
pub fn present_diagnostics(input: DiagnosticInput) -> DiagnosticReport {
    let onboarding = classify_onboarding(&input);
    let warnings = input
        .performance
        .as_ref()
        .map(performance_warnings)
        .unwrap_or_default();
    let (body, hanging) = align_fields(&render_body(&input, onboarding, &warnings));
    DiagnosticReport {
        onboarding,
        warnings,
        body,
        hanging,
    }
}

fn performance_warnings(facts: &PerformanceFacts) -> Vec<Warning> {
    let mut warnings = Vec::new();
    match assess_clock(&facts.clock) {
        ClockVerdict::Unknown | ClockVerdict::Healthy => {}
        ClockVerdict::SoftwareCap {
            cpu,
            policy_max_khz,
            hardware_max_khz,
        } => warnings.push(Warning {
            cause: format!(
                "{} (cpu{} {} {} {} {})",
                gettextrs::gettext("The CPU clock is capped by system policy"),
                cpu,
                gettextrs::gettext("limited to"),
                ghz(policy_max_khz),
                gettextrs::gettext("of"),
                ghz(hardware_max_khz),
            ),
            remedy: gettextrs::gettext(
                "Dictation will be slow. A power tool has capped the CPU clock.",
            ),
        }),
        ClockVerdict::LowPowerProfile {
            cpu,
            achieved_khz,
            hardware_max_khz,
        } => warnings.push(Warning {
            cause: format!(
                "{} ({})",
                gettextrs::gettext("The power profile is set to low-power"),
                reached(cpu, achieved_khz, hardware_max_khz),
            ),
            remedy: gettextrs::gettext(
                "Dictation will be slow. Switch to Balanced or Performance in the system power settings.",
            ),
        }),
        ClockVerdict::FirmwareClamp {
            cpu,
            achieved_khz,
            hardware_max_khz,
        } => warnings.push(Warning {
            cause: format!(
                "{} ({})",
                gettextrs::gettext("Firmware is holding the CPU at its lowest clock"),
                reached(cpu, achieved_khz, hardware_max_khz),
            ),
            remedy: gettextrs::gettext(
                "Dictation will be slow. Unplug the charger, wait a few seconds, and plug it back in. If that does not help, power off fully and start again.",
            ),
        }),
    }
    for warning in facts.pressure.map(assess_pressure).unwrap_or_default() {
        warnings.push(match warning {
            PressureWarning::Memory(stall) => Warning {
                cause: format!(
                    "{} ({})",
                    gettextrs::gettext("The system is short of memory"),
                    stalled(stall)
                ),
                remedy: gettextrs::gettext(
                    "Dictation will stutter. Close applications until the machine stops swapping.",
                ),
            },
            PressureWarning::Io(stall) => Warning {
                cause: format!(
                    "{} ({})",
                    gettextrs::gettext("Disk activity is stalling the system"),
                    stalled(stall)
                ),
                remedy: gettextrs::gettext(
                    "Dictation will stutter. Wait for the transfer or indexing to finish.",
                ),
            },
            PressureWarning::Cpu(stall) => Warning {
                cause: format!(
                    "{} ({})",
                    gettextrs::gettext("Other programs are saturating the CPU"),
                    stalled(stall)
                ),
                remedy: gettextrs::gettext(
                    "Dictation will be slow. Pause the build or workload before dictating.",
                ),
            },
        });
    }
    warnings
}

fn reached(cpu: u32, achieved_khz: u64, hardware_max_khz: u64) -> String {
    format!(
        "cpu{cpu} {} {} {} {}",
        gettextrs::gettext("reached"),
        ghz(achieved_khz),
        gettextrs::gettext("of"),
        ghz(hardware_max_khz)
    )
}

/// A pressure-stall share, from hundredths of a percent.
fn stalled(hundredths: u32) -> String {
    format!(
        "{} {}",
        percent(hundredths),
        gettextrs::gettext("of the last 10 s stalled")
    )
}

/// A share in hundredths of a percent, as "1.05%".
fn percent(hundredths: u32) -> String {
    format!("{}.{:02}%", hundredths / 100, hundredths % 100)
}

fn ghz(khz: u64) -> String {
    format!("{:.2} GHz", khz as f64 / 1_000_000.0)
}

fn classify_onboarding(input: &DiagnosticInput) -> OnboardingState {
    if !input.inventory_complete || !input.problems.is_empty() {
        return OnboardingState::Unavailable;
    }
    let has_myna = input.installed_snaps.iter().any(|snap| snap.name == "myna");
    if !has_myna {
        return OnboardingState::NoMyna;
    }
    if input.backends.is_empty() {
        return OnboardingState::NoBackend;
    }
    if input
        .backends
        .iter()
        .all(|backend| backend.connection == DiagnosticConnection::NotConnected)
    {
        return OnboardingState::NoModelConnected;
    }
    OnboardingState::Ready
}

/// Localized, user-facing label for an onboarding state.
pub fn onboarding_state_label(state: OnboardingState) -> String {
    match state {
        OnboardingState::NoMyna => gettextrs::gettext("Dictation is not installed"),
        OnboardingState::NoBackend => gettextrs::gettext("No model installed"),
        OnboardingState::NoModelConnected => gettextrs::gettext("No model connected"),
        OnboardingState::Ready => gettextrs::gettext("Ready"),
        OnboardingState::Unavailable => gettextrs::gettext("Installation status unavailable"),
    }
}

/// Localized, user-facing label for a backend connection state.
pub fn diagnostic_connection_label(connection: DiagnosticConnection) -> String {
    match connection {
        DiagnosticConnection::Connected => gettextrs::gettext("Connected"),
        DiagnosticConnection::MultipleConnections => gettextrs::gettext("Multiple connections"),
        DiagnosticConnection::NotConnected => gettextrs::gettext("Not connected"),
    }
}

fn render_body(
    input: &DiagnosticInput,
    onboarding: OnboardingState,
    warnings: &[Warning],
) -> String {
    let mut out = String::new();
    out.push_str(&gettextrs::gettext("Myna Settings"));
    out.push(' ');
    out.push_str(env!("MYNA_VERSION"));
    out.push('\n');
    out.push_str(&gettextrs::gettext("Onboarding"));
    out.push_str(": ");
    out.push_str(&onboarding_state_label(onboarding));
    out.push('\n');

    out.push('\n');
    out.push_str(&gettextrs::gettext("Machine"));
    out.push_str(":\n");
    match &input.machine {
        Some(machine) => {
            field(&mut out, &gettextrs::gettext("CPU"), &machine.cpu);
            field(&mut out, &gettextrs::gettext("Memory"), &machine.memory);
            if machine.gpus.is_empty() {
                field(
                    &mut out,
                    &gettextrs::gettext("GPU"),
                    &gettextrs::gettext("none"),
                );
            }
            for gpu in &machine.gpus {
                field(&mut out, &gettextrs::gettext("GPU"), gpu);
            }
        }
        None => field(
            &mut out,
            &gettextrs::gettext("CPU"),
            &gettextrs::gettext("(no model answered show-machine)"),
        ),
    }

    out.push('\n');
    out.push_str(&gettextrs::gettext("Performance"));
    out.push_str(":\n");
    match &input.performance {
        Some(facts) => {
            if facts.clock.classes.is_empty() {
                field(
                    &mut out,
                    &gettextrs::gettext("Clock"),
                    &gettextrs::gettext("(no cpufreq information)"),
                );
            }
            for class in &facts.clock.classes {
                field(
                    &mut out,
                    &gettextrs::gettext("Clock"),
                    &clock_summary(class),
                );
            }
            if let Some(profile) = &facts.clock.power.platform_profile {
                field(&mut out, &gettextrs::gettext("Profile"), profile);
            }
            if let Some(power) = power_summary(&facts.clock.power) {
                field(&mut out, &gettextrs::gettext("Power"), &power);
            }
            if let Some(pressure) = facts.pressure {
                field(
                    &mut out,
                    &gettextrs::gettext("Pressure"),
                    &gettextrs::gettext(
                        // TRANSLATORS: {cpu}, {memory} and {io} are shares such as "0.03%"; cpu, memory and io are kernel resource names, keep them.
                        "cpu {cpu}, memory {memory}, io {io} of the last 10 s stalled",
                    )
                    .replace("{cpu}", &percent(pressure.cpu_some))
                    .replace("{memory}", &percent(pressure.memory_some))
                    .replace("{io}", &percent(pressure.io_full)),
                );
            }
        }
        None => field(
            &mut out,
            &gettextrs::gettext("Clock"),
            &gettextrs::gettext("(not measured yet)"),
        ),
    }

    out.push('\n');
    out.push_str(&gettextrs::gettext("Daemon"));
    out.push_str(":\n");
    let myna = input
        .installed_snaps
        .iter()
        .find(|snap| snap.name == "myna");
    match (myna, input.daemon) {
        (Some(snap), Some(memory)) => {
            field(&mut out, &gettextrs::gettext("Version"), &snap.version);
            field(
                &mut out,
                &gettextrs::gettext("Memory"),
                &memory_summary(memory),
            );
            let report = input.daemon_report.clone().unwrap_or_default();
            if let Some(drops) = report.drops {
                field(
                    &mut out,
                    &gettextrs::gettext("Audio"),
                    &drops_summary(drops),
                );
            }
            field(
                &mut out,
                &gettextrs::gettext("Last error"),
                &last_error_summary(report.last_error.as_ref()),
            );
        }
        (Some(snap), None) => {
            field(&mut out, &gettextrs::gettext("Version"), &snap.version);
            field(
                &mut out,
                &gettextrs::gettext("Process"),
                &gettextrs::gettext("not running"),
            );
        }
        (None, _) => field(
            &mut out,
            &gettextrs::gettext("Version"),
            &gettextrs::gettext("not installed"),
        ),
    }
    if let Some(extension) = &input.extension {
        field(
            &mut out,
            &gettextrs::gettext("Shell extension"),
            &extension_summary(extension),
        );
    }
    for fact in &input.desktop {
        field(
            &mut out,
            &component_label(fact.purpose),
            &component_summary(fact.purpose, fact.status),
        );
    }

    out.push('\n');
    out.push_str(&gettextrs::gettext("Models"));
    out.push_str(":\n");
    if input.backends.is_empty() {
        out.push_str("  ");
        out.push_str(&gettextrs::gettext("(none discovered)"));
        out.push('\n');
    }
    for backend in &input.backends {
        out.push_str("  ");
        out.push_str(&backend.snap_name);
        if !backend.version.is_empty() {
            out.push(' ');
            out.push_str(&backend.version);
        }
        out.push_str(" - ");
        out.push_str(&diagnostic_connection_label(backend.connection));
        out.push('\n');
        field(
            &mut out,
            &gettextrs::gettext("Engine"),
            backend
                .engine
                .as_deref()
                .unwrap_or(&gettextrs::gettext("none selected")),
        );
        if let Some(model) = &backend.model {
            field(&mut out, &gettextrs::gettext("Model"), model);
        }
        if !backend.services.is_empty() {
            field(
                &mut out,
                &gettextrs::gettext("Services"),
                &backend.services.join(", "),
            );
        }
        if let Some(memory) = backend.memory {
            field(
                &mut out,
                &gettextrs::gettext("Memory"),
                &memory_summary(memory),
            );
        }
    }

    out.push('\n');
    out.push_str(&gettextrs::gettext("Problems"));
    out.push_str(":\n");
    let not_connected = (onboarding == OnboardingState::NoModelConnected)
        .then(|| gettextrs::gettext("No model is connected. Choose one to use for dictation."));
    let problems: Vec<&String> = input
        .problems
        .iter()
        .chain(not_connected.as_ref())
        .chain(input.backends.iter().flat_map(|backend| &backend.problems))
        .collect();
    if problems.is_empty() {
        out.push_str("  ");
        out.push_str(&gettextrs::gettext("(none)"));
        out.push('\n');
    }
    for problem in problems {
        out.push_str("  ");
        out.push_str(problem);
        out.push('\n');
    }

    out.push('\n');
    out.push_str(&gettextrs::gettext("Warnings"));
    out.push_str(":\n");
    if warnings.is_empty() {
        out.push_str("  ");
        out.push_str(&gettextrs::gettext("(none)"));
        out.push('\n');
    }
    for warning in warnings {
        out.push_str("  ");
        out.push_str(&warning.cause);
        out.push_str(". ");
        out.push_str(&warning.remedy);
        out.push('\n');
    }

    out
}

/// One frequency class: what one loaded core reached against what the
/// silicon and the policy allow. The probe number is the fact; the two
/// ceilings say which layer is in the way when it is low.
fn clock_summary(class: &ClockClass) -> String {
    let achieved = class
        .achieved_khz
        .map(ghz)
        .unwrap_or_else(|| gettextrs::gettext("not probed"));
    let mut summary = format!(
        "cpu{} {} {} {} {}",
        class.cpu,
        gettextrs::gettext("reached"),
        achieved,
        gettextrs::gettext("of"),
        ghz(class.hardware_max_khz),
    );
    if class.policy_max_khz < class.hardware_max_khz {
        // The knob is named here, in the report, not in the warning.
        summary.push_str(&format!(
            " ({} {}, scaling_max_freq)",
            gettextrs::gettext("policy allows"),
            ghz(class.policy_max_khz)
        ));
    }
    summary.push_str(&format!(
        ", {} {}",
        class.cores,
        gettextrs::gettext("cores in this class")
    ));
    summary
}

fn power_summary(power: &crate::performance::PowerFacts) -> Option<String> {
    let mains = match power.on_mains? {
        true => gettextrs::gettext("mains"),
        false => gettextrs::gettext("battery"),
    };
    Some(match &power.battery_status {
        Some(status) => format!(
            "{mains}, {} {}",
            gettextrs::gettext("battery"),
            status.to_lowercase()
        ),
        None => mains,
    })
}

/// Separates a field's label from its value until [`align_fields`] pads it.
/// A control character, so no translated label can contain it.
const FIELD_SEPARATOR: char = '\u{1f}';

fn field(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("    {label}{FIELD_SEPARATOR}{value}\n"));
}

/// Pads every field label to the widest one, so the values form one column
/// whatever the translation's label lengths. Also returns each line's
/// continuation column (see [`DiagnosticReport::continuation_columns`]).
fn align_fields(body: &str) -> (String, Vec<usize>) {
    let width = body
        .lines()
        .filter_map(|line| line.split_once(FIELD_SEPARATOR))
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::with_capacity(body.len());
    let mut hanging = Vec::new();
    for line in body.lines() {
        match line.split_once(FIELD_SEPARATOR) {
            Some((label, value)) => {
                out.push_str(&format!("{label:<width$} {value}"));
                hanging.push(width + 1);
            }
            None => {
                out.push_str(line);
                hanging.push(line.chars().take_while(|c| *c == ' ').count());
            }
        }
        out.push('\n');
    }
    (out, hanging)
}

/// Always printed when the daemon is up: a confirmed zero is the fact worth
/// having during a bug hunt.
fn drops_summary(drops: AudioDrops) -> String {
    format!(
        "{} {}",
        drops.not_active,
        gettextrs::gettext("chunks dropped this session")
    )
}

fn component_label(purpose: Purpose) -> String {
    match purpose {
        Purpose::TextInput => gettextrs::gettext("Input method"),
        Purpose::StatusSurface => gettextrs::gettext("Indicator host"),
    }
}

fn component_summary(purpose: Purpose, status: ComponentStatus) -> String {
    match status {
        ComponentStatus::Active => gettextrs::gettext("active"),
        ComponentStatus::ActiveAfterRelogin => {
            gettextrs::gettext("set up, starts at the next login")
        }
        ComponentStatus::Inactive => gettextrs::gettext("not running"),
        ComponentStatus::NeedsRelogin => gettextrs::gettext("not set up"),
        ComponentStatus::Blocked(Blocker::TurnedOff) => gettextrs::gettext("turned off"),
        ComponentStatus::Blocked(Blocker::Locked) => {
            gettextrs::gettext("locked by the administrator")
        }
        ComponentStatus::Blocked(Blocker::Shadowed) => gettextrs::gettext("hidden by another copy"),
        ComponentStatus::Blocked(Blocker::Incompatible) => match purpose {
            Purpose::TextInput => gettextrs::gettext("another input method is in use"),
            Purpose::StatusSurface => gettextrs::gettext("not supported on this desktop"),
        },
        ComponentStatus::Failed => gettextrs::gettext("failed"),
        ComponentStatus::Unavailable => gettextrs::gettext("not installed"),
    }
}

/// `<state>, <copy>`, plus gnome-shell's error when it has one. The copy is
/// a classification, never the path.
fn extension_summary(report: &ExtensionReport) -> String {
    match report {
        ExtensionReport::NoShell => gettextrs::gettext("gnome-shell did not answer"),
        ExtensionReport::NotInstalled => gettextrs::gettext("not installed"),
        ExtensionReport::SinceLogin {
            at_next_login: true,
        } => gettextrs::gettext("installed since login, starts at the next one"),
        ExtensionReport::SinceLogin {
            at_next_login: false,
        } => gettextrs::gettext("installed since login, not enabled"),
        ExtensionReport::Known { state, copy, error } => {
            let copy = match copy {
                ExtensionCopy::MynaConfigPackage => gettextrs::gettext("myna-config package"),
                ExtensionCopy::UbuntuPackage => gettextrs::gettext("Ubuntu package"),
                ExtensionCopy::DevelopmentOverride => gettextrs::gettext("development override"),
                ExtensionCopy::UserCopy => gettextrs::gettext("user copy"),
                ExtensionCopy::OtherSystemCopy => gettextrs::gettext("other system copy"),
            };
            let mut summary = format!("{state}, {copy}");
            if let Some(error) = error {
                summary.push_str(&format!(" ({})", redact_text(error)));
            }
            summary
        }
    }
}

/// `<headline> (<detail>), <local time>`, or "none". The detail is the one
/// value here that came from outside, so it is redacted like any other.
fn last_error_summary(last: Option<&LastError>) -> String {
    let Some(last) = last else {
        return gettextrs::gettext("none");
    };
    let mut summary = last.headline.clone();
    if !last.detail.is_empty() {
        summary.push_str(&format!(" ({})", redact_text(&last.detail)));
    }
    if !last.at.is_empty() {
        summary.push_str(&format!(", {}", last.at));
    }
    summary
}

fn memory_summary(memory: ProcessMemory) -> String {
    format!(
        "{} now, {} peak (pid {})",
        bytes(memory.resident),
        bytes(memory.peak),
        memory.pid
    )
}

/// Redact a value from outside this crate: write the user's home as `~`
/// and scrub `key=value` pairs whose key looks
/// sensitive. Everything else
/// stays, since an error is useless without its words; no source that
/// reaches here carries dictated text.
pub fn redact_text(value: &str) -> String {
    let scrubbed = scrub_secret_assignments(value);
    scrub_paths(&scrubbed)
}

/// Keep every path, since `/sys/bus/usb/devices` is the diagnosis, but write
/// the user's home directory as `~`.
fn scrub_paths(value: &str) -> String {
    static HOME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    scrub_home(
        value,
        HOME.get_or_init(|| gio::glib::home_dir().to_string_lossy().into_owned()),
    )
}

fn scrub_home(value: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find(home) {
        let start = cursor + relative;
        let end = start + home.len();
        let before = value[..start].chars().next_back();
        let after = value[end..].chars().next();
        out.push_str(&value[cursor..start]);
        let inside_a_longer_path =
            before.is_some_and(|character| is_name_char(character) || character == '/');
        if inside_a_longer_path || after.is_some_and(is_name_char) {
            out.push_str(home);
        } else {
            out.push('~');
        }
        cursor = end;
    }
    out.push_str(&value[cursor..]);
    out
}

fn is_name_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.')
}

fn scrub_secret_assignments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for token in value.split_inclusive(char::is_whitespace) {
        let (word, trailing) = split_trailing_whitespace(token);
        if let Some(eq) = word.find('=') {
            let (key, rest) = word.split_at(eq);
            let key_lower = key.trim_start_matches('-').to_ascii_lowercase();
            if SENSITIVE_KEY_SUBSTRINGS
                .iter()
                .any(|needle| key_lower.contains(needle))
            {
                out.push_str(key);
                out.push('=');
                out.push_str(PLACEHOLDER_REDACTED);
                out.push_str(trailing);
                // Skip the value; move on.
                let _ = rest;
                continue;
            }
        }
        out.push_str(token);
    }
    out
}

fn split_trailing_whitespace(token: &str) -> (&str, &str) {
    let trimmed = token.trim_end_matches(|c: char| c.is_whitespace());
    (trimmed, &token[trimmed.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value of the field labelled `label`, or `None` when the report
    /// has no such line.
    fn field_value<'a>(text: &'a str, label: &str) -> Option<&'a str> {
        text.lines().find_map(|line| {
            line.trim_start()
                .strip_prefix(label)
                .filter(|rest| rest.starts_with(' '))
                .map(str::trim_start)
        })
    }

    /// A wrapped value continues under its own column, not at the margin.
    #[test]
    fn continuations_align_to_the_value_or_the_indent() {
        let report = present_diagnostics(DiagnosticInput {
            extension: Some(ExtensionReport::NotInstalled),
            ..running_daemon(None)
        });
        let text = report.copy_text();
        let columns = report.continuation_columns();
        assert_eq!(text.lines().count(), columns.len());
        for (line, &column) in text.lines().zip(columns) {
            if field_value(line, "Last error").is_some() {
                assert_eq!(
                    line.len() - field_value(line, "Last error").unwrap().len(),
                    column
                );
            } else if line.starts_with("  ") && !line.starts_with("    ") {
                assert_eq!(column, 2, "{line:?}");
            } else if !line.starts_with(' ') {
                assert_eq!(column, 0, "{line:?}");
            }
        }
    }

    /// Labels of every length line their values up in one column, so a long
    /// label never runs into its value.
    #[test]
    fn field_values_share_one_column() {
        let text = present_diagnostics(DiagnosticInput {
            extension: Some(ExtensionReport::NotInstalled),
            ..running_daemon(None)
        })
        .copy_text();
        let columns: Vec<usize> = ["Version", "Last error", "Shell extension"]
            .iter()
            .map(|label| {
                let line = text
                    .lines()
                    .find(|line| line.trim_start().starts_with(label))
                    .unwrap_or_else(|| panic!("no {label} line in {text}"));
                line.len() - field_value(line, label).expect("value").len()
            })
            .collect();
        assert!(columns.windows(2).all(|w| w[0] == w[1]), "{text}");
        assert!(text.contains("Shell extension not installed"), "{text}");
        assert!(text.contains("Last error      none"), "{text}");
    }

    /// The path is the diagnosis: a permission denied on sysfs means a plug
    /// is not connected.
    #[test]
    fn system_paths_are_kept_and_the_home_becomes_a_tilde() {
        let scrub = |value| scrub_home(value, "/home/alice/");
        assert_eq!(
            scrub("open /sys/bus/usb/devices: permission denied"),
            "open /sys/bus/usb/devices: permission denied"
        );
        assert_eq!(
            scrub("failed at /home/alice/Private Recording.wav"),
            "failed at ~/Private Recording.wav"
        );
        assert_eq!(scrub("cd /home/alice"), "cd ~");
        assert_eq!(
            scrub("/media/alice/usb, /home/alicex, /srv/home/alice/x"),
            "/media/alice/usb, /home/alicex, /srv/home/alice/x"
        );
        assert_eq!(scrub("échec sans chemin"), "échec sans chemin");
    }

    #[test]
    fn a_root_home_changes_nothing() {
        assert_eq!(scrub_home("/etc/hosts", "/"), "/etc/hosts");
    }

    #[test]
    fn sanitize_scrubs_secrets_and_keeps_system_paths() {
        let text = redact_text("failed at /run/myna.sock --token=abc123 rest");
        assert_eq!(text, "failed at /run/myna.sock --token=[redacted] rest");
    }

    #[test]
    fn an_error_that_mentions_dictation_or_audio_is_kept() {
        for error in [
            "dictation failed: x",
            "audio device busy",
            "text: input rejected",
            "prompt too long",
        ] {
            assert_eq!(redact_text(error), error);
        }
    }

    #[test]
    fn a_secret_assignment_is_still_scrubbed() {
        assert_eq!(
            redact_text("dictation failed: token=abc123 password=hunter2 rest"),
            "dictation failed: token=[redacted] password=[redacted] rest"
        );
    }

    #[test]
    fn snap_list_parses_and_reports() {
        let snaps = parse_snap_list(
            "Name  Version  Rev  Tracking  Publisher  Notes\n\
             myna  1.2.3  7  latest/stable  canonical**  -\n",
        )
        .unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].name, "myna");
        assert_eq!(snaps[0].version, "1.2.3");
    }

    #[test]
    fn empty_state_still_reports_versions_and_onboarding_state() {
        let report = present_diagnostics(DiagnosticInput {
            inventory_complete: true,
            ..DiagnosticInput::default()
        });
        let text = report.copy_text();
        assert!(text.contains("Myna Settings "));
        assert!(text.contains("Onboarding: Dictation is not installed"));
        assert!(!text.contains("snap install"));
        assert!(text.contains("Models:\n  (none discovered)"));
        assert!(text.contains("Problems:\n  (none)"));
    }

    /// Installed is not usable: with every model disconnected, dictation
    /// fails, so the report must not read Ready with no problems.
    #[test]
    fn a_disconnected_model_is_not_ready() {
        let input = |connection| DiagnosticInput {
            inventory_complete: true,
            installed_snaps: vec![InstalledSnap {
                name: "myna".into(),
                version: "0.1.0".into(),
            }],
            backends: vec![BackendDiagnostic {
                snap_name: "myna-parakeet".into(),
                connection,
                ..BackendDiagnostic::default()
            }],
            ..DiagnosticInput::default()
        };
        let report = present_diagnostics(input(DiagnosticConnection::NotConnected));
        assert_eq!(report.onboarding(), OnboardingState::NoModelConnected);
        let text = report.copy_text();
        assert!(text.contains("Onboarding: No model connected"), "{text}");
        assert!(
            text.contains("Problems:\n  No model is connected. Choose one to use for dictation."),
            "{text}"
        );

        let report = present_diagnostics(input(DiagnosticConnection::Connected));
        assert_eq!(report.onboarding(), OnboardingState::Ready);
        assert!(report.copy_text().contains("Problems:\n  (none)"));
    }

    #[test]
    fn a_problem_is_one_sentence_and_blocks_the_ready_state() {
        let report = present_diagnostics(DiagnosticInput {
            problems: vec!["Installed snaps: permission denied".into()],
            ..DiagnosticInput::default()
        });
        let text = report.copy_text();
        assert!(text.contains("Problems:\n  Installed snaps: permission denied"));
        assert_eq!(report.onboarding(), OnboardingState::Unavailable);
    }

    #[test]
    fn an_xfce_session_reports_its_input_method_and_indicator_host() {
        use crate::platform::ComponentFact;
        let report = present_diagnostics(DiagnosticInput {
            desktop: vec![
                ComponentFact {
                    purpose: Purpose::TextInput,
                    status: ComponentStatus::Unavailable,
                },
                ComponentFact {
                    purpose: Purpose::StatusSurface,
                    status: ComponentStatus::Active,
                },
            ],
            ..DiagnosticInput::default()
        });
        let text = report.copy_text();
        assert_eq!(field_value(&text, "Input method"), Some("not installed"));
        assert_eq!(field_value(&text, "Indicator host"), Some("active"));
        assert_eq!(field_value(&text, "Shell extension"), None);
    }

    #[test]
    fn the_report_names_the_machine_and_what_each_process_costs() {
        let report = present_diagnostics(DiagnosticInput {
            inventory_complete: true,
            installed_snaps: vec![InstalledSnap {
                name: "myna".into(),
                version: "0.1.0".into(),
            }],
            machine: Some(MachineFacts {
                cpu: "amd64 AuthenticAMD, avx2".into(),
                memory: "30.1 GiB RAM, 8.0 GiB swap".into(),
                gpus: vec!["AMD 0x1114 gfx1152 512.0 MiB VRAM".into()],
            }),
            daemon: Some(ProcessMemory {
                pid: 42,
                resident: 16 * 1024 * 1024,
                peak: 18 * 1024 * 1024,
            }),
            daemon_report: Some(DaemonReport {
                drops: Some(AudioDrops { not_active: 3 }),
                last_error: None,
            }),
            extension: Some(ExtensionReport::Known {
                state: "active".into(),
                copy: ExtensionCopy::MynaConfigPackage,
                error: None,
            }),
            desktop: Vec::new(),
            performance: None,
            backends: vec![BackendDiagnostic {
                snap_name: "myna-parakeet".into(),
                version: "0.1.0".into(),
                connection: DiagnosticConnection::Connected,
                engine: Some("cpu".into()),
                model: Some("parakeet-tdt-0.6b-v3".into()),
                services: vec!["server: Active".into()],
                memory: Some(ProcessMemory {
                    pid: 43,
                    resident: 20 * 1024 * 1024,
                    peak: 1500 * 1024 * 1024,
                }),
                problems: Vec::new(),
            }],
            problems: Vec::new(),
        });
        let text = report.copy_text();

        assert!(text.contains("avx2"), "{text}");
        assert!(text.contains("gfx1152"), "{text}");
        // The peak is the point: idle residency says nothing about whether
        // this machine can hold the model.
        assert!(
            text.contains("20.0 MiB now, 1.5 GiB peak (pid 43)"),
            "{text}"
        );
        assert!(text.contains("parakeet-tdt-0.6b-v3"), "{text}");
        assert!(text.contains("3 chunks dropped this session"), "{text}");
        assert_eq!(field_value(&text, "Last error"), Some("none"), "{text}");
        assert_eq!(
            field_value(&text, "Shell extension"),
            Some("active, myna-config package"),
            "{text}"
        );
        assert!(text.contains("Problems:\n  (none)"), "{text}");
        // Nothing in the report came from outside this crate.
        assert!(!text.contains('/'), "{text}");
    }

    fn running_daemon(last_error: Option<LastError>) -> DiagnosticInput {
        DiagnosticInput {
            installed_snaps: vec![InstalledSnap {
                name: "myna".into(),
                version: "0.1.0".into(),
            }],
            daemon: Some(ProcessMemory {
                pid: 42,
                resident: 1024,
                peak: 1024,
            }),
            daemon_report: Some(DaemonReport {
                drops: None,
                last_error,
            }),
            ..DiagnosticInput::default()
        }
    }

    /// The page and the exported report are the same text, and the detail is
    /// the one value in it that came from outside: it is shown whole.
    #[test]
    fn the_last_error_is_shown_with_its_detail() {
        let report = present_diagnostics(running_daemon(Some(LastError {
            headline: "Model not connected".into(),
            detail: "/nonexistent/share: no model snap is connected; \
                     cannot reach the model: /run/user/1000/snap.myna/backend/provider/myna.sock"
                .into(),
            at: "2026-10-01 09:30:00".into(),
        })));
        let text = report.copy_text();
        assert_eq!(
            field_value(&text, "Last error"),
            Some("Model not connected (/nonexistent/share: no model snap is connected; cannot reach the model: /run/user/1000/snap.myna/backend/provider/myna.sock), 2026-10-01 09:30:00"),
            "{text}"
        );
    }

    #[test]
    fn a_failed_extension_shows_gnome_shells_error_without_the_home() {
        let report = present_diagnostics(DiagnosticInput {
            extension: Some(ExtensionReport::Known {
                state: "error".into(),
                copy: ExtensionCopy::UserCopy,
                error: Some(format!(
                    "SyntaxError at {}/x/extension.js: bad",
                    gio::glib::home_dir().display()
                )),
            }),
            ..running_daemon(None)
        });
        let text = report.copy_text();
        assert_eq!(
            field_value(&text, "Shell extension"),
            Some("error, user copy (SyntaxError at ~/x/extension.js: bad)"),
            "{text}"
        );
        for (extension, line) in [
            (ExtensionReport::NoShell, "gnome-shell did not answer"),
            (ExtensionReport::NotInstalled, "not installed"),
            (
                ExtensionReport::SinceLogin {
                    at_next_login: true,
                },
                "installed since login, starts at the next one",
            ),
            (
                ExtensionReport::SinceLogin {
                    at_next_login: false,
                },
                "installed since login, not enabled",
            ),
        ] {
            let text = present_diagnostics(DiagnosticInput {
                extension: Some(extension),
                ..running_daemon(None)
            })
            .copy_text();
            assert_eq!(field_value(&text, "Shell extension"), Some(line), "{text}");
        }
    }

    /// An older daemon publishes no last error; that reads as "none", never
    /// as a problem.
    #[test]
    fn no_last_error_reads_as_none() {
        let report = present_diagnostics(running_daemon(None));
        assert_eq!(field_value(&report.copy_text(), "Last error"), Some("none"));
        let report = present_diagnostics(DiagnosticInput {
            daemon_report: None,
            ..running_daemon(None)
        });
        assert_eq!(field_value(&report.copy_text(), "Last error"), Some("none"));
        assert!(report.copy_text().contains("Problems:\n  (none)"));
    }
}
