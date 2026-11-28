//! The contract of Message Broker.

use crate::files::Attachment;
use crate::{
    Alias, Caller, CorrelationId, FileId, MessageId, Result, SessionKey, TaskId, Timestamp,
};
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
    /// Files the sender may read; the recipient may read them too once sent (D106).
    #[serde(default)]
    pub attachments: Vec<FileId>,
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
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// From an adapter: text from outside, data and never instructions (D110).
    #[serde(default)]
    pub external: bool,
}

impl Message {
    pub fn session(&self) -> SessionKey {
        SessionKey::of(self.task_id, self.correlation_id)
    }
}

/// The operator's conversation with one agent: every session in one thread (D102).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub agent: Alias,
    pub last: Message,
    /// Messages from the agent the operator has not opened yet.
    pub unread: u32,
}

/// One conversation of the operator with an agent: a session of theirs (D78, D105).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub session: SessionKey,
    /// What started it, for a title.
    pub first: Message,
    pub last: Message,
    /// Messages from the agent the operator has not opened yet.
    pub unread: u32,
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

/// A chain that delivers nothing until it is resumed (D27).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedChain {
    pub chain: CorrelationId,
    pub stopped_at: Timestamp,
    /// Why the system stopped it, such as a safeguard (D100). `None` when the operator did.
    pub reason: Option<String>,
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

    /// The agents the operator talks to, the latest first (D102). Operator only.
    async fn conversations(&self, caller: &Caller) -> Result<Vec<Conversation>>;

    /// The operator's threads with `agent`, the latest first, at most a hundred
    /// (D105). Operator only.
    async fn threads(&self, caller: &Caller, agent: &Alias) -> Result<Vec<Thread>>;

    /// A page of the operator's messages with `agent`, oldest first: the newest, or
    /// those just before `before`; of one thread, or of all. Operator only.
    async fn conversation(
        &self,
        caller: &Caller,
        agent: &Alias,
        session: Option<SessionKey>,
        before: Option<MessageId>,
    ) -> Result<Vec<Message>>;

    /// Marks the thread of `up_to` read up to and including it, as when the operator
    /// opens it. Never moves back. Operator only.
    async fn mark_read(&self, caller: &Caller, agent: &Alias, up_to: MessageId) -> Result<()>;
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

    /// Whether the chain is stopped (D27).
    async fn is_stopped(&self, chain: CorrelationId) -> Result<bool>;

    /// Stops a chain on the system's own behalf, such as a safeguard (D100). The
    /// operator resumes it like any other.
    async fn halt_chain(&self, chain: CorrelationId, reason: &str) -> Result<()>;

    /// When the chain was last resumed. Its limits count from there (D100).
    async fn resumed_at(&self, chain: CorrelationId) -> Result<Option<Timestamp>>;

    /// Whether the recipient has read the message: a finished Run moved its cursor
    /// past it (D49), or the operator skipped it.
    async fn is_read(&self, message: MessageId) -> Result<bool>;
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

    async fn stopped_chains(&self, caller: &Caller) -> Result<Vec<StoppedChain>>;

    /// Wakes the agent if it has mail, as after it was enabled again (D96).
    async fn deliver_pending(&self, caller: &Caller, agent: &Alias) -> Result<()>;
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
