//! The settings portal appearance backend against a stand-in
//! `xdg-desktop-portal` on a private session bus: the conformance suite, and
//! how it shares the answer with its fallback.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gio::glib::{Variant, VariantTy};
use gio::prelude::*;
use myna_freedesktop::portal::{AccentFrom, PortalAppearance, INTERFACE, NAMESPACE, PATH, SERVICE};
use myna_freedesktop::testing::*;
use myna_platform::appearance::{Appearance, AppearanceReadings, Freshness, Rgb};
use myna_platform::conformance::appearance::{run, Fixture};
use myna_platform::Subscription;

const XML: &str = "<node>\
  <interface name='org.freedesktop.portal.Settings'>\
    <method name='ReadAll'>\
      <arg type='as' name='namespaces' direction='in'/>\
      <arg type='a{sa{sv}}' name='value' direction='out'/>\
    </method>\
  </interface>\
  <interface name='org.example.PortalStandIn'>\
    <method name='Set'>\
      <arg type='s' name='key' direction='in'/>\
      <arg type='v' name='value' direction='in'/>\
    </method>\
  </interface>\
</node>";
const STAND_IN: &str = "org.example.PortalStandIn";

/// Serve the Settings interface, answering `namespace` with what `Set`
/// stored.
fn serve(namespace: &'static str) -> impl FnOnce(&str, std::sync::mpsc::Sender<()>) + Send {
    move |address, ready| {
        serve_as(address, SERVICE, ready, |connection| {
            let store = Rc::new(RefCell::new(BTreeMap::<String, Variant>::new()));
            let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
            for name in [INTERFACE, STAND_IN] {
                let store = store.clone();
                let emitter = connection.clone();
                connection
                    .register_object(PATH, &node.lookup_interface(name).unwrap())
                    .method_call(move |_, _, _, _, method, parameters, invocation| {
                        let mut store = store.borrow_mut();
                        match method {
                            "ReadAll" => {
                                let settings = gio::glib::VariantDict::new(None);
                                for (key, value) in store.iter() {
                                    settings.insert_value(key, value);
                                }
                                let answered = Variant::array_from_iter_with_type(
                                    VariantTy::new("{sa{sv}}").unwrap(),
                                    [Variant::from_dict_entry(
                                        &namespace.to_variant(),
                                        &settings.end(),
                                    )],
                                );
                                invocation
                                    .return_value(Some(&Variant::tuple_from_iter([answered])));
                            }
                            "Set" => {
                                let key = parameters.child_value(0).str().unwrap().to_owned();
                                let value = parameters.child_value(1).as_variant().unwrap();
                                store.insert(key.clone(), value.clone());
                                emitter
                                    .emit_signal(
                                        None,
                                        PATH,
                                        INTERFACE,
                                        "SettingChanged",
                                        Some(&Variant::tuple_from_iter([
                                            namespace.to_variant(),
                                            key.to_variant(),
                                            Variant::from_variant(&value),
                                        ])),
                                    )
                                    .unwrap();
                                invocation.return_value(None);
                            }
                            other => unreachable!("{other}"),
                        }
                    })
                    .build()
                    .expect("register the stand-in portal");
            }
        });
    }
}

