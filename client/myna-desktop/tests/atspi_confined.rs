//! Regression suite for the AppArmor surface a strict snap actually gets on
//! `org.a11y.Bus` — feature 011-accessible-dictation-ux, T087.
//!
//! **Why this exists.** `atspi_hw.rs` proves the announcer works against a
//! *permissive* accessibility bus, which is what a dev session and CI both
//! provide. It cannot fail the way the packaged snap fails, because the
//! packaged snap's problem is not the AT-SPI protocol — it is that snapd's
//! `desktop-legacy` interface allows only a narrow member allowlist on the
//! accessibility bus, and a connection bootstrap that strays outside it is
//! refused before a single announcement is emitted.
//!
//! Verified against the generated profile on snapd 2.78
//! (`/var/lib/snapd/apparmor/profiles/snap.myna.myna`), probing from inside
//! `snap run --shell myna.myna`, `org.freedesktop.DBus.Properties.GetAll` is
//! **denied** on every path this bootstrap touches:
//!
//! | path | result |
//! | --- | --- |
//! | `/org/freedesktop/DBus` | `AccessDenied` |
//! | `/org/a11y/atspi/registry` | `AccessDenied` |
//! | `/org/a11y/atspi/accessible/root` | `AccessDenied` |
//!
//! The profile permits `Properties.Get{,All}` only on
//! `/org/a11y/atspi/accessible/[0-9]*`. The `member="Get*"` rule that does
//! exist on the application root is scoped to the `org.a11y.atspi.Accessible`
//! interface and so does not cover `org.freedesktop.DBus.Properties`.
//!
//! So this suite stands up a real `dbus-daemon` whose policy mirrors those
//! denials via `send_path`, and asserts the announcer still connects and
//! announces. A bootstrap that constructs an eagerly-property-caching proxy
//! fails here exactly as it fails in the snap — on a developer machine, in
//! seconds, with no snap build and no screen reader.
//!
//! Skips cleanly when `dbus-daemon` is absent (constitution Principle II).

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use myna_desktop::accessibility::atspi::AtspiAnnouncer;
use myna_desktop::accessibility::{AccessibilityAnnouncer, AnnouncementText, Severity};

/// A `dbus-daemon` whose policy reproduces the snap profile's refusals, killed
/// on drop so a failing assertion cannot leak the process or its config file.
struct ConfinedBus {
    child: Child,
    address: String,
    config: PathBuf,
}

impl Drop for ConfinedBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config);
    }
}

/// `send_path` is what lets this mirror AppArmor precisely: the denials are
/// per-object-path, not blanket, so everything the announcer legitimately does
/// (`Hello`, `AddMatch`, owning a name, exporting objects, emitting signals)
/// still succeeds and only the forbidden property reads are refused.
///
/// `receive_sender` is not optional: `dbus-daemon` defaults to denying receipt
/// as well as sending, so without it even an *allowed* call hangs forever
/// waiting for a reply it is not permitted to read — which looks exactly like
/// the bug this suite is meant to catch, and is not it.
const POLICY: &str = r#"<!DOCTYPE busconfig PUBLIC
 "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
    <deny send_interface="org.freedesktop.DBus.Properties"
          send_path="/org/freedesktop/DBus"/>
    <deny send_interface="org.freedesktop.DBus.Properties"
          send_path="/org/a11y/atspi/registry"/>
    <deny send_interface="org.freedesktop.DBus.Properties"
          send_path="/org/a11y/atspi/accessible/root"/>
  </policy>
</busconfig>
"#;

fn start_confined_bus() -> Option<ConfinedBus> {
    // Unique per test binary *and* per test, so the two tests in this file can
    // run concurrently under the default harness without racing on the path.
    let config = std::env::temp_dir().join(format!(
        "myna-atspi-confined-{}-{:?}.conf",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&config, POLICY).expect("the bus config should be writable");

    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .arg("--print-address")
        .arg("--nofork")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take().expect("piped stdout");
    let mut address = String::new();
    BufReader::new(stdout)
        .read_line(&mut address)
        .expect("dbus-daemon should print its address");

    Some(ConfinedBus {
        child,
        address: address.trim().to_string(),
        config,
    })
}

fn confined_bus() -> Option<ConfinedBus> {
    if Command::new("dbus-daemon")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping atspi_confined: dbus-daemon is not installed");
        return None;
    }
    start_confined_bus()
}

// ── T087: the bootstrap must stay inside the snap's member allowlist ───────

#[tokio::test]
async fn connects_to_a_bus_that_forbids_the_property_reads_snapd_denies() {
    let Some(bus) = confined_bus() else {
        return;
    };

    let announcer = AtspiAnnouncer::connect_to(&bus.address).await;

    assert!(
        announcer.is_ok(),
        "connect_to() must not depend on property reads snapd's desktop-legacy \
         interface denies — this is the packaged-snap failure reproduced: {:?}",
        announcer.err()
    );
}

#[tokio::test]
async fn announces_on_a_bus_that_forbids_those_property_reads() {
    let Some(bus) = confined_bus() else {
        return;
    };

    let mut announcer = AtspiAnnouncer::connect_to(&bus.address)
        .await
        .expect("connect_to() should succeed against the confined policy");

    announcer
        .announce(AnnouncementText::new("Listening"), None)
        .await
        .expect("a polite announcement should survive the confined policy");
    announcer
        .announce(
            AnnouncementText::new("Backend unavailable"),
            Some(Severity::Critical),
        )
        .await
        .expect("an assertive announcement should survive the confined policy");
}
