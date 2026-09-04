use std::io::{self, Write};
use std::time::Duration;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--fail") => {
            println!("failure stdout");
            eprintln!("failure stderr");
            std::process::exit(23);
        }
        Some("--sleep") => std::thread::sleep(Duration::from_secs(10)),
        Some("--invalid-stdout") => {
            io::stdout().write_all(&[0xff]).unwrap();
        }
        _ => {
            println!(
                "{}",
                std::env::var("COMMAND_RUNNER_TEST").unwrap_or_default()
            );
            for argument in arguments {
                println!("{argument}");
            }
        }
    }
}
