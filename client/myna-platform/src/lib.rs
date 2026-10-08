//! The contracts between Myna and the desktop it runs on.
//!
//! Each module is one desktop-neutral operation set; each desktop supplies a
//! backend for it in the process that runs it. Callers query capabilities
//! rather than assuming them, and decide policy from the answer. This crate
//! holds no backend and binds no toolkit or desktop service.

pub mod activation;
pub mod appearance;
pub mod components;
pub mod session;
pub mod status_surface;
pub mod subscription;
pub mod text_input;

pub use session::{Desktop, Profile, Session, SessionEnv, SessionKind, UnknownProfile};
pub use subscription::Subscription;
