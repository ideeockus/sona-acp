//! Agent Runtime: starts Pi for a Run and talks to it over its RPC protocol (D7, D58).
//!
//! An agent has at most one Run at a time (D30). When it has unread messages, Runtime
//! takes the session of the oldest one, starts Pi for it and delivers newer messages of
//! that session by `steer` until Pi settles (D12, D57).

mod context;
pub mod launcher;
pub mod rpc;
mod run;
mod runtime;

pub use runtime::{Runtime, RuntimeConfig};
