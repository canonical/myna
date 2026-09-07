//! Pure presenter for the About/Diagnostics page and the associated refresh
//! policy.
//!
//! Everything in this module is deliberately GTK-independent so it can be
//! exercised from unit tests and reused headlessly. The presenter enforces the
//! privacy contract for diagnostics: no audio, transcript, or freeform user
//! content is ever surfaced, filesystem paths are stripped in favour of a
//! `<path>` placeholder, and command-line arguments known to carry secrets are
//! replaced with `[redacted]`.

use std::time::Duration;

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

/// A command failure recorded during discovery or diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticFailure {
    pub surface: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub message: String,
    pub stderr: String,
}

/// Per-backend diagnostic summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackendDiagnostic {
    pub snap_name: String,
    pub modelctl_app: Option<String>,
    pub connection: DiagnosticConnection,
    pub failures: Vec<DiagnosticFailure>,
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
    pub backends: Vec<BackendDiagnostic>,
    pub inventory_failure: Option<DiagnosticFailure>,
    pub failures: Vec<DiagnosticFailure>,
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
    if !input.inventory_complete || input.inventory_failure.is_some() || !input.failures.is_empty()
    {
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
    out.push_str(&gettextrs::gettext("Installed Myna snaps"));
    out.push_str(":\n");
    let relevant_snaps = input.installed_snaps.iter().filter(|snap| {
        snap.name == "myna"
            || input
                .backends
                .iter()
                .any(|backend| backend.snap_name == snap.name)
    });
    let mut reported_snap = false;
    for snap in relevant_snaps {
        reported_snap = true;
        out.push_str("  ");
        out.push_str(&snap.name);
        out.push(' ');
        out.push_str(&snap.version);
        out.push('\n');
    }
    if !reported_snap {
        out.push_str("  ");
        out.push_str(&gettextrs::gettext("(none reported)"));
        out.push('\n');
    }

    if let Some(failure) = &input.inventory_failure {
        out.push('\n');
        out.push_str(&gettextrs::gettext("Snap inventory failure"));
        out.push_str(":\n");
        push_failure(&mut out, failure);
    }
    if !input.failures.is_empty() {
        out.push('\n');
        out.push_str(&gettextrs::gettext("Command failures"));
        out.push_str(":\n");
        for failure in &input.failures {
            push_failure(&mut out, failure);
        }
    }

    out.push('\n');
    out.push_str(&gettextrs::gettext("Backends"));
    out.push_str(":\n");
    if input.backends.is_empty() {
        out.push_str("  ");
        out.push_str(&gettextrs::gettext("(none discovered)"));
        out.push('\n');
    } else {
        for backend in &input.backends {
            out.push_str("  ");
            out.push_str(&backend.snap_name);
            out.push('\n');
            out.push_str("    ");
            out.push_str(&gettextrs::gettext("Model control command"));
            out.push_str(": ");
            let modelctl_app = backend
                .modelctl_app
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| gettextrs::gettext("(unresolved)"));
            out.push_str(&modelctl_app);
            out.push('\n');
            out.push_str("    ");
            out.push_str(&gettextrs::gettext("Connection"));
            out.push_str(": ");
            out.push_str(&diagnostic_connection_label(backend.connection));
            out.push('\n');
            for failure in &backend.failures {
                out.push_str("    ");
                out.push_str(&gettextrs::gettext("Failure"));
                out.push_str(":\n");
                push_failure_indented(&mut out, failure, "      ");
            }
        }
    }

    out
}

fn onboarding_command(state: OnboardingState) -> Option<&'static str> {
    match state {
        OnboardingState::NoMyna => Some(NO_MYNA_COMMAND),
        OnboardingState::NoBackend => Some(NO_BACKEND_COMMAND),
        OnboardingState::Ready => None,
        OnboardingState::Unavailable => None,
    }
}

fn push_failure(out: &mut String, failure: &DiagnosticFailure) {
    push_failure_indented(out, failure, "  ");
}

