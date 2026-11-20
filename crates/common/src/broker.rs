//! The contract of Message Broker.

use crate::{Alias, Caller, CorrelationId, MessageId, Result, SessionKey, TaskId, Timestamp};
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

impl Message {
    pub fn session(&self) -> SessionKey {
        SessionKey::of(self.task_id, self.correlation_id)
    }
}

/// Unread messages of one session, handed to one Run (D57).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxBatch {
    /// The session to run in (D78).
    pub session: SessionKey,
    /// Oldest first, never empty.
    pub messages: Vec<Message>,
}

/// A session whose Runs failed, as the operator sees it (D77).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailingSession {
    pub session: SessionKey,
    /// Failed Runs in a row.
    pub failures: u32,
    pub last_error: String,
    /// Delivery stopped until the operator retries or skips the session.
    pub stuck: bool,
}

/// An agent's inbox at a glance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxStatus {
    pub agent: Alias,
    pub unread: u32,
    pub failing: Vec<FailingSession>,
}

/// What senders and readers call. Exposed through the external API.
#[async_trait]
pub trait MessageBroker: Send + Sync {
    /// Stores a message from `caller` and wakes the recipient.
    ///
    /// `correlation_id` comes from the caller's Run. For a caller without a Run
    /// it is inherited through `in_reply_to`, otherwise a new chain starts (D41).
    async fn send(&self, caller: &Caller, message: NewMessage) -> Result<Message>;

    /// Unread messages of `caller` for one session, oldest first. An agent does not
    /// see messages of stopped chains (D27); the operator sees everything.
    ///
    /// The cursor does not move: only a finished Run moves it (D49).
    async fn read_messages(&self, caller: &Caller, session: SessionKey) -> Result<Vec<Message>>;
}

/// A stream of aliases of agents that just got a message (D86).
///
/// A hint only: the inbox stays the source of truth (D15). A subscriber that lags
/// behind gets `RecvError::Lagged` and should reconcile against the inbox.
pub type Notifications = tokio::sync::broadcast::Receiver<Alias>;

/// What Agent Runtime calls to drive delivery. Internal, never exposed through the API.
#[async_trait]
pub trait InboxDelivery: Send + Sync {
    /// Subscribes to new-message notifications. Broker decides who is notified,
    /// the subscriber decides what to do (D12). Subscribe before reconciling,
    /// so nothing falls between the two.
    fn subscribe(&self) -> Notifications;

    /// Agents that have unread messages. Used to reconcile on startup (D15).
    async fn agents_with_unread(&self) -> Result<Vec<Alias>>;

    /// All unread messages of the session holding the agent's oldest unread message.
    /// Stuck sessions (D77) and messages of stopped chains (D27) are left out.
    // Deliberately simple for one active Run per agent; revisit with parallel Runs (D57).
    async fn next_batch(&self, agent: &Alias) -> Result<Option<InboxBatch>>;

    /// Unread messages of one session, oldest first, without those of stopped chains.
    /// What an active Run may get by `steer`.
    async fn unread_in(&self, agent: &Alias, session: SessionKey) -> Result<Vec<Message>>;

    /// Marks a session's messages up to and including `up_to` as read (D49), and
    /// forgets the session's failures: they count only in a row (D77).
    ///
    /// The cursor follows delivery order and never moves back.
    async fn advance_cursor(
        &self,
        agent: &Alias,
        session: SessionKey,
        up_to: MessageId,
    ) -> Result<()>;

    /// Counts a failed Run of the session. Returns the failures in a row (D77).
    async fn record_failure(&self, agent: &Alias, session: SessionKey, error: &str) -> Result<u32>;

    /// Stops delivering the session until the operator retries or skips it (D77).
    async fn mark_stuck(&self, agent: &Alias, session: SessionKey) -> Result<()>;

    /// Whether the operator stopped the chain (D27).
    async fn is_stopped(&self, chain: CorrelationId) -> Result<bool>;
}

/// What the operator calls to see and unblock delivery (D77). Exposed through the API.
#[async_trait]
pub trait DeliveryControl: Send + Sync {
    /// Agents with unread messages or failing sessions.
    async fn inbox_status(&self, caller: &Caller) -> Result<Vec<InboxStatus>>;

    /// Forgets the session's failures and delivers it again.
    async fn retry(&self, caller: &Caller, agent: &Alias, session: SessionKey) -> Result<()>;

    /// Marks the session's unread messages as read without delivering them.
    async fn skip(&self, caller: &Caller, agent: &Alias, session: SessionKey) -> Result<()>;

    /// Stops delivering the chain's messages: they are kept, but wake nobody, and its
    /// agents may not write into it any more (D27). Stopping twice is fine.
    async fn stop_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()>;

    /// Delivers the chain again, including what arrived while it was stopped.
    async fn resume_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()>;

    async fn stopped_chains(&self, caller: &Caller) -> Result<Vec<CorrelationId>>;
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
        Arc<dyn DeliveryControl>,
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
