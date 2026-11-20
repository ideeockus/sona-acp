//! Identifiers. All of them are UUID v7, so they sort by creation time (D34).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new identifier with the current timestamp.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse().map(Self)
            }
        }
    };
}

id!(
    /// One launch of Pi for an agent.
    RunId
);
id!(
    /// A task in Task Manager.
    TaskId
);
id!(
    /// A message in Message Broker.
    MessageId
);
id!(
    /// A chain of Runs causally linked by wake-ups.
    CorrelationId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_round_trip() {
        let id = RunId::new();
        assert_eq!(id.to_string().parse::<RunId>().unwrap(), id);
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let id = TaskId::new();
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{id}\""));
    }

    #[test]
    fn rejects_garbage() {
        assert!("not-a-uuid".parse::<MessageId>().is_err());
    }
}
