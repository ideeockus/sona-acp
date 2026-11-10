//! The contract of Agent Journal: what agents did, like auditd (D13).

use crate::{Alias, Caller, CorrelationId, Result, RunId, TaskId, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The component that reports an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Gateway,
    Runtime,
    Broker,
    TaskManager,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
}

/// What happened, as a component reports it (D42). Journal adds the time.
///
/// Tool arguments and results never go here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub actor: Alias,
    pub component: Component,
    /// A tool name or a Run lifecycle step, e.g. `bash` or `run_started`.
    pub action: String,
    pub decision: Option<Decision>,
    pub result: Option<Outcome>,
    pub reason: Option<String>,
    pub run_id: Option<RunId>,
    pub correlation_id: Option<CorrelationId>,
    pub task_id: Option<TaskId>,
}

impl Event {
    /// An event of `caller`, with its Run and chain if it has them.
    ///
    /// Set the remaining fields with struct update syntax:
    /// `Event { decision: Some(Decision::Deny), ..Event::new(caller, component, action) }`.
    pub fn new(caller: &Caller, component: Component, action: impl Into<String>) -> Self {
        let (run_id, correlation_id) = match caller {
            Caller::Agent {
                run_id,
                correlation_id,
                ..
            } => (Some(*run_id), Some(*correlation_id)),
            Caller::Operator { .. } | Caller::Adapter { .. } => (None, None),
        };
        Self {
            actor: caller.alias().clone(),
            component,
            action: action.into(),
            decision: None,
            result: None,
            reason: None,
            run_id,
            correlation_id,
            task_id: None,
        }
    }
}

/// One line of the journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub timestamp: Timestamp,
    #[serde(flatten)]
    pub event: Event,
}

/// Which entries to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "id", rename_all = "snake_case")]
pub enum JournalFilter {
    Chain(CorrelationId),
    Run(RunId),
    Task(TaskId),
}

impl JournalFilter {
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Self::Chain(id) => event.correlation_id == Some(*id),
            Self::Run(id) => event.run_id == Some(*id),
            Self::Task(id) => event.task_id == Some(*id),
        }
    }
}

/// What components call to report agent actions. Internal, never exposed through the API.
#[async_trait]
pub trait JournalWriter: Send + Sync {
    async fn record(&self, event: Event) -> Result<()>;
}

/// What the operator calls. Exposed through the external API.
#[async_trait]
pub trait JournalReader: Send + Sync {
    /// Matching entries, oldest first. Operator only.
    async fn read(&self, caller: &Caller, filter: JournalFilter) -> Result<Vec<JournalEntry>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Components hold each other as trait objects (D67).
    #[allow(dead_code)]
    struct DynCompatible(Arc<dyn JournalWriter>, Arc<dyn JournalReader>);

    fn agent() -> Caller {
        Caller::Agent {
            alias: "coder".parse().unwrap(),
            run_id: RunId::new(),
            correlation_id: CorrelationId::new(),
        }
    }

    #[test]
    fn entry_is_one_flat_line() {
        let entry = JournalEntry {
            timestamp: Timestamp::from_unix_millis(1_794_138_926_400).unwrap(),
            event: Event {
                decision: Some(Decision::Deny),
                reason: Some("not granted".to_owned()),
                ..Event::new(&agent(), Component::Gateway, "bash")
            },
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["timestamp"], "2026-11-08T11:55:26.4Z");
        assert_eq!(json["actor"], "coder");
        assert_eq!(json["component"], "gateway");
        assert_eq!(json["decision"], "deny");
        assert_eq!(serde_json::from_value::<JournalEntry>(json).unwrap(), entry);
    }

    #[test]
    fn filter_matches_by_chain_run_and_task() {
        let caller = agent();
        let Caller::Agent {
            run_id,
            correlation_id,
            ..
        } = &caller
        else {
            unreachable!()
        };
        let task_id = TaskId::new();
        let event = Event {
            task_id: Some(task_id),
            ..Event::new(&caller, Component::Runtime, "run_started")
        };
        assert!(JournalFilter::Chain(*correlation_id).matches(&event));
        assert!(JournalFilter::Run(*run_id).matches(&event));
        assert!(JournalFilter::Task(task_id).matches(&event));
        assert!(!JournalFilter::Task(TaskId::new()).matches(&event));
    }
}
