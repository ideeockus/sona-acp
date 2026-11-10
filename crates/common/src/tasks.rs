//! The contract of Task Manager.

use crate::{Alias, Caller, Result, TaskId, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    /// Waiting for a decision of the operator.
    NeedsOwner,
    Done,
    Cancelled,
}

impl TaskStatus {
    /// A closed task never changes again (D72).
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

/// What a task produced. Files live in the task's file space instead (D56).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub text: String,
    pub links: Vec<String>,
}

/// A task as its creator submits it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    pub context: String,
    pub parent_id: Option<TaskId>,
    pub assignee: Option<Alias>,
    pub members: Vec<Alias>,
    pub deadline: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub context: String,
    pub status: TaskStatus,
    pub creator: Alias,
    pub created_at: Timestamp,
    pub parent_id: Option<TaskId>,
    /// The one who moves the task forward.
    pub assignee: Option<Alias>,
    /// Others working on it.
    pub members: Vec<Alias>,
    pub deadline: Option<Timestamp>,
    pub result: Option<TaskResult>,
}

impl Task {
    /// Creator, assignee and members (D72).
    pub fn is_participant(&self, alias: &Alias) -> bool {
        &self.creator == alias
            || self.assignee.as_ref() == Some(alias)
            || self.members.contains(alias)
    }
}

/// One change to a task. At least one field must be set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub status: Option<TaskStatus>,
    pub result: Option<TaskResult>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Created,
    Status { from: TaskStatus, to: TaskStatus },
    Result { result: TaskResult },
    Note { text: String },
}

/// An append-only history record. Who and when are set by Task Manager.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at: Timestamp,
    pub who: Alias,
    pub change: Change,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    pub task: Task,
    pub history: Vec<HistoryEntry>,
}

/// What agents and the operator call. Exposed through the external API.
///
/// Tasks never wake anyone: messages do (D53).
#[async_trait]
pub trait TaskManager: Send + Sync {
    async fn create_task(&self, caller: &Caller, task: NewTask) -> Result<Task>;

    /// Participants and the operator only.
    async fn get_task(&self, caller: &Caller, id: TaskId) -> Result<TaskView>;

    /// Participants and the operator only. Closing is for the creator and the operator.
    async fn update_task(&self, caller: &Caller, id: TaskId, update: TaskUpdate) -> Result<Task>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Components hold each other as trait objects (D67).
    #[allow(dead_code)]
    struct DynCompatible(Arc<dyn TaskManager>);

    #[test]
    fn change_serializes_with_a_kind() {
        let change = Change::Status {
            from: TaskStatus::Open,
            to: TaskStatus::InProgress,
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["kind"], "status");
        assert_eq!(json["to"], "in_progress");
    }
}
