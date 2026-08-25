//! Meld's deterministic reliability core.
//!
//! Workers can execute tasks and propose results. Only [`Supervisor`] can
//! mutate authoritative task state or accept a verified result.

pub mod api;
pub mod domain;
pub mod events;
#[cfg(feature = "rig-worker")]
pub mod rig_worker;
pub mod supervisor;
pub mod verifier;
pub mod worker;

pub use supervisor::{AppState, Supervisor};
