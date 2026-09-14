//! Whether this machine can currently deliver the CPU that inference needs.
//!
//! Dictation is only usable at full clock, and an idle core legitimately sits
//! at its floor, so a snapshot of `scaling_cur_freq` says nothing. The probe
//! here loads one core per frequency class for a fraction of a second and
//! records what clock it actually reached. The verdict then attributes a
//! shortfall to the layer that can fix it: a policy cap the user set, a
//! low-power platform profile, or a clamp below the kernel that only a
//! firmware-level action clears.
//!
//! Everything is a `/sys` or `/proc` read plus a busy thread; no subprocess.
//! The readers take the tree root so tests can hand them a fixture, and the
//! probe is a closure for the same reason. Verdicts are enums, never text:
//! wording belongs to the presenter.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long one core is loaded for. Long enough for any governor to ramp
/// (amd-pstate and intel_pstate both settle within tens of milliseconds),
/// short enough that a diagnostics refresh with several classes stays well
/// under a second.
pub const PROBE_DURATION: Duration = Duration::from_millis(300);
const PROBE_SAMPLE_INTERVAL: Duration = Duration::from_millis(20);

/// A core that cannot reach this fraction of its own hardware maximum under
/// a single-thread load is not going to run inference acceptably. A healthy
/// laptop boosts one core to or near its maximum; a firmware clamp leaves it
/// at 10-20 %. Half is far from both.
pub const CLAMP_FRACTION_PERCENT: u64 = 50;

/// Pressure-stall thresholds, in hundredths of a percent of the last ten
/// seconds. Memory and I/O stalls of a tenth of the wall clock mean the
/// machine is swapping or thrashing; CPU "some" is only worth a warning when
/// nearly everything is waiting for a core, since a build in another window
/// is not a fault.
pub const MEMORY_PRESSURE_WARN: u32 = 10_00;
pub const IO_PRESSURE_WARN: u32 = 10_00;
pub const CPU_PRESSURE_WARN: u32 = 80_00;

/// One frequency class: every core that shares a hardware maximum, probed
/// through one representative. Heterogeneous parts expose several classes
/// (a 5.1 GHz class and a 3.5 GHz class on a Zen 5 laptop, for instance), and
/// each has to be judged against its own ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockClass {
    /// The core that was probed.
    pub cpu: u32,
    /// How many cores share this hardware maximum.
    pub cores: usize,
    /// `cpuinfo_max_freq`: what the silicon can do.
    pub hardware_max_khz: u64,
    /// `scaling_max_freq`: what policy allows.
    pub policy_max_khz: u64,
    /// `scaling_min_freq`: the floor an idle core rests at.
    pub min_khz: u64,
    /// The highest `scaling_cur_freq` observed while the probe loaded the
    /// core. `None` when the probe could not run.
    pub achieved_khz: Option<u64>,
}

/// What the host says about its own power state. Context for the report,
/// never the basis of a verdict: a clamp is a clamp whether or not the
/// charger is plugged in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerFacts {
    /// `/sys/firmware/acpi/platform_profile`, when the platform exposes one.
    pub platform_profile: Option<String>,
    /// Whether a mains supply reports itself online.
    pub on_mains: Option<bool>,
    /// The first battery's status line, verbatim from the kernel.
    pub battery_status: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClockFacts {
    pub classes: Vec<ClockClass>,
    pub power: PowerFacts,
}

/// Pressure-stall information from `/proc/pressure`, in hundredths of a
/// percent over the ten-second window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pressure {
    pub cpu_some: u32,
    pub memory_some: u32,
    pub io_full: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PerformanceFacts {
    pub clock: ClockFacts,
    pub pressure: Option<Pressure>,
}

/// Why the CPU is not delivering, in the order a user should try fixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockVerdict {
    /// No cpufreq tree, or no class could be probed.
    Unknown,
    /// Every class reached an acceptable fraction of its maximum.
    Healthy,
    /// `scaling_max_freq` is set well below the hardware maximum. Something
    /// in userspace or the boot line did this, and it can undo it.
    SoftwareCap {
        cpu: u32,
        policy_max_khz: u64,
        hardware_max_khz: u64,
    },
    /// The platform profile is `low-power`. Switching it is a settings change.
    LowPowerProfile {
        cpu: u32,
        achieved_khz: u64,
        hardware_max_khz: u64,
    },
    /// Policy allows the maximum, the profile is not low-power, and the core
    /// still would not leave its floor: the clamp sits below the kernel.
    FirmwareClamp {
        cpu: u32,
        achieved_khz: u64,
        hardware_max_khz: u64,
    },
}

