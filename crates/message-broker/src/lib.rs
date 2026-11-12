//! Message Broker on SQLite: messages, inboxes and cursors.

use async_trait::async_trait;
use control_plane_common::broker::{
    InboxBatch, InboxDelivery, Message, MessageBroker, NewMessage, Notifications,
};
use control_plane_common::journal::{Component, Decision, Event, JournalWriter};
use control_plane_common::registry::{AgentDirectory, Credentials};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, SessionKey, TaskId, Timestamp,
};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Notifications kept for a slow subscriber; beyond this it lags and reconciles (D86).
const NOTIFICATION_BUFFER: usize = 1024;

pub struct SqliteBroker {
    pool: SqlitePool,
    directory: Arc<dyn AgentDirectory>,
    credentials: Arc<dyn Credentials>,
    journal: Arc<dyn JournalWriter>,
    notifications: broadcast::Sender<Alias>,
}

impl SqliteBroker {
    /// Wraps a pool and brings the schema up to date.
    pub async fn new(
        pool: SqlitePool,
        directory: Arc<dyn AgentDirectory>,
        credentials: Arc<dyn Credentials>,
        journal: Arc<dyn JournalWriter>,
    ) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self {
            pool,
            directory,
            credentials,
            journal,
            notifications: broadcast::channel(NOTIFICATION_BUFFER).0,
        })
    }

    /// Records a refused send in Agent Journal: it is an agent action (D13).
    async fn deny(&self, caller: &Caller, task_id: Option<TaskId>, reason: &str) -> Result<()> {
        self.journal
            .record(Event {
                decision: Some(Decision::Deny),
                reason: Some(reason.to_owned()),
                task_id,
                ..Event::new(caller, Component::Broker, "send_message")
            })
            .await
    }

    /// Checks that `caller` may write to `to` (D50).
    async fn authorize(&self, caller: &Caller, to: &Alias) -> Result<()> {
        match caller {
            Caller::Agent { alias, .. } => {
                let card = self.directory.agent(alias).await?;
                if card.allow_message_to.contains(to) {
                    Ok(())
                } else {
                    Err(Error::Forbidden(format!("{alias} may not message {to}")))
                }
            }
            // The operator may write to any agent.
            Caller::Operator { .. } => self.directory.agent(to).await.map(drop),
            // Adapters are not registered yet (D71).
            Caller::Adapter { alias } => Err(Error::Forbidden(format!(
                "adapter {alias} is not supported yet"
            ))),
        }
    }

    /// The chain a new message belongs to (D41).
    async fn correlation(
        &self,
        caller: &Caller,
        in_reply_to: Option<MessageId>,
    ) -> Result<CorrelationId> {
        if let Caller::Agent { correlation_id, .. } = caller {
            return Ok(*correlation_id);
        }
        let Some(original) = in_reply_to else {
            return Ok(CorrelationId::new());
        };
        let found: Option<String> =
            sqlx::query_scalar("SELECT correlation_id FROM messages WHERE id = ?")
                .bind(original.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        let found = found
            .ok_or_else(|| Error::InvalidInput(format!("no message {original} to reply to")))?;
        parse(&found)
    }

    /// Unread messages of a recipient, oldest first.
    async fn unread(&self, recipient: &Alias) -> Result<Vec<Message>> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT m.id, m.sender, m.recipient, m.body, m.subject, m.task_id,
                    m.in_reply_to, m.correlation_id, m.created_at
             FROM messages m
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.session = m.session
             WHERE m.recipient = ? AND m.seq > COALESCE(c.up_to, 0)
             ORDER BY m.seq",
        )
        .bind(recipient.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter().map(Row::into_message).collect()
    }
}

