// Oldest release measured to compile data/*.blp to output identical to 0.20.4.
pub const MINIMUM: (u32, u32, u32) = (0, 16, 0);

pub fn check(version_output: &str) -> Result<(), String> {
    let found = version_output.trim();
    let (major, minor, patch) = MINIMUM;
    let install = format!("install blueprint-compiler {major}.{minor}.{patch} or newer");
    match parse(found) {
        Some(version) if version >= MINIMUM => Ok(()),
        Some(_) => Err(format!("blueprint-compiler {found} is too old; {install}")),
        None => Err(format!(
            "blueprint-compiler --version printed {found:?}, not a release version; {install}"
        )),
    }
}

fn parse(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    let parsed = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(parsed)
}