/// Which pressure signals crossed their threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressureWarning {
    Memory(u32),
    Io(u32),
    Cpu(u32),
}

/// Judge the probed classes. The worst class decides: a part whose big
/// cores are clamped is unusable even if its small cores are fine.
pub fn assess_clock(facts: &ClockFacts) -> ClockVerdict {
    let probed: Vec<&ClockClass> = facts
        .classes
        .iter()
        .filter(|class| class.achieved_khz.is_some())
        .collect();
    if probed.is_empty() {
        return ClockVerdict::Unknown;
    }
    let threshold = |class: &ClockClass| class.hardware_max_khz * CLAMP_FRACTION_PERCENT / 100;
    let short = probed
        .iter()
        .copied()
        .filter(|class| class.achieved_khz.unwrap_or(0) < threshold(class))
        .min_by_key(|class| class.achieved_khz.unwrap_or(0) * 100 / class.hardware_max_khz.max(1));
    let Some(class) = short else {
        return ClockVerdict::Healthy;
    };
    let achieved_khz = class.achieved_khz.unwrap_or(0);
    if class.policy_max_khz < threshold(class) {
        return ClockVerdict::SoftwareCap {
            cpu: class.cpu,
            policy_max_khz: class.policy_max_khz,
            hardware_max_khz: class.hardware_max_khz,
        };
    }
    if facts.power.platform_profile.as_deref() == Some("low-power") {
        return ClockVerdict::LowPowerProfile {
            cpu: class.cpu,
            achieved_khz,
            hardware_max_khz: class.hardware_max_khz,
        };
    }
    ClockVerdict::FirmwareClamp {
        cpu: class.cpu,
        achieved_khz,
        hardware_max_khz: class.hardware_max_khz,
    }
}

pub fn assess_pressure(pressure: Pressure) -> Vec<PressureWarning> {
    let mut warnings = Vec::new();
    if pressure.memory_some >= MEMORY_PRESSURE_WARN {
        warnings.push(PressureWarning::Memory(pressure.memory_some));
    }
    if pressure.io_full >= IO_PRESSURE_WARN {
        warnings.push(PressureWarning::Io(pressure.io_full));
    }
    if pressure.cpu_some >= CPU_PRESSURE_WARN {
        warnings.push(PressureWarning::Cpu(pressure.cpu_some));
    }
    warnings
}

/// Everything the diagnostics page wants, measured against the live host.
/// Blocks for [`PROBE_DURATION`] per frequency class; call it off the main
/// thread.
pub fn performance_facts() -> PerformanceFacts {
    PerformanceFacts {
        clock: read_clock_facts(Path::new("/sys"), spin_probe),
        pressure: read_pressure(Path::new("/proc/pressure")),
    }
}

/// Read the cpufreq tree under `sys` and probe one core per hardware-maximum
/// class with `probe`, which loads the given core and returns the highest
/// clock it saw.
pub fn read_clock_facts(sys: &Path, probe: impl Fn(u32) -> Option<u64>) -> ClockFacts {
    let mut classes = clock_classes(&sys.join("devices/system/cpu"));
    for class in &mut classes {
        class.achieved_khz = probe(class.cpu);
    }
    ClockFacts {
        classes,
        power: read_power(sys),
    }
}

