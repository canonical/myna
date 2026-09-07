//! Pure presenter for the About/Diagnostics page and the associated refresh
//! policy.
//!
//! GTK-independent so it can be exercised headlessly. The report is built from
//! facts this crate produces itself - machine shape, process memory, engine and
//! model names, versions - and never embeds a command line, stderr, or any
//! other text that came from outside. That is what keeps the privacy contract
//! (no audio, no transcript, no filesystem path) true by construction rather
//! than by scrubbing.

use std::time::Duration;

use crate::machine::{bytes, AudioDrops, MachineFacts, ProcessMemory};

/// Copy-safe onboarding instructions surfaced when Myna itself is missing.
pub const NO_MYNA_COMMAND: &str = "sudo snap install myna";
/// Copy-safe onboarding instructions surfaced when Myna is installed but no
/// backend has been chosen yet.
pub const NO_BACKEND_COMMAND: &str = "sudo snap install myna-parakeet";

/// Upper bound on subprocess spawns required for a single application-level
/// refresh (currently `snap list` and `snap connections`).
pub const APP_REFRESH_PROCESS_BUDGET: usize = 2;
/// Upper bound on subprocess spawns required to refresh a single backend
/// snapshot: `snap info`, at most four prioritized modelctl candidate probes,
/// and the four modelctl data commands (status, get, list-models,
/// list-engines).
pub const BACKEND_REFRESH_PROCESS_BUDGET: usize = 9;

const PLACEHOLDER_PATH: &str = "<path>";
const PLACEHOLDER_REDACTED: &str = "[redacted]";

/// Keys that indicate freeform user content. Any field containing one of
/// these assignments is replaced in full.
const SENSITIVE_LINE_KEYS: &[&str] = &[
    "audio",
    "transcript",
    "phrase",
    "phrases",
    "text",
    "content",
    "prompt",
    "dictation",
    "utterance",
];

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
    pub drops: Option<AudioDrops>,
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
    /// Nothing to onboard; discovery has at least one backend.
    Ready,
    /// Installation state could not be determined.
    Unavailable,
}

/// Output of the diagnostics presenter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticReport {
    onboarding: OnboardingState,
    body: String,
}

impl DiagnosticReport {
    pub fn onboarding(&self) -> OnboardingState {
        self.onboarding
    }

    pub fn onboarding_command(&self) -> Option<&'static str> {
        match self.onboarding {
            OnboardingState::NoMyna => Some(NO_MYNA_COMMAND),
            OnboardingState::NoBackend => Some(NO_BACKEND_COMMAND),
            OnboardingState::Ready | OnboardingState::Unavailable => None,
        }
    }

    pub fn copy_text(&self) -> String {
        self.body.clone()
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

/// The dominant reason to trigger a refresh cycle. Each variant maps to a
/// documented process budget so the UI can never regress into perpetual
/// broad polling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshReason {
    /// The application is idle (no user interaction, no focus, no visible
    /// diagnostics page). No processes must be spawned.
    Idle,
    /// The application is starting up and needs an initial inventory refresh.
    Startup,
    /// The user selected a specific backend page.
    BackendSelected,
    /// The user opened or explicitly refreshed the diagnostics page.
    DiagnosticsRequested,
}

/// A planned refresh work item and its associated process budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPlan {
    processes: usize,
}

impl RefreshPlan {
    pub const fn processes(self) -> usize {
        self.processes
    }
}

/// Pure refresh policy. Deliberately does not schedule background polling —
/// refreshes must be event-driven (startup, sidebar selection, explicit
/// user request) so that idle sessions produce zero subprocess churn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    debounce: Duration,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(250),
        }
    }
}

impl RefreshPolicy {
    /// Interval between periodic refreshes. Always `None` — periodic broad
    /// polling is explicitly forbidden.
    pub fn periodic_interval(&self) -> Option<Duration> {
        None
    }

    /// Debounce window for coalescing user-initiated refresh requests.
    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// Plans the number of subprocesses a single refresh cycle may spawn.
    pub fn plan(&self, reason: RefreshReason, visible_backends: usize) -> RefreshPlan {
        let processes = match reason {
            RefreshReason::Idle => 0,
            RefreshReason::Startup => APP_REFRESH_PROCESS_BUDGET,
            RefreshReason::BackendSelected => {
                BACKEND_REFRESH_PROCESS_BUDGET * visible_backends.min(1)
            }
            RefreshReason::DiagnosticsRequested => {
                APP_REFRESH_PROCESS_BUDGET + visible_backends * BACKEND_REFRESH_PROCESS_BUDGET
            }
        };
        RefreshPlan { processes }
    }
}

/// Produce the diagnostics report for the given input.
pub fn present_diagnostics(input: DiagnosticInput) -> DiagnosticReport {
    let onboarding = classify_onboarding(&input);
    let body = render_body(&input, onboarding);
    DiagnosticReport { onboarding, body }
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
    OnboardingState::Ready
}

