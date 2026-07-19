//! Live input-device enumeration (plan T52, feature
//! 002-native-pipewire-backend, FR-008/FR-008a). Lists current PipeWire input
//! sources and notifies an observer as devices appear/disappear, so a settings
//! chooser stays current without polling. Read-only: no audio, nothing
//! persisted (audio-adapter-api §9; constitution Principle V).
//!
//! The stable `node.name` this yields is exactly what feeds
//! `CaptureSpec.target` for selection — enumeration and selection tie together
//! there (contract device-enumeration E7).

use std::collections::HashMap;
use std::sync::mpsc;

use myna_core::CaptureError;
use pipewire::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use tokio::sync::watch;

/// A discoverable input device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    /// Stable PipeWire `node.name` — the selector used as `CaptureSpec.target`.
    pub node_name: String,
    /// Human-readable `node.description` — for display only, never a selector.
    pub label: String,
}

/// A live enumeration event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceChange {
    /// A new input device appeared (name + label).
    Added(InputDevice),
    /// A device disappeared (by stable name).
    Removed { node_name: String },
}

/// Live input-device enumerator (audio-adapter-api §9; FR-008/FR-008a).
///
/// A dedicated PipeWire main-loop thread watches the registry and maintains the
/// current set of input sources. The latest full list is published on a
/// [`watch`] channel — a settings chooser subscribes and re-renders on change,
/// no polling. Dropping the handle stops the listener and releases the thread.
///
/// Read-only: no audio is captured, nothing is persisted (Principle V).
pub struct InputDevices {
    /// Latest snapshot; updated as devices appear/disappear.
    devices: watch::Receiver<Vec<InputDevice>>,
    /// Dropping this tells the loop thread to quit.
    _quit: QuitOnDrop,
}

struct QuitOnDrop(Option<mpsc::Sender<()>>);
impl Drop for QuitOnDrop {
    fn drop(&mut self) {
        // Sending (or dropping the sender) trips the loop's quit check.
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

impl InputDevices {
    /// Start watching the registry. Returns `Err(DeviceUnavailable)` if the
    /// PipeWire loop/context/registry can't be established (contract E5).
    pub fn new() -> Result<Self, CaptureError> {
        let (dev_tx, dev_rx) = watch::channel(Vec::<InputDevice>::new());
        let (quit_tx, quit_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), CaptureError>>();

        std::thread::Builder::new()
            .name("myna-pw-devices".into())
            .spawn(move || run_registry(dev_tx, quit_rx, ready_tx))
            .expect("spawning the PipeWire device-registry thread");

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { devices: dev_rx, _quit: QuitOnDrop(Some(quit_tx)) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(CaptureError::DeviceUnavailable(
                "PipeWire device-registry thread exited before signaling readiness".into(),
            )),
        }
    }

    /// Current snapshot of available input devices (empty, not an error, when
    /// none are present — contract E2).
    pub fn list(&self) -> Vec<InputDevice> {
        self.devices.borrow().clone()
    }

    /// A [`watch::Receiver`] of the latest device list — updates live as devices
    /// appear/disappear (FR-008a). Cheap for a UI to await changes on.
    pub fn watch(&self) -> watch::Receiver<Vec<InputDevice>> {
        self.devices.clone()
    }
}

/// The registry-watch body, on its own PipeWire loop thread. Publishes the full
/// input-device list to `dev_tx` on every add/remove; quits when `quit_rx`
/// receives (or its sender drops).
fn run_registry(
    dev_tx: watch::Sender<Vec<InputDevice>>,
    quit_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::Sender<Result<(), CaptureError>>,
) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    let main_loop = match MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            let _ = ready_tx.send(Err(CaptureError::DeviceUnavailable(format!(
                "cannot create PipeWire loop: {e}"
            ))));
            return;
        }
    };
    let context = match ContextRc::new(&main_loop, None) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(CaptureError::DeviceUnavailable(format!(
                "cannot create PipeWire context: {e}"
            ))));
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(CaptureError::DeviceUnavailable(format!(
                "cannot connect to PipeWire: {e}"
            ))));
            return;
        }
    };
    let registry = match core.get_registry_rc() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(CaptureError::DeviceUnavailable(format!(
                "cannot get PipeWire registry: {e}"
            ))));
            return;
        }
    };

    // id → InputDevice for the sources we've seen, so global_remove (which only
    // carries the id) can drop the right entry and republish.
    let known: Rc<RefCell<HashMap<u32, InputDevice>>> = Rc::new(RefCell::new(HashMap::new()));
    let publish = {
        let dev_tx = dev_tx.clone();
        let known = known.clone();
        move || {
            let mut list: Vec<InputDevice> = known.borrow().values().cloned().collect();
            list.sort_by(|a, b| a.node_name.cmp(&b.node_name));
            let _ = dev_tx.send(list);
        }
    };

    let _listener = registry
        .add_listener_local()
        .global({
            let known = known.clone();
            let publish = publish.clone();
            move |global| {
                if global.type_ != ObjectType::Node {
                    return;
                }
                let Some(props) = &global.props else { return };
                if let Some(dev) = map_input_device(|k| props.get(k)) {
                    known.borrow_mut().insert(global.id, dev);
                    publish();
                }
            }
        })
        .global_remove({
            let known = known.clone();
            let publish = publish.clone();
            move |id| {
                if known.borrow_mut().remove(&id).is_some() {
                    publish();
                }
            }
        })
        .register();

    // Quit-poll timer: leave the loop promptly once the handle is dropped.
    let timer = main_loop.loop_().add_timer({
        let main_loop = main_loop.clone();
        move |_| {
            // Any message, or a disconnected sender, means "quit".
            match quit_rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => main_loop.quit(),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    });
    let _ = timer.update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100)))
        .into_result();

    // Registry callbacks fire during the initial roundtrip once the loop runs,
    // so signal readiness now; the first snapshot publishes as globals arrive.
    let _ = ready_tx.send(Ok(()));
    main_loop.run();
    drop(timer);
}

