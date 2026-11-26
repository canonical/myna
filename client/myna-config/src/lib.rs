//! Native configuration application for Myna.

/// The GLib log domain: `G_MESSAGES_DEBUG=myna-config` shows its debug lines.
pub const LOG_DOMAIN: &str = "myna-config";

pub mod active_backend;
pub mod adapters;
pub mod app;
pub mod apply_plan;
pub mod backend_apply;
pub mod backend_controller;
pub mod backend_ui;
pub mod chain_notice;
pub mod chains_view;
pub mod command;
pub mod diagnostics;
pub mod domain;
pub mod hud_host;
pub mod installed_models;
pub mod languages;
pub mod list_editor_ui;
pub mod machine;
pub mod markup;
pub mod model_family;
pub mod models_ui;
pub mod myna_settings;
pub mod onboarding;
pub mod onboarding_ui;
pub mod operation_gate;
pub mod past_dictations_ui;
pub mod performance;
pub mod platform;
pub mod plugin_catalog;
pub mod plugin_install;
pub mod ports;
pub mod presentation;
pub mod processing_catalog_ui;
pub mod processing_step_ui;
pub mod processing_steps;
pub mod processing_ui;
pub mod shortcut;
pub mod shortcut_ui;
pub mod snap_changes;
pub mod snap_install;
pub mod sound_preview;
pub mod step_catalog;
pub mod subpage;
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
    /// Run onboarding's serialized set-up plan as root, the same way.
    SetUp(String),
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut command = Command::Launch;
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        let next = match argument.as_str() {
            "--help" | "-h" => Command::PrintHelp,
            "--version" => Command::PrintVersion,
            flag @ (apply_plan::APPLY_PLAN_FLAG | apply_plan::SET_UP_FLAG) => {
                let Some(plan) = args.next() else {
                    return Err(format!("myna-config: {flag} requires a plan argument"));
                };
                if flag == apply_plan::SET_UP_FLAG {
                    Command::SetUp(plan)
                } else {
                    Command::ApplyPlan(plan)
                }
            }
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
    // A gettext localedir, as bindtextdomain takes it:
    // `<dir>/<lang>/LC_MESSAGES/myna-config.mo`. TextDomain::push wants the
    // directory above `locale/` instead, which the name does not say.
    if let Some(directory) = std::env::var_os("MYNA_CONFIG_LOCALEDIR").filter(|d| !d.is_empty()) {
        gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
        let _ = gettextrs::bindtextdomain(GETTEXT_DOMAIN, std::path::PathBuf::from(directory));
        let _ = gettextrs::bind_textdomain_codeset(GETTEXT_DOMAIN, "UTF-8");
        let _ = gettextrs::textdomain(GETTEXT_DOMAIN);
        return;
    }
    let _ = gettextrs::TextDomain::new(GETTEXT_DOMAIN).init();
}
