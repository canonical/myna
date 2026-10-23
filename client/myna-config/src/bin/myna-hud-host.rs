//! Runs the HUD while the dictation daemon is up; see `hud_host`.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(myna_config::hud_host::run::run() as u8)
}