/// Map a registry node's PipeWire properties to an [`InputDevice`], or `None`
/// if the node is not a selectable input source (data-model `InputDevice`,
/// contract device-enumeration E-mapping). `lookup` returns a property value by
/// key (e.g. from `pipewire::spa::utils::dict::DictRef::get`), so this stays a
/// pure function testable without a PipeWire connection.
///
/// Rules: keep only `media.class == "Audio/Source"` (a real capture source —
/// sink monitors are `Audio/Source/Virtual` or carry a monitor marker and are
/// excluded); require a non-empty stable `node.name`; use `node.description`
/// (falling back to `node.nick`, then `node.name`) as the human label.
pub(crate) fn map_input_device<'a>(
        lookup: impl Fn(&str) -> Option<&'a str>,
    ) -> Option<InputDevice> {
    // Must be a plain audio capture source.
    if lookup("media.class") != Some("Audio/Source") {
        return None;
    }
    // Exclude sink monitors even if mislabeled as Audio/Source.
    if lookup("node.name").is_some_and(|n| n.ends_with(".monitor"))
        || matches!(lookup("media.role"), Some("Monitor"))
    {
        return None;
    }
    // A stable, non-empty node.name is required — it is the selector.
    let node_name = lookup("node.name").filter(|n| !n.is_empty())?;
    let label = lookup("node.description")
        .or_else(|| lookup("node.nick"))
        .filter(|s| !s.is_empty())
        .unwrap_or(node_name)
        .to_string();
    Some(InputDevice { node_name: node_name.to_string(), label })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a `lookup` closure over a fixed set of PipeWire props.
    fn props(pairs: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn valid_source_maps_to_input_device() {
        let p = props(&[
            ("media.class", "Audio/Source"),
            ("node.name", "alsa_input.usb-Razer_Kiyo_Pro"),
            ("node.description", "Razer Kiyo Pro"),
        ]);
        let dev = map_input_device(|k| p.get(k).copied()).expect("should map");
        assert_eq!(dev.node_name, "alsa_input.usb-Razer_Kiyo_Pro");
        assert_eq!(dev.label, "Razer Kiyo Pro");
    }

    #[test]
    fn missing_node_name_is_skipped() {
        let p = props(&[("media.class", "Audio/Source")]);
        assert!(map_input_device(|k| p.get(k).copied()).is_none());
    }

    #[test]
    fn empty_node_name_is_skipped() {
        let p = props(&[("media.class", "Audio/Source"), ("node.name", "")]);
        assert!(map_input_device(|k| p.get(k).copied()).is_none());
    }

    #[test]
    fn non_source_is_skipped() {
        let p = props(&[
            ("media.class", "Audio/Sink"),
            ("node.name", "alsa_output.pci-0000_00_1f.3"),
        ]);
        assert!(map_input_device(|k| p.get(k).copied()).is_none());
    }

    #[test]
    fn sink_monitor_is_excluded() {
        let p = props(&[
            ("media.class", "Audio/Source"),
            ("node.name", "alsa_output.pci-0000_00_1f.3.monitor"),
            ("node.description", "Monitor of Built-in Audio"),
        ]);
        assert!(map_input_device(|k| p.get(k).copied()).is_none());
    }

    #[test]
    fn label_falls_back_to_node_name_when_no_description() {
        let p = props(&[
            ("media.class", "Audio/Source"),
            ("node.name", "my-source"),
        ]);
        let dev = map_input_device(|k| p.get(k).copied()).expect("should map");
        assert_eq!(dev.label, "my-source");
    }
}
