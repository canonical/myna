//! Notification replacement over a private bus, including a server refusing
//! a duplicate update as Plasma does. No desktop notification server is used.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use myna_desktop::indicator::notify::NotifyIndicator;
use myna_desktop::indicator::{Indicator, IndicatorState};
use zbus::zvariant::OwnedValue;

struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    fn spawn() -> Self {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn a private dbus-daemon (install dbus-daemon, or run in the workshop)");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().expect("dbus-daemon stdout"))
            .read_line(&mut address)
            .expect("read the private bus address");
        let address = address.trim().to_owned();
        assert!(!address.is_empty(), "dbus-daemon printed no address");
        Self { daemon, address }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[derive(Default)]
struct NotificationState {
    next_id: u32,
    active_ids: BTreeSet<u32>,
    replacement_ids: Vec<u32>,
    rejected_updates: usize,
    closed_ids: Vec<u32>,
    last_text: Option<(String, String)>,
}

struct NotificationServer(Arc<Mutex<NotificationState>>);

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.Notifications.Error")]
enum NotificationError {
    ExcessNotificationGeneration(String),
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationServer {
    #[expect(
        clippy::too_many_arguments,
        reason = "the notification protocol has eight arguments"
    )]
    fn notify(
        &mut self,
        _app_name: &str,
        replaces_id: u32,
        _app_icon: &str,
        summary: &str,
        body: &str,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> Result<u32, NotificationError> {
        let mut state = self.0.lock().unwrap();
        state.replacement_ids.push(replaces_id);
        let text = (summary.to_owned(), body.to_owned());
        // Reject duplicates like Plasma, without a clock-dependent test.
        if state.last_text.as_ref() == Some(&text) {
            state.rejected_updates += 1;
            return Err(NotificationError::ExcessNotificationGeneration(
                "Created too many similar notifications in quick succession".into(),
            ));
        }
        let id = if replaces_id == 0 {
            state.next_id += 1;
            state.next_id
        } else {
            replaces_id
        };
        state.active_ids.insert(id);
        state.last_text = Some(text);
        Ok(id)
    }

    fn close_notification(&mut self, id: u32) {
        let mut state = self.0.lock().unwrap();
        state.closed_ids.push(id);
        state.active_ids.remove(&id);
    }
}

#[test]
fn a_rejected_update_keeps_the_live_notification_id() {
    let bus = PrivateBus::spawn();
    // This integration binary has one test. Set the address before creating
    // runtime threads, so notify-rust cannot connect to the user's session.
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address);
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    runtime.block_on(async {
        let state = Arc::new(Mutex::new(NotificationState::default()));
        let _server = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at(
                "/org/freedesktop/Notifications",
                NotificationServer(state.clone()),
            )
            .unwrap()
            .build()
            .await
            .expect("serve notifications on the private bus");

        let mut indicator = NotifyIndicator::new();
        indicator.set_state(IndicatorState::Recording).await;
        {
            let state = state.lock().unwrap();
            assert_eq!(state.replacement_ids, [0]);
            assert_eq!(state.active_ids, BTreeSet::from([1]));
        }

        // Loading/Ready events can repeat Recording immediately after start.
        indicator.set_state(IndicatorState::Recording).await;
        {
            let state = state.lock().unwrap();
            assert_eq!(state.replacement_ids, [0, 1]);
            assert_eq!(
                state.rejected_updates, 1,
                "the server must reject the update"
            );
            assert_eq!(state.active_ids, BTreeSet::from([1]));
        }

        indicator.set_state(IndicatorState::Finalizing).await;
        {
            let state = state.lock().unwrap();
            assert_eq!(
                state.replacement_ids,
                [0, 1, 1],
                "a rejected update must retain the live ID"
            );
            assert_eq!(
                state.active_ids,
                BTreeSet::from([1]),
                "an update must not stack a second toast"
            );
        }

        indicator.set_state(IndicatorState::Hidden).await;
        let state = state.lock().unwrap();
        assert_eq!(state.closed_ids, [1]);
        assert!(
            state.active_ids.is_empty(),
            "Hidden must leave no orphaned toast"
        );
    });
}
