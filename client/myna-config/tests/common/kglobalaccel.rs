//! A stand-in kglobalaccel: the slice of `org.kde.KGlobalAccel` Settings and
//! Plasma's Shortcuts settings use, with the daemon's quirks that matter to a
//! client (kglobalacceld 6.6):
//!
//! - registering a desktop entry's component gives it a grabbed `_launch`;
//!   registering `_launch` itself in the same call adds a second, ungrabbed
//!   one in its place, so the key never fires;
//! - a key another action holds is dropped from a new key list;
//! - a key sequence is four ints; KWin aborts on fewer.
//!
//! What the stand-in holds is shared with the test through [`State`].

use std::sync::{Arc, Mutex};

use gio::glib::{Variant, VariantTy};
use gio::prelude::*;

const XML: &str = "<node>\
  <interface name='org.kde.KGlobalAccel'>\
    <method name='allMainComponents'><arg type='aas' direction='out'/></method>\
    <method name='doRegister'><arg type='as' direction='in'/></method>\
    <method name='getComponent'>\
      <arg type='s' direction='in'/><arg type='o' direction='out'/>\
    </method>\
    <method name='unregister'>\
      <arg type='s' direction='in'/><arg type='s' direction='in'/>\
      <arg type='b' direction='out'/>\
    </method>\
    <method name='shortcutKeys'>\
      <arg type='as' direction='in'/><arg type='a(ai)' direction='out'/>\
    </method>\
    <method name='setForeignShortcutKeys'>\
      <arg type='as' direction='in'/><arg type='a(ai)' direction='in'/>\
    </method>\
    <method name='globalShortcutsByKey'>\
      <arg type='(ai)' direction='in'/><arg type='(i)' direction='in'/>\
      <arg type='a(ssssssaiai)' direction='out'/>\
    </method>\
    <signal name='yourShortcutsChanged'>\
      <arg type='as'/><arg type='a(ai)'/>\
    </signal>\
  </interface>\
</node>";
const INTERFACE: &str = "org.kde.KGlobalAccel";

#[derive(Clone, Debug, Default)]
pub struct Action {
    pub name: String,
    pub friendly: String,
    pub keys: Vec<i32>,
    /// Grabbed: its keys fire.
    pub present: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Component {
    pub name: String,
    pub friendly: String,
    pub actions: Vec<Action>,
}

#[derive(Debug, Default)]
pub struct State {
    pub components: Vec<Component>,
    /// How many registrations of a new desktop entry's component go unseen,
    /// as while KSycoca has not looked at the disk since the entry was written.
    pub unseen: u32,
}

impl State {
    pub fn action(&self, component: &str, action: &str) -> Option<&Action> {
        self.components
            .iter()
            .find(|c| c.name == component)?
            .actions
            .iter()
            .find(|a| a.name == action)
    }

    fn action_mut(&mut self, component: &str, action: &str) -> Option<&mut Action> {
        self.components
            .iter_mut()
            .find(|c| c.name == component)?
            .actions
            .iter_mut()
            .find(|a| a.name == action)
    }

    fn holder(&self, key: i32) -> Option<(&Component, &Action)> {
        self.components.iter().find_map(|component| {
            component
                .actions
                .iter()
                .find(|action| action.keys.contains(&key))
                .map(|action| (component, action))
        })
    }