/// Localized, user-facing label for an onboarding state.
pub fn onboarding_state_label(state: OnboardingState) -> String {
    match state {
        OnboardingState::NoMyna => gettextrs::gettext("Myna is not installed"),
        OnboardingState::NoBackend => gettextrs::gettext("No backend discovered"),
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

fn render_body(input: &DiagnosticInput, onboarding: OnboardingState) -> String {
    let mut out = String::new();
    out.push_str(&gettextrs::gettext("Myna Settings"));
    out.push(' ');
    out.push_str(env!("CARGO_PKG_VERSION"));
    out.push('\n');
    out.push_str(&gettextrs::gettext("Onboarding"));
    out.push_str(": ");
    out.push_str(&onboarding_state_label(onboarding));
    out.push('\n');
    if let Some(command) = onboarding_command(onboarding) {
        out.push_str(&gettextrs::gettext("Suggested command"));
        out.push_str(": ");
        out.push_str(command);
        out.push('\n');
    }

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
            &gettextrs::gettext("(no backend answered show-machine)"),
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
            if let Some(drops) = input.drops {
                field(
                    &mut out,
                    &gettextrs::gettext("Audio"),
                    &drops_summary(drops),
                );
            }
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

    out.push('\n');
    out.push_str(&gettextrs::gettext("Backends"));
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
    let problems: Vec<&String> = input
        .problems
        .iter()
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

    out
}

fn field(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("    {label:<10} {value}\n"));
}

/// Always printed when the daemon is up: a confirmed zero is the fact worth
/// having during a bug hunt.
fn drops_summary(drops: AudioDrops) -> String {
    let total = drops.not_resident + drops.not_active;
    format!(
        "{} {} ({} {})",
        total,
        gettextrs::gettext("chunks dropped this session"),
        drops.not_resident,
        gettextrs::gettext("before the model was ready")
    )
}

fn memory_summary(memory: ProcessMemory) -> String {
    format!(
        "{} now, {} peak (pid {})",
        bytes(memory.resident),
        bytes(memory.peak),
        memory.pid
    )
}

fn onboarding_command(state: OnboardingState) -> Option<&'static str> {
    match state {
        OnboardingState::NoMyna => Some(NO_MYNA_COMMAND),
        OnboardingState::NoBackend => Some(NO_BACKEND_COMMAND),
        OnboardingState::Ready | OnboardingState::Unavailable => None,
    }
}

/// Redact a single-line value: strip filesystem paths and any embedded
/// `key=value` pairs whose key looks sensitive.
pub fn redact_text(value: &str) -> String {
    if contains_sensitive_content(value) {
        return PLACEHOLDER_REDACTED.to_owned();
    }
    let scrubbed = scrub_secret_assignments(value);
    scrub_paths(&scrubbed)
}

fn contains_sensitive_content(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    SENSITIVE_LINE_KEYS.iter().any(|key| {
        lower.match_indices(key).any(|(index, _)| {
            let before = lower[..index].chars().next_back();
            let after = lower[index + key.len()..].chars().next();
            !before.is_some_and(|character| character.is_ascii_alphanumeric())
                && matches!(after, Some('=' | ':' | ' '))
        })
    })
}

fn scrub_paths(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find('/') {
        let start = cursor + relative_start;
        out.push_str(&value[cursor..start]);
        let after_slash = &value[start + 1..];
        if after_slash.chars().next().map_or(true, char::is_whitespace) {
            out.push('/');
            cursor = start + 1;
            continue;
        }
        let end = after_slash
            .char_indices()
            .find(|(_, character)| matches!(character, '\n' | '\r' | ',' | ';' | '"' | '\''))
            .map_or(value.len(), |(offset, _)| start + 1 + offset);
        out.push_str(PLACEHOLDER_PATH);
        cursor = end;
    }
    out.push_str(&value[cursor..]);
    out
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

    #[test]
    fn sanitize_replaces_paths_and_secrets() {
        let text = redact_text("failed at /home/alice/private --token=abc123 rest");
        assert!(text.contains("<path>"));
        assert!(!text.contains("abc123"));
        assert!(!text.contains("/home/alice"));
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
    fn refresh_policy_matches_documented_budgets() {
        let policy = RefreshPolicy::default();
        assert!(policy.periodic_interval().is_none());
        assert_eq!(policy.plan(RefreshReason::Idle, 4).processes(), 0);
        assert_eq!(
            policy.plan(RefreshReason::Startup, 4).processes(),
            APP_REFRESH_PROCESS_BUDGET
        );
        assert_eq!(
            policy.plan(RefreshReason::BackendSelected, 3).processes(),
            BACKEND_REFRESH_PROCESS_BUDGET
        );
        assert_eq!(
            policy
                .plan(RefreshReason::DiagnosticsRequested, 3)
                .processes(),
            APP_REFRESH_PROCESS_BUDGET + 3 * BACKEND_REFRESH_PROCESS_BUDGET
        );
    }

    #[test]
    fn scrub_paths_leaves_stand_alone_slash() {
        assert_eq!(scrub_paths("a / b"), "a / b");
        assert_eq!(scrub_paths("/etc/hosts"), "<path>");
        assert_eq!(
            scrub_paths("failed at /home/alice/Private Recording.wav"),
            "failed at <path>"
        );
        assert_eq!(scrub_paths("échec sans chemin"), "échec sans chemin");
    }

    #[test]
    fn empty_state_still_reports_versions_and_onboarding_hint() {
        let report = present_diagnostics(DiagnosticInput {
            inventory_complete: true,
            ..DiagnosticInput::default()
        });
        let text = report.copy_text();
        assert!(text.contains("Myna Settings "));
        assert!(text.contains("Onboarding: Myna is not installed"));
        assert!(text.contains("Suggested command: sudo snap install myna"));
        assert!(text.contains("Backends:\n  (none discovered)"));
        assert!(text.contains("Problems:\n  (none)"));
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
            drops: Some(AudioDrops {
                not_resident: 3,
                not_active: 0,
            }),
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
        assert!(
            text.contains("3 chunks dropped this session (3 before the model was ready)"),
            "{text}"
        );
        assert!(text.contains("Problems:\n  (none)"), "{text}");
        // Nothing in the report came from outside this crate.
        assert!(!text.contains('/'), "{text}");
    }
}
