//! Native configuration application for Myna.

pub mod active_backend;
pub mod adapters;
pub mod app;
pub mod apply_plan;
pub mod backend_apply;
pub mod backend_controller;
pub mod backend_ui;
pub mod command;
pub mod diagnostics;
pub mod domain;
pub mod machine;
pub mod markup;
pub mod myna_settings;
pub mod onboarding;
pub mod onboarding_ui;
pub mod operation_gate;
pub mod performance;
pub mod ports;
pub mod presentation;
pub mod shortcut;
pub mod shortcut_ui;
pub mod ui;

pub const APP_ID: &str = "com.canonical.Myna.Config";
pub const GETTEXT_DOMAIN: &str = "myna-config";
pub const USAGE: &str = "\
Usage: myna-config [OPTION]

Native configuration application for Myna.

  --version      print the version and exit
  -h, --help     print this help and exit";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Launch,
    PrintHelp,
    PrintVersion,
    /// Run a serialized apply plan as root. Not advertised in `USAGE`: the
    /// UI invokes it through `pkexec` and nothing else should.
    ApplyPlan(String),
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut command = Command::Launch;
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        let next = match argument.as_str() {
            "--help" | "-h" => Command::PrintHelp,
            "--version" => Command::PrintVersion,
            apply_plan::APPLY_PLAN_FLAG => match args.next() {
                Some(plan) => Command::ApplyPlan(plan),
                None => {
                    return Err(format!(
                        "myna-config: {} requires a plan argument",
                        apply_plan::APPLY_PLAN_FLAG
                    ))
                }
            },
            other => return Err(format!("myna-config: unknown option {other}\n\n{USAGE}")),
        };
        if command != Command::Launch {
            return Err(format!(
                "myna-config: only one option may be specified\n\n{USAGE}"
            ));
        }
        command = next;
    }

    Ok(command)
}

pub fn init_i18n() {
    let mut domain = gettextrs::TextDomain::new(GETTEXT_DOMAIN);
    if let Ok(directory) = std::env::var("MYNA_CONFIG_LOCALEDIR") {
        if !directory.is_empty() {
            domain = domain.push(directory);
        }
    }
    let _ = domain.init();
}