#[async_trait]
impl MessageBroker for SqliteBroker {
    async fn send(&self, caller: &Caller, message: NewMessage) -> Result<Message> {
        if let Err(error) = self.authorize(caller, &message.to).await {
            if let Error::Forbidden(reason) = &error {
                self.deny(caller, message.task_id, reason).await?;
            }
            return Err(error);
        }

        // A Run token must not leave its agent (D24).
        for text in [Some(&message.body), message.subject.as_ref()]
            .into_iter()
            .flatten()
        {
            if self.credentials.contains_active_run_token(text).await? {
                let reason = "message contains an active Run token";
                self.deny(caller, message.task_id, reason).await?;
                return Err(Error::Forbidden(reason.to_owned()));
            }
        }

        let stored = Message {
            id: MessageId::new(),
            from: caller.alias().clone(),
            correlation_id: self.correlation(caller, message.in_reply_to).await?,
            created_at: Timestamp::now(),
            to: message.to,
            body: message.body,
            subject: message.subject,
            task_id: message.task_id,
            in_reply_to: message.in_reply_to,
        };

        sqlx::query(
            "INSERT INTO messages
                 (id, sender, recipient, body, subject, task_id, in_reply_to, correlation_id,
                  session, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(stored.id.to_string())
        .bind(stored.from.as_str())
        .bind(stored.to.as_str())
        .bind(&stored.body)
        .bind(&stored.subject)
        .bind(stored.task_id.map(|id| id.to_string()))
        .bind(stored.in_reply_to.map(|id| id.to_string()))
        .bind(stored.correlation_id.to_string())
        .bind(stored.session().to_string())
        .bind(stored.created_at.as_unix_millis())
        .execute(&self.pool)
        .await
        .map_err(internal)?;

        // Only agents are notified. The operator reads their inbox through the CLI.
        if self.directory.agent(&stored.to).await.is_ok() {
            // Having no subscriber is fine: the inbox is the source of truth (D15).
            let _ = self.notifications.send(stored.to.clone());
        }
        Ok(stored)
    }

    async fn read_messages(&self, caller: &Caller, session: SessionKey) -> Result<Vec<Message>> {
        self.unread_in(caller.alias(), session).await
    }
}

#[async_trait]
impl InboxDelivery for SqliteBroker {
    fn subscribe(&self) -> Notifications {
        self.notifications.subscribe()
    }

    async fn agents_with_unread(&self) -> Result<Vec<Alias>> {
        let recipients: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT m.recipient
             FROM messages m
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.session = m.session
             WHERE m.seq > COALESCE(c.up_to, 0)
             ORDER BY m.recipient",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut agents = Vec::new();
        for recipient in recipients {
            let alias: Alias = parse(&recipient)?;
            if self.directory.agent(&alias).await.is_ok() {
                agents.push(alias);
            }
        }
        Ok(agents)
    }

    async fn next_batch(&self, agent: &Alias) -> Result<Option<InboxBatch>> {
        let mut unread = self.unread(agent).await?;
        let Some(oldest) = unread.first() else {
            return Ok(None);
        };
        let session = oldest.session();
        unread.retain(|m| m.session() == session);
        Ok(Some(InboxBatch {
            session,
            messages: unread,
        }))
    }

    async fn unread_in(&self, agent: &Alias, session: SessionKey) -> Result<Vec<Message>> {
        let mut unread = self.unread(agent).await?;
        unread.retain(|m| m.session() == session);
        Ok(unread)
    }

    async fn advance_cursor(
        &self,
        agent: &Alias,
        session: SessionKey,
        up_to: MessageId,
    ) -> Result<()> {
        let seq: Option<i64> = sqlx::query_scalar(
            "SELECT seq FROM messages WHERE id = ? AND recipient = ? AND session = ?",
        )
        .bind(up_to.to_string())
        .bind(agent.as_str())
        .bind(session.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        let seq = seq.ok_or_else(|| {
            Error::InvalidInput(format!("message {up_to} is not in this session of {agent}"))
        })?;

        sqlx::query(
            "INSERT INTO cursors (recipient, session, up_to) VALUES (?, ?, ?)
             ON CONFLICT (recipient, session) DO UPDATE SET up_to = MAX(up_to, excluded.up_to)",
        )
        .bind(agent.as_str())
        .bind(session.to_string())
        .bind(seq)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    sender: String,
    recipient: String,
    body: String,
    subject: Option<String>,
    task_id: Option<String>,
    in_reply_to: Option<String>,
    correlation_id: String,
    created_at: i64,
}

impl Row {
    fn into_message(self) -> Result<Message> {
        Ok(Message {
            id: parse(&self.id)?,
            from: parse(&self.sender)?,
            to: parse(&self.recipient)?,
            body: self.body,
            subject: self.subject,
            task_id: self.task_id.as_deref().map(parse).transpose()?,
            in_reply_to: self.in_reply_to.as_deref().map(parse).transpose()?,
            correlation_id: parse(&self.correlation_id)?,
            created_at: Timestamp::from_unix_millis(self.created_at)?,
        })
    }
}

fn internal(error: sqlx::Error) -> Error {
    Error::Internal(error.to_string())
}

fn parse<T: FromStr>(value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|e: T::Err| Error::Internal(e.to_string()))
}

#[cfg(test)]
mod tests;
