//! Host facts for the diagnostics page: what this machine is, and what the
//! running Myna processes cost.
//!
//! Everything here is a `/proc` or `/sys` read. Both are world-readable and
//! need no cooperation from the process being measured, and the native UI is
//! host-only (`snap_packaging.rs`), so nothing here is confined. No
//! subprocess, and it still answers on a machine with no backend installed -
//! which is exactly when someone is looking at this page.

use std::path::Path;

/// The CPU flags worth naming: the ones an engine is selected on.
const NOTABLE_FLAGS: &[&str] = &["avx", "avx2", "avx512f", "f16c", "fma", "amx_bf16"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineFacts {
    pub cpu: String,
    pub memory: String,
    pub gpus: Vec<String>,
}

pub fn machine_facts() -> MachineFacts {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    MachineFacts {
        cpu: cpu_summary(&cpuinfo),
        memory: memory_summary(&meminfo),
        gpus: gpus(Path::new("/sys/bus/pci/devices")),
    }
}

/// The desktop the user runs Myna on: what every bug report needs before
/// anything else, since injection and activation differ per release and
/// session type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemFacts {
    pub os: String,
    pub kernel: String,
    /// `XDG_CURRENT_DESKTOP` and `XDG_SESSION_TYPE`.
    pub desktop: String,
    /// gnome-shell's own version, when it answers.
    pub shell: Option<String>,
    /// The user's languages, most preferred first, once read.
    pub languages: Vec<String>,
}

/// `shell` and `languages` come from the session bus and the locale reader,
/// which the caller already has.
pub fn system_facts(shell: Option<String>, languages: Vec<String>) -> SystemFacts {
    let os_release = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    SystemFacts {
        os: os_name(&os_release).unwrap_or_else(|| "unknown".into()),
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|kernel| kernel.trim().to_owned())
            .unwrap_or_else(|_| "unknown".into()),
        desktop: desktop_summary(env("XDG_CURRENT_DESKTOP"), env("XDG_SESSION_TYPE")),
        shell,
        languages,
    }
}

fn os_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value);
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn desktop_summary(desktop: Option<String>, session: Option<String>) -> String {
    format!(
        "{}, {}",
        desktop.as_deref().unwrap_or("unknown"),
        session.as_deref().unwrap_or("unknown session")
    )
}

/// Whether an NVIDIA display controller is present, which is what makes an
/// inference snap's install hook pick its GPU engine.
pub fn has_nvidia_gpu() -> bool {
    gpus(Path::new("/sys/bus/pci/devices"))
        .iter()
        .any(|gpu| gpu.starts_with("NVIDIA "))
}

fn cpu_summary(cpuinfo: &str) -> String {
    let field = |name: &str| {
        cpuinfo
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_owned())
    };
    let model = field("model name").unwrap_or_else(|| "unknown".into());
    let threads = cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    let flags = field("flags").unwrap_or_default();
    let notable: Vec<&str> = NOTABLE_FLAGS
        .iter()
        .copied()
        .filter(|flag| flags.split_whitespace().any(|have| have == *flag))
        .collect();
    format!("{model}, {threads} threads, {}", notable.join(" "))
}

fn memory_summary(meminfo: &str) -> String {
    let field = |name: &str| {
        meminfo
            .lines()
            .find_map(|line| {
                line.strip_prefix(name)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
            .map(|kb| kb * 1024)
            .unwrap_or_default()
    };
    format!(
        "{} RAM, {} swap",
        bytes(field("MemTotal:")),
        bytes(field("SwapTotal:"))
    )
}

/// Every PCI display controller (class `0x03…`), with its VRAM where the
/// driver publishes it.
fn gpus(devices: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(devices)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let read = |name: &str| {
                std::fs::read_to_string(entry.path().join(name))
                    .ok()
                    .map(|value| value.trim().to_owned())
            };
            if !read("class")?.starts_with("0x03") {
                return None;
            }
            let mut parts = vec![vendor_name(&read("vendor")?), read("device")?];
            if let Some(vram) = read("mem_info_vram_total").and_then(|v| v.parse::<u64>().ok()) {
                parts.push(format!("{} VRAM", bytes(vram)));
            }
            Some(parts.join(" "))
        })
        .collect();
    found.sort();
    found
}

fn vendor_name(id: &str) -> String {
    match id {
        "0x1002" => "AMD".to_owned(),
        "0x10de" => "NVIDIA".to_owned(),
        "0x8086" => "Intel".to_owned(),
        other => other.to_owned(),
    }
}

/// What one Myna process costs. `peak` is `VmHWM`, which is the number that
/// matters: a backend idling at 20 MiB after an idle-unload still peaked at
/// 1.5 GiB, and only the peak says whether this machine can hold the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessMemory {
    pub pid: u32,
    pub resident: u64,
    pub peak: u64,
}

/// The heaviest live process running out of `/snap/<snap>/`.
///
/// Heaviest, not first: a refresh spawns short-lived `snap run <app> …`
/// children out of the same tree, and the resident server outweighs them by
/// three orders of magnitude.
pub fn snap_process(snap: &str) -> Option<ProcessMemory> {
    let needle = format!("/snap/{snap}/");
    std::fs::read_dir("/proc")
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            String::from_utf8_lossy(&cmdline)
                .contains(&needle)
                .then(|| {
                    process_memory(
                        pid,
                        &std::fs::read_to_string(entry.path().join("status")).ok()?,
                    )
                })
        })
        .flatten()
        .max_by_key(|memory| memory.peak)
}