fn push_failure_indented(out: &mut String, failure: &DiagnosticFailure, indent: &str) {
    out.push_str(indent);
    out.push_str(&gettextrs::gettext("Surface"));
    out.push_str(": ");
    out.push_str(&redact_text(&failure.surface));
    out.push('\n');
    out.push_str(indent);
    out.push_str(&gettextrs::gettext("Command"));
    out.push_str(": ");
    out.push_str(&redact_text(&failure.executable));
    for arg in sanitize_arguments(&failure.arguments) {
        out.push(' ');
        out.push_str(&arg);
    }
    out.push('\n');
    out.push_str(indent);
    out.push_str(&gettextrs::gettext("Message"));
    out.push_str(": ");
    out.push_str(&redact_text(&failure.message));
    out.push('\n');
    if !failure.stderr.is_empty() {
        out.push_str(indent);
        out.push_str(&gettextrs::gettext("Error output"));
        out.push_str(": ");
        out.push_str(&gettextrs::gettext(
            "omitted because command output may contain private content",
        ));
        out.push('\n');
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

/// Redact a single command-line argument, treating the entire argument as a
/// path if it starts with `/` and otherwise recursing into the standard
/// single-line rules.
fn sanitize_argument(argument: &str) -> String {
    if argument.starts_with('/') {
        return PLACEHOLDER_PATH.to_owned();
    }
    if let Some(eq) = argument.find('=') {
        let key = &argument[..eq];
        let key_lower = key.trim_start_matches('-').to_ascii_lowercase();
        if SENSITIVE_KEY_SUBSTRINGS
            .iter()
            .any(|needle| key_lower.contains(needle))
        {
            return format!("{key}={PLACEHOLDER_REDACTED}");
        }
    }
    redact_text(argument)
}

fn sanitize_arguments(arguments: &[String]) -> Vec<String> {
    let mut sanitized = Vec::with_capacity(arguments.len() + 1);
    let mut redact_next = false;

    for argument in arguments {
        if redact_next {
            if is_split_secret_flag(argument) {
                sanitized.push(PLACEHOLDER_REDACTED.to_owned());
                sanitized.push(sanitize_argument(argument));
                continue;
            }
            sanitized.push(PLACEHOLDER_REDACTED.to_owned());
            redact_next = false;
            continue;
        }

        sanitized.push(sanitize_argument(argument));
        if is_split_secret_flag(argument) {
            redact_next = true;
        }
    }

    if redact_next {
        sanitized.push(PLACEHOLDER_REDACTED.to_owned());
    }
    sanitized
}

fn is_split_secret_flag(argument: &str) -> bool {
    if argument.contains('=') {
        return false;
    }
    let key = argument.trim_start_matches('-').to_ascii_lowercase();
    argument.starts_with('-')
        && SENSITIVE_KEY_SUBSTRINGS
            .iter()
            .any(|needle| key.contains(needle))
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

/// Format a command failure for user-visible diagnostics after applying the
/// same privacy filter used by the copyable report.
pub fn format_failure(failure: &DiagnosticFailure) -> String {
    let mut output = String::new();
    push_failure(&mut output, failure);
    output.trim().to_owned()
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
    fn sanitize_argument_replaces_absolute_paths() {
        assert_eq!(sanitize_argument("/etc/hosts"), "<path>");
        assert_eq!(sanitize_argument("--flag=/etc/hosts"), "--flag=<path>");
        assert_eq!(sanitize_argument("list"), "list");
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
        assert!(text.contains("Installed Myna snaps:\n  (none reported)"));
        assert!(text.contains("Backends:\n  (none discovered)"));
    }

    #[test]
    fn inventory_failure_without_backends_surfaces_command_and_message() {
        let report = present_diagnostics(DiagnosticInput {
            inventory_failure: Some(DiagnosticFailure {
                surface: "inventory".into(),
                executable: "snap".into(),
                arguments: vec!["list".into()],
                message: "permission denied".into(),
                stderr: "cannot connect to snapd".into(),
            }),
            ..DiagnosticInput::default()
        });
        let text = report.copy_text();
        assert!(text.contains("Snap inventory failure"));
        assert!(text.contains("snap list"));
        assert!(text.contains("permission denied"));
        assert!(text.contains("omitted because command output may contain private content"));
        // Even under failure, the onboarding hint is still surfaced so the
        // user knows what to do.
        assert_eq!(report.onboarding(), OnboardingState::Unavailable);
    }
}
