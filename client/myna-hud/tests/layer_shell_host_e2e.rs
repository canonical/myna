// tests/layer_shell_host_e2e.rs - `myna-hud --host layer-shell` as a
// wlr-layer-shell surface, read back from the requests it sends.
//
// The real binary runs under a headless sway of its own against a fake
// publisher on a private bus, with libwayland's request log on
// (`WAYLAND_DEBUG=client`): the layer surface's role, its anchor, margin,
// keyboard interactivity and exclusive zone, and the input region, as a
// compositor receives them, across an idle unmap and remap.
//
// Needs sway and dbus-daemon, which the workshop has; it fails without them.

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use myna_platform::status_surface::BOTTOM_MARGIN;
use support::{dictation, Hud, PrivateBus};

const WAIT: Duration = Duration::from_secs(20);

/// A headless sway in a runtime directory of its own, torn down on drop.
struct Sway {
    compositor: Child,
    runtime: PathBuf,
    display: String,
}

impl Sway {
    fn spawn() -> Self {
        let runtime = std::env::temp_dir().join(format!("myna-hud-sway-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime);
        std::fs::create_dir_all(&runtime).unwrap();
        let config = runtime.join("config");
        std::fs::write(&config, "xwayland disable\n").unwrap();
        let compositor = Command::new("sway")
            .arg("-c")
            .arg(&config)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sway (the workshop installs it)");
        let deadline = Instant::now() + WAIT;
        let display = loop {
            if let Some(name) = socket(&runtime) {
                break name;
            }
            assert!(Instant::now() < deadline, "sway made no Wayland socket");
            std::thread::sleep(Duration::from_millis(20));
        };
        Self {
            compositor,
            runtime,
            display,
        }
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        let _ = self.compositor.kill();
        let _ = self.compositor.wait();
        let _ = std::fs::remove_dir_all(&self.runtime);
    }
}

/// The `wayland-N` socket in `runtime`, once its lock exists too.
fn socket(runtime: &Path) -> Option<String> {
    std::fs::read_dir(runtime)
        .ok()?
        .flatten()
        .find_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let locked = runtime.join(format!("{name}.lock")).exists();
            (name.starts_with("wayland-") && !name.contains('.') && locked).then_some(name)
        })
}

/// The HUD's protocol log, as libwayland writes it.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn follow(&self, stderr: impl std::io::Read + Send + 'static) {
        let lines = self.0.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        });
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn wait_for<T>(&self, what: &str, mut found: impl FnMut(&[String]) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(value) = found(&self.lines()) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "{what}; the log:\n{}",
                self.lines().join("\n")
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A request the client sent, `interface#id.call` and its arguments.
fn request<'a>(line: &'a str, target: &str) -> Option<&'a str> {
    let (_, sent) = line.split_once(" -> ")?;
    sent.strip_prefix(target)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// An event the client received for `target` (`interface#id.event`).
fn event<'a>(line: &'a str, target: &str) -> Option<&'a str> {
    if line.contains(" -> ") {
        return None;
    }
    let (_, received) = line.split_once("} ")?;
    received
        .trim_start()
        .strip_prefix(target)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// The `#id` after `prefix` in `text`.
