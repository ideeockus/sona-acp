//! The error returned by component calls.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A component call failure.
///
/// Serializable, so it survives moving a component into its own service (D11).
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Error {
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let error = Error::Forbidden("@coder may not message @ops".to_owned());
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "forbidden");
        assert_eq!(serde_json::from_value::<Error>(json).unwrap(), error);
    }
}
