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
/// a chunk refused. Read through `gio`'s D-Bus rather than a zbus client -
/// `gio` is already a dependency, the call is synchronous, and this page
/// refreshes on demand rather than subscribing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioDrops {
    pub not_active: u64,
}

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";

/// `None` when the daemon is not running, which is not an error: "not running"
/// is a perfectly good diagnostic answer and the report says so.
pub fn audio_drops() -> Option<AudioDrops> {
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
    let properties = gio::glib::VariantDict::new(Some(&reply.child_value(0)));
    // Absent, not zero, on a daemon older than this property - the report
    // omits the line rather than claiming a clean session it cannot see.
    // VariantDict lookup unboxes the value from GetAll's a{sv} reply.
    let read = |name: &str| properties.lookup::<u64>(name).ok().flatten();
    Some(AudioDrops {
        not_active: read("AudioDroppedNotActive")?,
    })
}
