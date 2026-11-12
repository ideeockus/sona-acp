//! Which Pi session a message belongs to.

use crate::{CorrelationId, TaskId};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// An agent's session: its task, or its chain for messages without a task (D14, D78).
///
/// Written as `task-<uuid>` or `chain-<uuid>`, safe for keys and file names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SessionKey {
    Task(TaskId),
    Chain(CorrelationId),
}

impl SessionKey {
    /// The session of a message with this task and chain.
    pub fn of(task_id: Option<TaskId>, correlation_id: CorrelationId) -> Self {
        task_id.map_or(Self::Chain(correlation_id), Self::Task)
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task(id) => write!(f, "task-{id}"),
            Self::Chain(id) => write!(f, "chain-{id}"),
        }
    }
}

impl FromStr for SessionKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid session key {s:?}");
        if let Some(id) = s.strip_prefix("task-") {
            id.parse().map(Self::Task).map_err(|_| invalid())
        } else if let Some(id) = s.strip_prefix("chain-") {
            id.parse().map(Self::Chain).map_err(|_| invalid())
        } else {
            Err(invalid())
        }
    }
}

impl TryFrom<String> for SessionKey {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<SessionKey> for String {
    fn from(key: SessionKey) -> Self {
        key.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_wins_over_chain() {
        let (task, chain) = (TaskId::new(), CorrelationId::new());
        assert_eq!(SessionKey::of(Some(task), chain), SessionKey::Task(task));
        assert_eq!(SessionKey::of(None, chain), SessionKey::Chain(chain));
    }

    #[test]
    fn text_form_round_trips() {
        for key in [
            SessionKey::Task(TaskId::new()),
            SessionKey::Chain(CorrelationId::new()),
        ] {
            assert_eq!(key.to_string().parse::<SessionKey>().unwrap(), key);
            let json = serde_json::to_string(&key).unwrap();
            assert_eq!(serde_json::from_str::<SessionKey>(&json).unwrap(), key);
        }
        assert!("job-1".parse::<SessionKey>().is_err());
    }
}