fn id_after(text: &str, prefix: &str) -> Option<u32> {
    let rest = &text[text.find(prefix)? + prefix.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// One layer surface the HUD made: what it asked for, from its creation.
#[derive(Debug, PartialEq)]
struct LayerSurface {
    id: u32,
    surface: u32,
    /// `output, layer, namespace`.
    role: String,
    /// Every request on it, in order.
    calls: Vec<String>,
}

fn layer_surfaces(lines: &[String]) -> Vec<LayerSurface> {
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(args) = line.split_once(" -> ").and_then(|(_, sent)| {
            sent.starts_with("zwlr_layer_shell_v1#")
                .then(|| sent.split_once(".get_layer_surface("))
                .flatten()
                .map(|(_, args)| args.trim_end_matches(')'))
        }) else {
            continue;
        };
        let (Some(id), Some(surface)) = (
            id_after(args, "zwlr_layer_surface_v1#"),
            id_after(args, "wl_surface#"),
        ) else {
            continue;
        };
        let role = args.splitn(3, ", ").nth(2).unwrap_or_default().to_string();
        let target = format!("zwlr_layer_surface_v1#{id}.");
        let calls = lines[i + 1..]
            .iter()
            .filter_map(|l| l.split_once(" -> ").map(|(_, sent)| sent))
            .filter_map(|sent| sent.strip_prefix(&target))
            .map(str::to_string)
            .take_while(|call| call != "destroy()")
            .collect();
        found.push(LayerSurface {
            id,
            surface,
            role,
            calls,
        });
    }
    found
}

/// Whether the surface's last input region was empty: a region it set
/// with nothing added to it since the region was made.
fn input_region_is_empty(lines: &[String], surface: u32) -> bool {
    let set = format!("wl_surface#{surface}.set_input_region");
    let Some(at) = lines.iter().rposition(|l| request(l, &set).is_some()) else {
        return false;
    };
    let Some(region) = request(&lines[at], &set).and_then(|a| id_after(a, "wl_region#")) else {
        return false;
    };
    let made = format!("new id wl_region#{region}");
    let Some(from) = lines[..at].iter().rposition(|l| l.contains(&made)) else {
        return false;
    };
    let add = format!("wl_region#{region}.add");
    !lines[from..at].iter().any(|l| request(l, &add).is_some())
}

/// The requests the HUD's layer surface must carry, after its role.
fn expected_calls() -> Vec<String> {
    [
        "set_anchor(2)".to_string(),
        format!("set_margin(0, 0, {BOTTOM_MARGIN}, 0)"),
        "set_keyboard_interactivity(0)".to_string(),
        "set_exclusive_zone(0)".to_string(),
    ]
    .into()
}

fn configured(lines: &[String], layer: &LayerSurface) -> bool {
    let target = format!("zwlr_layer_surface_v1#{}.configure", layer.id);
    let attach = format!("wl_surface#{}.attach", layer.surface);
    let Some(at) = lines.iter().position(|l| event(l, &target).is_some()) else {
        return false;
    };
    lines[at..].iter().any(|l| request(l, &attach).is_some())
}

#[test]
fn the_hud_is_a_layer_surface_on_wayland() {
    let bus = PrivateBus::spawn().expect("dbus-daemon (the workshop installs it)");
    let sway = Sway::spawn();
    let publisher = dictation::serve(&bus.address, "recording");

    let log = Log::default();
    let mut hud = Hud(Command::new(env!("CARGO_BIN_EXE_myna-hud"))
        .args(["--host", "layer-shell"])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("WAYLAND_DISPLAY", &sway.display)
        .env("WAYLAND_DEBUG", "client")
        .env("GIO_USE_VFS", "local")
        .env("GTK_A11Y", "none")
        .env_remove("DISPLAY")
        .env_remove("GDK_BACKEND")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn myna-hud"));
    log.follow(hud.0.stderr.take().unwrap());

    // Mapped: a configured layer surface with a buffer on it.
    let first = log.wait_for("no layer surface was mapped", |lines| {
        layer_surfaces(lines)
            .into_iter()
            .next()
            .filter(|layer| configured(lines, layer))
    });
    assert_eq!(
        first.role, "nil, 3, \"myna-hud\"",
        "output, layer, namespace"
    );
    for call in expected_calls() {
        assert!(first.calls.contains(&call), "{call} missing: {first:?}");
    }
    assert!(
        input_region_is_empty(&log.lines(), first.surface),
        "the HUD takes pointer input"
    );

    // Idle unmaps; the next state maps an identical layer surface.
    dictation::publish(&publisher, "idle", "");
    log.wait_for("idle kept the layer surface", |lines| {
        let destroy = format!("zwlr_layer_surface_v1#{}.destroy", first.id);
        lines
            .iter()
            .any(|l| request(l, &destroy).is_some())
            .then_some(())
    });
    dictation::publish(&publisher, "recording", "");
    let second = log.wait_for("no second layer surface was mapped", |lines| {
        layer_surfaces(lines)
            .into_iter()
            .nth(1)
            .filter(|layer| configured(lines, layer))
    });
    assert_eq!(second.role, first.role);
    for call in expected_calls() {
        assert!(second.calls.contains(&call), "{call} missing: {second:?}");
    }
    assert!(
        input_region_is_empty(&log.lines(), second.surface),
        "the remapped HUD takes pointer input"
    );

    let pid = hud.0.id();
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = hud.0.try_wait().expect("try_wait") {
            break status;
        }
        assert!(Instant::now() < deadline, "myna-hud ignored SIGTERM");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0), "myna-hud exited {status}");
}

#[test]
fn the_layer_shell_host_refuses_an_x11_session() {
    let output = Command::new(env!("CARGO_BIN_EXE_myna-hud"))
        .args(["--host", "layer-shell"])
        .env("DISPLAY", ":99")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("run myna-hud");
    assert_eq!(output.status.code(), Some(78), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("needs a session of kind Wayland"),
        "{stderr}"
    );
}
