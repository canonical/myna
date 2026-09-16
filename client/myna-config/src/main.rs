use gtk::glib;
use gtk4 as gtk;

use myna_config::{parse_args, Command, USAGE};

fn main() -> glib::ExitCode {
    let mut raw_args = std::env::args();
    let _program = raw_args.next();
    let args: Vec<String> = raw_args.collect();

    myna_config::init_i18n();

    match parse_args(args) {
        Ok(Command::Launch) => myna_config::app::run(),
        Ok(Command::PrintHelp) => {
            println!("{USAGE}");
            glib::ExitCode::SUCCESS
        }
        Ok(Command::PrintVersion) => {
            println!("myna-config {}", env!("MYNA_VERSION"));
            glib::ExitCode::SUCCESS
        }
        Ok(Command::ApplyPlan(plan)) => {
            glib::ExitCode::from(myna_config::apply_plan::run_plan(&plan).code() as u8)
        }
        Err(message) => {
            eprintln!("{message}");
            glib::ExitCode::FAILURE
        }
    }
}