fn clock_classes(cpu_root: &Path) -> Vec<ClockClass> {
    let read_khz = |dir: &Path, name: &str| {
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    };
    let mut cores: Vec<(u32, u64, u64, u64)> = std::fs::read_dir(cpu_root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let cpu: u32 = name.to_str()?.strip_prefix("cpu")?.parse().ok()?;
            let cpufreq = entry.path().join("cpufreq");
            Some((
                cpu,
                read_khz(&cpufreq, "cpuinfo_max_freq")?,
                read_khz(&cpufreq, "scaling_max_freq")?,
                read_khz(&cpufreq, "scaling_min_freq")?,
            ))
        })
        .collect();
    cores.sort_unstable();
    let mut classes: Vec<ClockClass> = Vec::new();
    for (cpu, hardware_max_khz, policy_max_khz, min_khz) in cores {
        match classes
            .iter_mut()
            .find(|class| class.hardware_max_khz == hardware_max_khz)
        {
            Some(class) => class.cores += 1,
            None => classes.push(ClockClass {
                cpu,
                cores: 1,
                hardware_max_khz,
                policy_max_khz,
                min_khz,
                achieved_khz: None,
            }),
        }
    }
    // Biggest cores first: that is the class inference runs on, and the one
    // the report should lead with.
    classes.sort_by_key(|c| std::cmp::Reverse(c.hardware_max_khz));
    classes
}

