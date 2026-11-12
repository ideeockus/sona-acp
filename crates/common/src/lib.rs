//! Types shared by all control plane components.
//!
//! Components depend on this crate and never on each other (D29).

mod alias;
mod caller;
mod error;
mod ids;
mod session;
mod timestamp;

pub mod broker;
pub mod gateway;
pub mod journal;
pub mod layout;
pub mod registry;
pub mod tasks;

pub use alias::{Alias, InvalidAlias};
pub use caller::Caller;
pub use error::{Error, Result};
pub use ids::{CorrelationId, MessageId, RunId, TaskId};
pub use session::SessionKey;
pub use timestamp::Timestamp;