fn process_memory(pid: u32, status: &str) -> Option<ProcessMemory> {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| {
                line.strip_prefix(name)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
            .map(|kb| kb * 1024)
    };
    Some(ProcessMemory {
        pid,
        resident: field("VmRSS:")?,
        peak: field("VmHWM:")?,
    })
}

pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 4] = ["B", "MiB", "GiB", "TiB"];
    let mut value = value as f64;
    let mut unit = 0;
    // Straight to MiB: nothing measured here is meaningfully sized in KiB.
    if value >= 1024.0 * 1024.0 {
        value /= 1024.0 * 1024.0;
        unit = 1;
    }
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// This session's accept-gate drop count, read from the running daemon.
///
/// The one capture-health fact with no host-side source: only the daemon sees
/// a chunk refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioDrops {
    pub not_active: u64,
}

/// The daemon's latest failure, kept after its HUD pill is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastError {
    /// The headline the user was shown.
    pub headline: String,
    /// The untranslated cause. May name paths: redact before showing it.
    pub detail: String,
    /// When it happened, in local time.
    pub at: String,
}

/// What the running daemon publishes for this page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonReport {
    /// `None` on a daemon older than the property - the report omits the line
    /// rather than claiming a clean session it cannot see.
    pub drops: Option<AudioDrops>,
    /// `None` when nothing has failed, or the daemon predates the property.
    pub last_error: Option<LastError>,
}

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";

/// `None` when the daemon is not running, which is not an error: "not running"
/// is a perfectly good diagnostic answer and the report says so.
///
/// Read through `gio`'s D-Bus rather than a zbus client - `gio` is already a
/// dependency, the call is synchronous, and this page refreshes on demand
/// rather than subscribing.
pub fn daemon_report() -> Option<DaemonReport> {
    use gio::glib::variant::ToVariant;

    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = connection
        .call_sync(
            Some(DICTATION_BUS),
            DICTATION_PATH,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            Some(&(DICTATION_BUS,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            1_000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    Some(parse_daemon_report(&reply.child_value(0)))
}

/// Read a `GetAll` reply's `a{sv}`. A property this daemon does not publish is
/// absent, never an error: a Settings app newer than the daemon is normal.
fn parse_daemon_report(all: &gio::glib::Variant) -> DaemonReport {
    let properties = gio::glib::VariantDict::new(Some(all));
    // VariantDict lookup unboxes the value from GetAll's a{sv} reply.
    let drops = properties
        .lookup::<u64>("AudioDroppedNotActive")
        .ok()
        .flatten()
        .map(|not_active| AudioDrops { not_active });
    let text = |name: &str| properties.lookup::<String>(name).ok().flatten();
    let time = properties.lookup::<i64>("LastErrorTime").ok().flatten();
    let last_error = match (text("LastError"), time) {
        (Some(headline), Some(usec)) if !headline.is_empty() && usec > 0 => Some(LastError {
            headline,
            detail: text("LastErrorDetail").unwrap_or_default(),
            at: local_time(usec),
        }),
        _ => None,
    };
    DaemonReport { drops, last_error }
}

fn local_time(usec: i64) -> String {
    gio::glib::DateTime::from_unix_local(usec / 1_000_000)
        .ok()
        .and_then(|time| time.format("%Y-%m-%d %H:%M:%S").ok())
        .map(|time| time.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gio::glib::variant::ToVariant;

    fn reply(properties: &[(&str, gio::glib::Variant)]) -> gio::glib::Variant {
        let dict = gio::glib::VariantDict::new(None);
        for (name, value) in properties {
            dict.insert_value(name, value);
        }
        dict.end()
    }

    /// A daemon that predates the properties reads as "nothing to report",
    /// never as a failure of the page.
    #[test]
    fn an_older_daemon_has_no_last_error_and_no_drop_count() {
        let report = parse_daemon_report(&reply(&[("State", "idle".to_variant())]));
        assert_eq!(report, DaemonReport::default());
    }

    #[test]
    fn a_daemon_that_never_failed_has_no_last_error() {
        let report = parse_daemon_report(&reply(&[
            ("AudioDroppedNotActive", 0u64.to_variant()),
            ("LastError", "".to_variant()),
            ("LastErrorDetail", "".to_variant()),
            ("LastErrorTime", 0i64.to_variant()),
        ]));
        assert_eq!(report.drops, Some(AudioDrops { not_active: 0 }));
        assert_eq!(report.last_error, None);
    }

    #[test]
    fn the_last_error_is_read_with_its_time() {
        let report = parse_daemon_report(&reply(&[
            ("LastError", "Model not running".to_variant()),
            ("LastErrorDetail", "x is connected".to_variant()),
            ("LastErrorTime", 1_759_276_800_000_000i64.to_variant()),
        ]));
        let last = report.last_error.expect("a last error");
        assert_eq!(last.headline, "Model not running");
        assert_eq!(last.detail, "x is connected");
        assert!(last.at.starts_with("2025-"), "{}", last.at);
    }

    #[test]
    fn the_os_is_its_pretty_name() {
        let release = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.3 LTS\"\nID=ubuntu\n";
        assert_eq!(os_name(release).as_deref(), Some("Ubuntu 24.04.3 LTS"));
        assert_eq!(os_name("PRETTY_NAME=Debian\n").as_deref(), Some("Debian"));
        assert_eq!(os_name("PRETTY_NAME=\"\"\n"), None);
        assert_eq!(os_name(""), None);
    }

    #[test]
    fn the_desktop_names_its_session_type() {
        assert_eq!(
            desktop_summary(Some("ubuntu:GNOME".into()), Some("wayland".into())),
            "ubuntu:GNOME, wayland"
        );
        assert_eq!(desktop_summary(None, None), "unknown, unknown session");
    }
}