fn read_power(sys: &Path) -> PowerFacts {
    let trimmed = |path: &Path| {
        std::fs::read_to_string(path)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let mut on_mains = None;
    let mut battery_status = None;
    let supplies = sys.join("class/power_supply");
    let mut entries: Vec<_> = std::fs::read_dir(&supplies)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for supply in entries {
        match trimmed(&supply.join("type")).as_deref() {
            Some("Mains") => {
                let online = trimmed(&supply.join("online")).as_deref() == Some("1");
                on_mains = Some(on_mains.unwrap_or(false) || online);
            }
            Some("Battery") if battery_status.is_none() => {
                battery_status = trimmed(&supply.join("status"));
            }
            _ => {}
        }
    }
    PowerFacts {
        platform_profile: trimmed(&sys.join("firmware/acpi/platform_profile")),
        on_mains,
        battery_status,
    }
}

/// Parse the three `/proc/pressure` files. `None` when the kernel has no PSI,
/// which is a fine answer: the report omits the line rather than guessing.
pub fn read_pressure(dir: &Path) -> Option<Pressure> {
    let file = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    Some(Pressure {
        cpu_some: avg10(&file("cpu")?, "some")?,
        memory_some: avg10(&file("memory")?, "some")?,
        io_full: avg10(&file("io")?, "full")?,
    })
}

/// The `avg10` field of the `some` or `full` line, in hundredths of a percent.
fn avg10(content: &str, line: &str) -> Option<u32> {
    let value = content
        .lines()
        .find(|candidate| candidate.starts_with(line))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?;
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    let whole: u32 = whole.parse().ok()?;
    let fraction: u32 = format!("{fraction:0<2}").get(..2)?.parse().ok()?;
    Some(whole * 100 + fraction)
}

/// Load `cpu` from a pinned thread for [`PROBE_DURATION`] and return the
/// highest `scaling_cur_freq` seen on the core the thread actually ran on.
///
/// Pinning can be refused (a cpuset, a container); the spinner then reports
/// which core it landed on and the sampler follows it, so the number is still
/// a loaded core's clock, just not necessarily the requested one.
pub fn spin_probe(cpu: u32) -> Option<u64> {
    let stop = Arc::new(AtomicBool::new(false));
    let running_on = Arc::new(AtomicUsize::new(cpu as usize));
    let spinner = {
        let stop = Arc::clone(&stop);
        let running_on = Arc::clone(&running_on);
        std::thread::spawn(move || {
            pin_to(cpu);
            let mut x: u64 = 1;
            let mut tick: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                tick = tick.wrapping_add(1);
                if tick % 4096 == 0 {
                    if let Some(here) = current_cpu() {
                        running_on.store(here, Ordering::Relaxed);
                    }
                }
            }
            std::hint::black_box(x);
        })
    };
    let started = Instant::now();
    let mut best = None;
    while started.elapsed() < PROBE_DURATION {
        std::thread::sleep(PROBE_SAMPLE_INTERVAL);
        let here = running_on.load(Ordering::Relaxed);
        let path = format!("/sys/devices/system/cpu/cpu{here}/cpufreq/scaling_cur_freq");
        if let Some(khz) = std::fs::read_to_string(path)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
        {
            best = Some(best.map_or(khz, |current: u64| current.max(khz)));
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.join();
    best
}

fn pin_to(cpu: u32) {
    // SAFETY: cpu_set_t is plain data; CPU_ZERO/CPU_SET write within it, and
    // sched_setaffinity reads exactly size_of::<cpu_set_t>() bytes from it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu as usize, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

fn current_cpu() -> Option<usize> {
    // SAFETY: sched_getcpu takes no pointers and has no failure mode beyond
    // returning -1.
    let cpu = unsafe { libc::sched_getcpu() };
    usize::try_from(cpu).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(cpu: u32, hardware: u64, policy: u64, achieved: Option<u64>) -> ClockClass {
        ClockClass {
            cpu,
            cores: 1,
            hardware_max_khz: hardware,
            policy_max_khz: policy,
            min_khz: 600_000,
            achieved_khz: achieved,
        }
    }

    #[test]
    fn avg10_parses_kernel_psi_lines() {
        let text = "some avg10=12.34 avg60=0.01 avg300=0.00 total=1\nfull avg10=0.50 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(avg10(text, "some"), Some(1234));
        assert_eq!(avg10(text, "full"), Some(50));
        assert_eq!(avg10("some avg10=7 total=1\n", "some"), Some(700));
        assert_eq!(avg10("", "some"), None);
    }

    #[test]
    fn the_worst_class_decides_and_the_floor_reads_as_firmware() {
        let facts = ClockFacts {
            classes: vec![
                class(0, 5_090_910, 5_090_910, Some(858_836)),
                class(1, 3_506_494, 3_506_494, Some(3_400_000)),
            ],
            power: PowerFacts::default(),
        };
        assert_eq!(
            assess_clock(&facts),
            ClockVerdict::FirmwareClamp {
                cpu: 0,
                achieved_khz: 858_836,
                hardware_max_khz: 5_090_910
            }
        );
    }

    #[test]
    fn a_policy_cap_is_named_before_firmware_is_blamed() {
        let facts = ClockFacts {
            classes: vec![class(0, 5_090_910, 1_500_000, Some(1_480_000))],
            power: PowerFacts::default(),
        };
        assert!(matches!(
            assess_clock(&facts),
            ClockVerdict::SoftwareCap {
                policy_max_khz: 1_500_000,
                ..
            }
        ));
    }

    #[test]
    fn low_power_profile_is_named_when_policy_allows_the_maximum() {
        let facts = ClockFacts {
            classes: vec![class(0, 5_090_910, 5_090_910, Some(1_200_000))],
            power: PowerFacts {
                platform_profile: Some("low-power".into()),
                ..PowerFacts::default()
            },
        };
        assert!(matches!(
            assess_clock(&facts),
            ClockVerdict::LowPowerProfile { .. }
        ));
    }

    #[test]
    fn low_power_profile_with_a_healthy_clock_is_healthy() {
        let facts = ClockFacts {
            classes: vec![class(0, 5_090_910, 5_090_910, Some(4_900_000))],
            power: PowerFacts {
                platform_profile: Some("low-power".into()),
                ..PowerFacts::default()
            },
        };
        assert_eq!(assess_clock(&facts), ClockVerdict::Healthy);
    }

    #[test]
    fn an_unprobed_tree_is_unknown_not_an_alarm() {
        assert_eq!(assess_clock(&ClockFacts::default()), ClockVerdict::Unknown);
        let unprobed = ClockFacts {
            classes: vec![class(0, 5_090_910, 5_090_910, None)],
            power: PowerFacts::default(),
        };
        assert_eq!(assess_clock(&unprobed), ClockVerdict::Unknown);
    }

    #[test]
    fn pressure_thresholds() {
        let quiet = Pressure::default();
        assert!(assess_pressure(quiet).is_empty());
        let swapping = Pressure {
            cpu_some: 2_00,
            memory_some: 25_50,
            io_full: 12_00,
        };
        assert_eq!(
            assess_pressure(swapping),
            vec![PressureWarning::Memory(25_50), PressureWarning::Io(12_00)]
        );
        let busy = Pressure {
            cpu_some: 79_99,
            ..Pressure::default()
        };
        assert!(assess_pressure(busy).is_empty());
    }
}
