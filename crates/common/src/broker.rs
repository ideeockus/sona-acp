//! The contract of Message Broker.

use crate::{Alias, Caller, CorrelationId, MessageId, Result, TaskId, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A message as the sender submits it.
///
/// Separate from [`Message`] on purpose: `id`, `from`, `correlation_id` and `created_at`
/// are set by Broker (D41), so a sender cannot even express them. A single struct would
/// leave spoofing protection to Broker remembering to overwrite those fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMessage {
    pub to: Alias,
    pub body: String,
    pub subject: Option<String>,
    pub task_id: Option<TaskId>,
    pub in_reply_to: Option<MessageId>,
}

/// A stored message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub from: Alias,
    pub to: Alias,
    pub body: String,
    pub subject: Option<String>,
    pub task_id: Option<TaskId>,
    pub in_reply_to: Option<MessageId>,
    pub correlation_id: CorrelationId,
    pub created_at: Timestamp,
}

/// Unread messages of one session, handed to one Run (D57).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxBatch {
    /// The session to run in. `None` means messages without a task (O8).
    pub task_id: Option<TaskId>,
    /// Oldest first, never empty.
    pub messages: Vec<Message>,
}

/// What senders and readers call. Exposed through the external API.
#[async_trait]
pub trait MessageBroker: Send + Sync {
    /// Stores a message from `caller` and wakes the recipient.
    ///
    /// `correlation_id` comes from the caller's Run. For a caller without a Run
    /// it is inherited through `in_reply_to`, otherwise a new chain starts (D41).
    async fn send(&self, caller: &Caller, message: NewMessage) -> Result<Message>;

    /// Unread messages of `caller` for one session, oldest first.
    ///
    /// `task_id: None` selects messages without a task. The cursor does not move (D49).
    async fn read_messages(&self, caller: &Caller, task_id: Option<TaskId>)
    -> Result<Vec<Message>>;
}

/// What Agent Runtime calls to drive delivery. Internal, never exposed through the API.
#[async_trait]
pub trait InboxDelivery: Send + Sync {
    /// Agents that have unread messages. Used to reconcile on startup (D15).
    async fn agents_with_unread(&self) -> Result<Vec<Alias>>;

    /// All unread messages of the session holding the agent's oldest unread message.
    // Deliberately simple for one active Run per agent; revisit with parallel Runs (D57).
    async fn next_batch(&self, agent: &Alias) -> Result<Option<InboxBatch>>;

    /// Marks a session's messages up to and including `up_to` as read (D49).
    ///
    /// The cursor follows delivery order and never moves back.
    async fn advance_cursor(
        &self,
        agent: &Alias,
        task_id: Option<TaskId>,
        up_to: MessageId,
    ) -> Result<()>;
}

/// How Broker reports that an agent has something to read.
///
/// Implemented by Agent Runtime, which decides between a new Run and `steer` (D12).
/// A hint only: the inbox stays the source of truth (D15).
#[async_trait]
pub trait WakeSink: Send + Sync {
    async fn wake(&self, agent: &Alias);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Components hold each other as trait objects (D67).
    #[allow(dead_code)]
    struct DynCompatible(
        Arc<dyn MessageBroker>,
        Arc<dyn InboxDelivery>,
        Arc<dyn WakeSink>,
    );

    #[test]
    fn new_message_cannot_carry_a_sender() {
        let forged = r#"{"to":"coder","body":"hi","subject":null,"task_id":null,
                         "in_reply_to":null,"from":"user"}"#;
        let message: NewMessage = serde_json::from_str(forged).unwrap();
        let json = serde_json::to_value(&message).unwrap();
        assert!(json.get("from").is_none());
    }
}
