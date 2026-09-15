use std::path::{Path, PathBuf};

use myna_config::performance::{
    assess_clock, read_clock_facts, read_pressure, spin_probe, ClockVerdict, Pressure,
    CLAMP_FRACTION_PERCENT,
};

/// A fake `/sys` with the files the reader consults, nothing else.
struct FakeSys {
    root: PathBuf,
}

impl FakeSys {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("myna-config-perf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn cpu(&self, index: u32, hardware_max: u64, policy_max: u64, min: u64) -> &Self {
        let dir = self
            .root
            .join(format!("devices/system/cpu/cpu{index}/cpufreq"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cpuinfo_max_freq"), format!("{hardware_max}\n")).unwrap();
        std::fs::write(dir.join("scaling_max_freq"), format!("{policy_max}\n")).unwrap();
        std::fs::write(dir.join("scaling_min_freq"), format!("{min}\n")).unwrap();
        self
    }

    fn profile(&self, profile: &str) -> &Self {
        let dir = self.root.join("firmware/acpi");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("platform_profile"), format!("{profile}\n")).unwrap();
        self
    }

    fn supply(&self, name: &str, kind: &str, online: Option<&str>, status: Option<&str>) -> &Self {
        let dir = self.root.join("class/power_supply").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        if let Some(online) = online {
            std::fs::write(dir.join("online"), format!("{online}\n")).unwrap();
        }
        if let Some(status) = status {
            std::fs::write(dir.join("status"), format!("{status}\n")).unwrap();
        }
        self
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for FakeSys {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const BIG: u64 = 5_090_910;
const SMALL: u64 = 3_506_494;
const FLOOR: u64 = 623_377;

/// The state this was written against: a Framework 13 Ryzen AI 300 after a
/// long suspend on mains, every core stuck at its floor with policy wide
/// open and the profile on balanced.
#[test]
fn the_clamped_laptop_is_reported_as_a_firmware_clamp() {
    let sys = FakeSys::new("clamped");
    for cpu in 0..16 {
        let max = if cpu % 2 == 0 { BIG } else { SMALL };
        sys.cpu(cpu, max, max, FLOOR);
    }
    sys.profile("balanced")
        .supply("ACAD", "Mains", Some("1"), None)
        .supply("BAT1", "Battery", None, Some("Not charging"));

    let facts = read_clock_facts(sys.path(), |cpu| {
        Some(if cpu % 2 == 0 { 858_836 } else { 602_579 })
    });

    assert_eq!(facts.classes.len(), 2, "{facts:?}");
    assert_eq!(facts.classes[0].cpu, 0);
    assert_eq!(facts.classes[0].cores, 8);
    assert_eq!(facts.classes[0].hardware_max_khz, BIG);
    assert_eq!(facts.classes[1].cpu, 1);
    assert_eq!(facts.classes[1].hardware_max_khz, SMALL);
    assert_eq!(facts.power.platform_profile.as_deref(), Some("balanced"));
    assert_eq!(facts.power.on_mains, Some(true));
    assert_eq!(facts.power.battery_status.as_deref(), Some("Not charging"));
    assert_eq!(
        assess_clock(&facts),
        ClockVerdict::FirmwareClamp {
            cpu: 0,
            achieved_khz: 858_836,
            hardware_max_khz: BIG,
        }
    );
}

#[test]
fn a_healthy_boost_on_every_class_is_healthy() {
    let sys = FakeSys::new("healthy");
    sys.cpu(0, BIG, BIG, FLOOR).cpu(1, SMALL, SMALL, FLOOR);
    let facts = read_clock_facts(sys.path(), |cpu| {
        Some(if cpu == 0 { 4_950_000 } else { 3_400_000 })
    });
    assert_eq!(assess_clock(&facts), ClockVerdict::Healthy);
}

#[test]
fn a_policy_cap_is_attributed_to_software() {
    let sys = FakeSys::new("capped");
    let cap = BIG * (CLAMP_FRACTION_PERCENT - 10) / 100;
    sys.cpu(0, BIG, cap, FLOOR).profile("balanced");
    let facts = read_clock_facts(sys.path(), |_| Some(cap - 1_000));
    assert_eq!(
        assess_clock(&facts),
        ClockVerdict::SoftwareCap {
            cpu: 0,
            policy_max_khz: cap,
            hardware_max_khz: BIG,
        }
    );
}

#[test]
fn a_low_power_profile_is_attributed_before_firmware() {
    let sys = FakeSys::new("lowpower");
    sys.cpu(0, BIG, BIG, FLOOR).profile("low-power");
    let facts = read_clock_facts(sys.path(), |_| Some(1_000_000));
    assert!(
        matches!(
            assess_clock(&facts),
            ClockVerdict::LowPowerProfile { cpu: 0, .. }
        ),
        "{facts:?}"
    );
}

#[test]
fn only_the_slow_class_needs_to_fail() {
    let sys = FakeSys::new("mixed");
    sys.cpu(0, BIG, BIG, FLOOR).cpu(1, SMALL, SMALL, FLOOR);
    // The big cores boost, the small class is stuck: still a clamp, and the
    // report names the class that failed.
    let facts = read_clock_facts(sys.path(), |cpu| {
        Some(if cpu == 0 { 4_900_000 } else { 602_000 })
    });
    assert_eq!(
        assess_clock(&facts),
        ClockVerdict::FirmwareClamp {
            cpu: 1,
            achieved_khz: 602_000,
            hardware_max_khz: SMALL,
        }
    );
}

#[test]
fn a_host_without_cpufreq_is_unknown_never_an_alarm() {
    let sys = FakeSys::new("nocpufreq");
    std::fs::create_dir_all(sys.path().join("devices/system/cpu/cpu0")).unwrap();
    let probed = std::cell::Cell::new(false);
    let facts = read_clock_facts(sys.path(), |_| {
        probed.set(true);
        Some(1)
    });
    assert!(!probed.get(), "nothing to probe on a tree without cpufreq");
    assert!(facts.classes.is_empty());
    assert_eq!(facts.power.on_mains, None);
    assert_eq!(assess_clock(&facts), ClockVerdict::Unknown);

    let missing = FakeSys::new("missing");
    assert_eq!(read_pressure(&missing.path().join("pressure")), None);
}

#[test]
fn a_failed_probe_is_unknown_never_an_alarm() {
    let sys = FakeSys::new("noprobe");
    sys.cpu(0, BIG, BIG, FLOOR);
    let facts = read_clock_facts(sys.path(), |_| None);
    assert_eq!(assess_clock(&facts), ClockVerdict::Unknown);
}

#[test]
fn pressure_is_read_in_hundredths_of_a_percent() {
    let sys = FakeSys::new("psi");
    let dir = sys.path().join("pressure");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cpu"),
        "some avg10=81.20 avg60=0.11 avg300=0.06 total=714228610\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("memory"),
        "some avg10=0.00 avg60=0.01 avg300=0.00 total=38623647\nfull avg10=0.00 avg60=0.01 avg300=0.00 total=33630956\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("io"),
        "some avg10=0.00 avg60=0.13 avg300=0.06 total=317620255\nfull avg10=3.5 avg60=0.12 avg300=0.06 total=296782285\n",
    )
    .unwrap();
    assert_eq!(
        read_pressure(&dir),
        Some(Pressure {
            cpu_some: 81_20,
            memory_some: 0,
            io_full: 3_50,
        })
    );
}

/// `cpuinfo_max_freq` is not a ceiling everywhere: cppc_cpufreq and
/// acpi-cpufreq report the nominal clock while `scaling_cur_freq` includes
/// boost. A GitHub runner reported 2.3 GHz and reached 3.6 GHz.
#[test]
fn a_core_boosting_past_its_reported_maximum_is_healthy() {
    let sys = FakeSys::new("pastmax");
    sys.cpu(0, 2_300_000, 2_300_000, 800_000);
    let facts = read_clock_facts(sys.path(), |_| Some(3_600_178));
    assert_eq!(assess_clock(&facts), ClockVerdict::Healthy);
}

/// The one test against the live host: the probe must produce a number on
/// any Linux box with cpufreq. No verdict or ceiling is asserted; both depend
/// on the host and its cpufreq driver.
#[test]
fn the_live_probe_reports_a_loaded_core() {
    let facts = read_clock_facts(Path::new("/sys"), spin_probe);
    if facts.classes.is_empty() {
        eprintln!("no cpufreq on this host; nothing to probe");
        return;
    }
    for class in &facts.classes {
        let achieved = class.achieved_khz.expect("the probe returned nothing");
        assert!(achieved > 0, "{class:?}");
    }
    eprintln!("live verdict: {:?} from {facts:?}", assess_clock(&facts));
}