fn set(connection: &gio::DBusConnection, key: &str, value: Variant) {
    connection
        .call_sync(
            Some(SERVICE),
            PATH,
            STAND_IN,
            "Set",
            Some(&(key, value).to_variant()),
            Some(VariantTy::UNIT),
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
}

type Watchers = Rc<RefCell<Vec<Rc<dyn Fn(Freshness)>>>>;

/// A fallback whose readings the test sets and whose watchers it calls.
#[derive(Clone, Default)]
struct Toolkit {
    readings: Rc<Cell<AppearanceReadings>>,
    watchers: Watchers,
}

impl Toolkit {
    fn change(&self, readings: AppearanceReadings) {
        self.readings.set(readings);
        let watchers = self.watchers.borrow().clone();
        for watcher in watchers {
            watcher(Freshness::NextFrame);
        }
    }
}

impl Appearance for Toolkit {
    fn read(&self) -> AppearanceReadings {
        self.readings.get()
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: Rc<dyn Fn(Freshness)> = changed.into();
        self.watchers.borrow_mut().push(changed.clone());
        let watchers = Rc::downgrade(&self.watchers);
        Subscription::new(move || {
            if let Some(watchers) = watchers.upgrade() {
                watchers
                    .borrow_mut()
                    .retain(|watcher| !Rc::ptr_eq(watcher, &changed));
            }
        })
    }
}

struct Rig {
    connection: gio::DBusConnection,
    _bus: Bus,
}

impl Rig {
    fn new() -> Self {
        let (connection, bus, _thread) = connect_with(serve(NAMESPACE));
        Self {
            connection,
            _bus: bus,
        }
    }

    fn open(&self, fallback: Toolkit, accent: AccentFrom) -> Rc<dyn Appearance> {
        PortalAppearance::open(&self.connection, Rc::new(fallback), accent)
    }

    fn set(&self, key: &str, value: u32) {
        set(&self.connection, key, value.to_variant());
    }
}

impl Fixture for Rig {
    fn setup(&mut self) -> Box<dyn Appearance> {
        for key in ["color-scheme", "contrast", "reduced-motion"] {
            self.set(key, 0);
        }
        settle();
        let appearance = self.open(Toolkit::default(), AccentFrom::Portal);
        Box::new(Shared(appearance))
    }

    fn set_reduced_motion(&mut self, on: bool) -> bool {
        self.set("reduced-motion", on as u32);
        true
    }

    fn set_high_contrast(&mut self, on: bool) -> bool {
        self.set("contrast", on as u32);
        true
    }

    fn set_prefers_dark(&mut self, on: bool) -> bool {
        self.set("color-scheme", if on { 1 } else { 2 });
        true
    }

    fn settle(&mut self) {
        settle();
    }
}

/// The suite takes a box; the backend comes as an `Rc`.
struct Shared(Rc<dyn Appearance>);

impl Appearance for Shared {
    fn read(&self) -> AppearanceReadings {
        self.0.read()
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        self.0.watch(changed)
    }
}

const ORANGE: Rgb = Rgb {
    r: 0.914,
    g: 0.329,
    b: 0.125,
};
const BREEZE: (f64, f64, f64) = (0.239, 0.682, 0.914);

#[test]
fn the_portal_backend_conforms() {
    on_own_context(|| {
        let report = run(&mut Rig::new());
        assert!(report.not_applicable.is_empty(), "{report:?}");
    });
}

#[test]
fn without_a_portal_the_fallback_answers_alone() {
    on_own_context(|| {
        let (bus, address) = bus();
        let connection = connect_to(&address);
        let toolkit = Toolkit::default();
        let dark = AppearanceReadings {
            prefers_dark: true,
            accent: Some(ORANGE),
            ..Default::default()
        };
        toolkit.change(dark);
        let appearance =
            PortalAppearance::open(&connection, Rc::new(toolkit.clone()), AccentFrom::Portal);
        assert_eq!(appearance.read(), dark);
        drop(bus);
    });
}

#[test]
fn a_portal_without_the_namespace_leaves_the_fallback_alone() {
    on_own_context(|| {
        let (connection, _bus, _thread) = connect_with(serve("org.gnome.desktop.interface"));
        set(&connection, "gtk-theme", "Adwaita".to_variant());
        let toolkit = Toolkit::default();
        toolkit.change(AppearanceReadings {
            reduced_motion: true,
            ..Default::default()
        });
        let appearance =
            PortalAppearance::open(&connection, Rc::new(toolkit.clone()), AccentFrom::Portal);
        assert!(appearance.read().reduced_motion);
    });
}

#[test]
fn a_stated_preference_outranks_the_fallback_and_no_preference_defers_to_it() {
    on_own_context(|| {
        let rig = Rig::new();
        let toolkit = Toolkit::default();
        toolkit.change(AppearanceReadings {
            prefers_dark: true,
            reduced_motion: true,
            high_contrast: true,
            accent: None,
        });
        rig.set("color-scheme", 2);
        let appearance = rig.open(toolkit.clone(), AccentFrom::Portal);
        let readings = appearance.read();
        assert!(!readings.prefers_dark, "prefer-light lost to a dark theme");
        assert!(readings.reduced_motion && readings.high_contrast);
        rig.set("color-scheme", 0);
        settle();
        assert!(
            appearance.read().prefers_dark,
            "no preference hid the theme"
        );
    });
}

#[test]
fn the_accent_comes_from_whichever_side_the_caller_names() {
    on_own_context(|| {
        let rig = Rig::new();
        set(&rig.connection, "accent-color", BREEZE.to_variant());
        let toolkit = Toolkit::default();
        let breeze = Rgb {
            r: BREEZE.0,
            g: BREEZE.1,
            b: BREEZE.2,
        };
        let portal = rig.open(toolkit.clone(), AccentFrom::Portal);
        let fallback = rig.open(toolkit.clone(), AccentFrom::Fallback);
        assert_eq!(portal.read().accent, Some(breeze));
        assert_eq!(fallback.read().accent, Some(breeze), "none of its own");
        toolkit.change(AppearanceReadings {
            accent: Some(ORANGE),
            ..Default::default()
        });
        assert_eq!(portal.read().accent, Some(breeze));
        assert_eq!(fallback.read().accent, Some(ORANGE));
        set(
            &rig.connection,
            "accent-color",
            (-1.0, -1.0, -1.0).to_variant(),
        );
        settle();
        assert_eq!(portal.read().accent, Some(ORANGE), "an unset accent");
    });
}

#[test]
fn a_watch_hears_both_sides_as_each_reports_until_dropped() {
    on_own_context(|| {
        let rig = Rig::new();
        let toolkit = Toolkit::default();
        let appearance = rig.open(toolkit.clone(), AccentFrom::Portal);
        let heard = Rc::new(RefCell::new(Vec::new()));
        let subscription = appearance.watch(Box::new({
            let heard = heard.clone();
            let appearance = appearance.clone();
            move |freshness| {
                heard
                    .borrow_mut()
                    .push((freshness, appearance.read().reduced_motion))
            }
        }));
        rig.set("reduced-motion", 1);
        settle();
        toolkit.change(AppearanceReadings::default());
        assert_eq!(
            *heard.borrow(),
            [(Freshness::Current, true), (Freshness::NextFrame, true)]
        );
        drop(subscription);
        rig.set("reduced-motion", 0);
        settle();
        toolkit.change(AppearanceReadings::default());
        assert_eq!(heard.borrow().len(), 2, "heard after the drop");
        assert!(
            !appearance.read().reduced_motion,
            "read stale once unwatched"
        );
    });
}