    fn register(&mut self, id: &[String]) {
        let [component, action, component_friendly, action_friendly] = id else {
            return;
        };
        if self.action(component, action).is_some() {
            return;
        }
        if !self.components.iter().any(|c| &c.name == component) {
            if component.ends_with(".desktop") && self.unseen > 0 {
                self.unseen -= 1;
                return;
            }
            let mut new = Component {
                name: component.clone(),
                friendly: component_friendly.clone(),
                actions: Vec::new(),
            };
            if component.ends_with(".desktop") {
                new.actions.push(Action {
                    name: "_launch".into(),
                    friendly: component_friendly.clone(),
                    keys: Vec::new(),
                    present: true,
                });
            }
            self.components.push(new);
        }
        let actions = &mut self
            .components
            .iter_mut()
            .find(|c| &c.name == component)
            .unwrap()
            .actions;
        actions.retain(|a| &a.name != action);
        actions.push(Action {
            name: action.clone(),
            friendly: action_friendly.clone(),
            keys: Vec::new(),
            present: false,
        });
    }
}

fn sequences(keys: &[i32]) -> Variant {
    Variant::array_from_iter_with_type(
        VariantTy::new("(ai)").unwrap(),
        keys.iter()
            .map(|key| Variant::tuple_from_iter([[*key, 0, 0, 0].to_variant()])),
    )
}

/// The first int of each sequence, or why KWin would abort.
fn keys_of(value: &Variant) -> Result<Vec<i32>, String> {
    value
        .iter()
        .map(|sequence| {
            let ints = sequence.child_value(0).get::<Vec<i32>>().unwrap();
            match ints.as_slice() {
                [key, 0, 0, 0] => Ok(*key),
                _ => Err(format!("KWin aborts on the key sequence {ints:?}")),
            }
        })
        .filter(|key| key != &Ok(0))
        .collect()
}

/// Serve kglobalaccel on the bus at `address` until it goes.
pub fn serve(address: &str, ready: std::sync::mpsc::Sender<()>, state: Arc<Mutex<State>>) {
    super::serve_as(address, "org.kde.kglobalaccel", ready, |connection| {
        let interface = gio::DBusNodeInfo::for_xml(XML)
            .unwrap()
            .lookup_interface(INTERFACE)
            .unwrap();
        let emitter = connection.clone();
        connection
            .register_object("/kglobalaccel", &interface)
            .method_call(move |_, _, _, _, method, parameters, invocation| {
                let mut state = state.lock().unwrap();
                let id = || parameters.child_value(0).get::<Vec<String>>().unwrap();
                match method {
                    "allMainComponents" => {
                        let names: Vec<Vec<String>> = state
                            .components
                            .iter()
                            .map(|c| {
                                vec![
                                    c.name.clone(),
                                    c.friendly.clone(),
                                    String::new(),
                                    String::new(),
                                ]
                            })
                            .collect();
                        invocation.return_value(Some(&(names,).to_variant()));
                    }
                    "doRegister" => {
                        state.register(&id());
                        invocation.return_value(None);
                    }
                    "getComponent" => {
                        let (name,) = parameters.get::<(String,)>().unwrap();
                        if state.components.iter().any(|c| c.name == name) {
                            let path = format!("/component/{}", name.replace(['.', '-', ' '], "_"));
                            let path = gio::glib::variant::ObjectPath::try_from(path).unwrap();
                            invocation.return_value(Some(&(path,).to_variant()));
                        } else {
                            invocation.return_dbus_error(
                                "org.kde.kglobalaccel.NoSuchComponent",
                                &format!("The component '{name}' doesn't exist."),
                            );
                        }
                    }
                    "unregister" => {
                        let (component, action) = parameters.get::<(String, String)>().unwrap();
                        let found = state
                            .components
                            .iter_mut()
                            .find(|c| c.name == component)
                            .is_some_and(|c| {
                                let before = c.actions.len();
                                c.actions.retain(|a| a.name != action);
                                c.actions.len() != before
                            });
                        invocation.return_value(Some(&(found,).to_variant()));
                    }
                    "shortcutKeys" => {
                        let id = id();
                        let keys = state
                            .action(&id[0], &id[1])
                            .map(|a| a.keys.clone())
                            .unwrap_or_default();
                        invocation
                            .return_value(Some(&Variant::tuple_from_iter([sequences(&keys)])));
                    }
                    "setForeignShortcutKeys" => {
                        let id = id();
                        let wanted = match keys_of(&parameters.child_value(1)) {
                            Ok(keys) => keys,
                            Err(abort) => {
                                return invocation.return_dbus_error("org.example.Abort", &abort)
                            }
                        };
                        if state.action(&id[0], &id[1]).is_none() {
                            return invocation.return_value(None);
                        }
                        if let Some(action) = state.action_mut(&id[0], &id[1]) {
                            action.keys.clear();
                        }
                        let free: Vec<i32> = wanted
                            .into_iter()
                            .filter(|key| state.holder(*key).is_none())
                            .collect();
                        state.action_mut(&id[0], &id[1]).unwrap().keys = free.clone();
                        drop(state);
                        emitter
                            .emit_signal(
                                None,
                                "/kglobalaccel",
                                INTERFACE,
                                "yourShortcutsChanged",
                                Some(&Variant::tuple_from_iter([
                                    id.to_variant(),
                                    sequences(&free),
                                ])),
                            )
                            .unwrap();
                        invocation.return_value(None);
                    }
                    "globalShortcutsByKey" => {
                        let key = match keys_of(&Variant::array_from_iter_with_type(
                            VariantTy::new("(ai)").unwrap(),
                            [parameters.child_value(0)],
                        )) {
                            Ok(keys) => keys.first().copied().unwrap_or(0),
                            Err(abort) => {
                                return invocation.return_dbus_error("org.example.Abort", &abort)
                            }
                        };
                        // Like kglobalaccel, the first component with a match.
                        let found: Vec<Variant> = state
                            .components
                            .iter()
                            .map(|c| {
                                (
                                    c,
                                    c.actions
                                        .iter()
                                        .filter(|a| a.keys.contains(&key))
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .find(|(_, actions)| !actions.is_empty())
                            .map(|(c, actions)| {
                                actions
                                    .into_iter()
                                    .map(|a| {
                                        (
                                            a.name.as_str(),
                                            a.friendly.as_str(),
                                            c.name.as_str(),
                                            c.friendly.as_str(),
                                            "default",
                                            "Default Context",
                                            a.keys.clone(),
                                            Vec::<i32>::new(),
                                        )
                                            .to_variant()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let array = Variant::array_from_iter_with_type(
                            VariantTy::new("(ssssssaiai)").unwrap(),
                            found,
                        );
                        invocation.return_value(Some(&Variant::tuple_from_iter([array])));
                    }
                    other => unreachable!("{other}"),
                }
            })
            .build()
            .expect("register the stand-in kglobalaccel");
    });
}

/// The client end of a stand-in kglobalaccel on a fresh bus, and what it
/// holds.
pub fn connect() -> (
    gio::DBusConnection,
    super::Bus,
    std::thread::JoinHandle<()>,
    Arc<Mutex<State>>,
) {
    let state = Arc::new(Mutex::new(State::default()));
    let (connection, bus, thread) = super::connect_with({
        let state = Arc::clone(&state);
        move |address, ready| serve(address, ready, state)
    });
    (connection, bus, thread, state)
}
