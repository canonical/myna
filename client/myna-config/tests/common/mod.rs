//! Stand-in desktop services on a private session bus
//! ([`myna_freedesktop::testing`]), shared by the backends' tests: xfconfd
//! here, kglobalaccel in [`kglobalaccel`].
#![allow(dead_code)]

pub mod kglobalaccel;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gio::glib::{Variant, VariantTy};
use gio::prelude::*;
pub use myna_freedesktop::testing::*;

const XML: &str = "<node>\
  <interface name='org.xfce.Xfconf'>\
    <method name='GetProperty'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property' direction='in'/>\
      <arg type='v' name='value' direction='out'/>\
    </method>\
    <method name='SetProperty'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property' direction='in'/>\
      <arg type='v' name='value' direction='in'/>\
    </method>\
    <method name='GetAllProperties'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property_base' direction='in'/>\
      <arg type='a{sv}' name='properties' direction='out'/>\
    </method>\
    <method name='ResetProperty'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property' direction='in'/>\
      <arg type='b' name='recursive' direction='in'/>\
    </method>\
  </interface>\
</node>";
const NOT_FOUND: &str = "org.xfce.Xfconf.Error.PropertyNotFound";

type Store = Rc<RefCell<BTreeMap<String, Variant>>>;

/// Serve xfconf's methods on the bus at `address` until it goes.
pub fn serve(address: &str, ready: std::sync::mpsc::Sender<()>) {
    serve_as(address, "org.xfce.Xfconf", ready, |connection| {
        let store = Store::default();
        let interface = gio::DBusNodeInfo::for_xml(XML)
            .unwrap()
            .lookup_interface("org.xfce.Xfconf")
            .unwrap();
        let emitter = connection.clone();
        connection
            .register_object("/org/xfce/Xfconf", &interface)
            .method_call(move |_, _, _, _, method, parameters, invocation| {
                let channel = parameters
                    .child_value(0)
                    .str()
                    .unwrap_or_default()
                    .to_owned();
                let property = parameters
                    .child_value(1)
                    .str()
                    .unwrap_or_default()
                    .to_owned();
                let mut store = store.borrow_mut();
                let emit = |name: &str, body: Variant| {
                    emitter
                        .emit_signal(
                            None,
                            "/org/xfce/Xfconf",
                            "org.xfce.Xfconf",
                            name,
                            Some(&body),
                        )
                        .unwrap();
                };
                match method {
                    "GetProperty" => match store.get(&property) {
                        Some(value) => invocation.return_value(Some(&Variant::tuple_from_iter([
                            Variant::from_variant(value),
                        ]))),
                        None => invocation.return_dbus_error(NOT_FOUND, "no such property"),
                    },
                    "SetProperty" => {
                        let value = parameters.child_value(2).as_variant().unwrap();
                        store.insert(property.clone(), value.clone());
                        emit(
                            "PropertyChanged",
                            Variant::tuple_from_iter([
                                channel.to_variant(),
                                property.to_variant(),
                                Variant::from_variant(&value),
                            ]),
                        );
                        invocation.return_value(None);
                    }
                    "GetAllProperties" => {
                        let below = format!("{property}/");
                        let root = property.is_empty() || property == "/";
                        let found: Vec<Variant> = store
                            .iter()
                            .filter(|(path, _)| {
                                root || **path == property || path.starts_with(&below)
                            })
                            .map(|(path, value)| {
                                Variant::from_dict_entry(
                                    &path.to_variant(),
                                    &Variant::from_variant(value),
                                )
                            })
                            .collect();
                        if found.is_empty() && !root {
                            invocation.return_dbus_error(NOT_FOUND, "no such property");
                        } else {
                            let array = Variant::array_from_iter_with_type(
                                VariantTy::new("{sv}").unwrap(),
                                found,
                            );
                            invocation.return_value(Some(&Variant::tuple_from_iter([array])));
                        }
                    }
                    "ResetProperty" => {
                        let recursive = parameters.child_value(2).get::<bool>().unwrap();
                        let below = format!("{property}/");
                        let gone: Vec<String> = store
                            .keys()
                            .filter(|path| {
                                **path == property || (recursive && path.starts_with(&below))
                            })
                            .cloned()
                            .collect();
                        if gone.is_empty() {
                            invocation.return_dbus_error(NOT_FOUND, "no such property");
                            return;
                        }
                        for path in gone {
                            store.remove(&path);
                            emit(
                                "PropertyRemoved",
                                Variant::tuple_from_iter([channel.to_variant(), path.to_variant()]),
                            );
                        }
                        invocation.return_value(None);
                    }
                    other => unreachable!("{other}"),
                }
            })
            .build()
            .expect("register the stand-in xfconfd");
    });
}

/// The client end of a stand-in xfconfd on a fresh bus. The server thread
/// ends when the bus does.
pub fn connect() -> (gio::DBusConnection, Bus, std::thread::JoinHandle<()>) {
    connect_with(serve)
}
