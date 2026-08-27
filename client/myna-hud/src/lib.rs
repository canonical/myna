//! `myna-hud` — the dictation indicator renderer application (feature 004,
//! 2026-08-26 architecture revision; research R21–R27).
//!
//! The HUD pill and its GPU wave ribbon are drawn by this standalone GTK4 +
//! libadwaita application. On GNOME the `myna-shell` extension hosts its
//! window as a focus-safe overlay (dock-typed, window-list-hidden,
//! click-through — it launches and positions us; we never position or
//! type ourselves). Elsewhere the same binary is the development lab
//! (`--lab`) and the backend simulator (`--serve-dbus`).
//!
//! Module layout (plan.md Project Structure):
//! - **Pure, std-only, unit-tested** (ported 1:1 from the former GJS
//!   extension modules — their GJS test suites are the ported assertions):
//!   [`states`] (wire state → descriptor; port of states.js),
//!   [`vumeter`] (calibrated envelope; port of vumeter.js),
//!   [`ribbon`] (strand model + phase machine; port of ribbon.js),
//!   [`shader`] (GLSL generator + uniform packing; port of ribbonGlsl.js —
//!   the Cairo painter is deliberately NOT ported, GPU-only per R23),
//!   [`hud_logic`] (icon/phase/color/notice rules; port of hudLogic.js),
//!   [`input_region`] (per-state click-through geometry, new R22),
//!   [`accent`] (accent-color resolution rules, R26),
//!   [`motion`] (reduced-motion resolution, absent-safe, R26/E2b),
//!   [`simulator`] (lab-controls ↔ wire-state mapping).
//! - **Application half** (Phase C): the window/pill UI, GLArea renderer,
//!   D-Bus consumer, lab and simulator modes.
//!
//! Privacy invariant (constitution V): state + level only — nothing here
//! ever sees, renders, logs, or persists transcript content; no network.

pub mod accent;
pub mod bus;
pub mod dbus_consumer;
pub mod gl;
pub mod hud_logic;
pub mod input_region;
pub mod lab;
pub mod motion;
pub mod notice_slot;
pub mod platform;
pub mod ribbon;
pub mod serve;
pub mod session_control;
pub mod shader;
pub mod simulator;
pub mod states;
pub mod vumeter;
pub mod window;
