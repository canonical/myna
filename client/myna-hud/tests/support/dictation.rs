//! The publisher the HUD consumes, reduced to what it reads.

use myna_hud::dbus_consumer::{BUS_NAME, OBJECT_PATH};

struct Dictation {
    state: String,
    message: String,
}

#[zbus::interface(name = "com.canonical.Myna.Dictation")]
impl Dictation {
    #[zbus(property)]
    fn state(&self) -> String {
        self.state.clone()
    }
    #[zbus(property)]
    fn status_message(&self) -> String {
        self.message.clone()
    }
    #[zbus(property)]
    fn audio_rms(&self) -> f64 {
        0.0
    }
    #[zbus(property)]
    fn audio_peak(&self) -> f64 {
        0.0
    }
    #[zbus(property)]
    fn hud_style(&self) -> String {
        "bar".into()
    }
    fn register_client(&self) -> u32 {
        1
    }
}

/// `com.canonical.Myna.Dictation` on the bus at `address`, in `state`.
pub fn serve(address: &str, state: &str) -> zbus::blocking::Connection {
    zbus::blocking::connection::Builder::address(address)
        .unwrap()
        .name(BUS_NAME)
        .unwrap()
        .serve_at(
            OBJECT_PATH,
            Dictation {
                state: state.into(),
                message: String::new(),
            },
        )
        .unwrap()
        .build()
        .expect("publish com.canonical.Myna.Dictation")
}

/// Publish a new state and status message.
pub fn publish(connection: &zbus::blocking::Connection, state: &str, message: &str) {
    let iface = connection
        .object_server()
        .interface::<_, Dictation>(OBJECT_PATH)
        .expect("served interface");
    {
        let mut dictation = iface.get_mut();
        dictation.state = state.into();
        dictation.message = message.into();
    }
    let emitter = iface.signal_emitter();
    let dictation = iface.get();
    zbus::block_on(dictation.status_message_changed(emitter)).expect("emit StatusMessage");
    zbus::block_on(dictation.state_changed(emitter)).expect("emit State");
}
