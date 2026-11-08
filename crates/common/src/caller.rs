//! Who is making a call.

use crate::{Alias, CorrelationId, RunId};
use serde::{Deserialize, Serialize};

/// An authenticated caller, passed explicitly to every component call (D11).
///
/// It is derived from a verified credential and never from data sent by the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caller {
    /// The human running the system.
    Operator { alias: Alias },
    /// An external adapter or the scheduler, sending as its own alias (D35).
    Adapter { alias: Alias },
    /// An agent acting within a Run.
    Agent {
        alias: Alias,
        run_id: RunId,
        correlation_id: CorrelationId,
    },
}

impl Caller {
    pub fn alias(&self) -> &Alias {
        match self {
            Self::Operator { alias } | Self::Adapter { alias } | Self::Agent { alias, .. } => alias,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_form_round_trips() {
        let caller = Caller::Agent {
            alias: "coder".parse().unwrap(),
            run_id: RunId::new(),
            correlation_id: CorrelationId::new(),
        };
        let json = serde_json::to_value(&caller).unwrap();
        assert_eq!(json["kind"], "agent");
        assert_eq!(json["alias"], "coder");
        assert_eq!(serde_json::from_value::<Caller>(json).unwrap(), caller);
    }

    #[test]
    fn alias_is_available_for_every_kind() {
        let alias: Alias = "radmir".parse().unwrap();
        let caller = Caller::Operator {
            alias: alias.clone(),
        };
        assert_eq!(caller.alias(), &alias);
    }
}
