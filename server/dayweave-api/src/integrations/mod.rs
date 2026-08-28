//! Ports for services that live outside the `DayWeave` trust boundary.
//!
//! These traits are contracts, not fake integrations. Production adapters will
//! implement them only after OAuth and Codex App Server credentials are wired.

mod ports;

pub use ports::*;
