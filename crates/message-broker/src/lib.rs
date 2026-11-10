//! Message Broker on SQLite: messages, inboxes and cursors.

use async_trait::async_trait;
use control_plane_common::broker::{
    InboxBatch, InboxDelivery, Message, MessageBroker, NewMessage, WakeSink,
};
use control_plane_common::registry::{AgentDirectory, Credentials};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, TaskId, Timestamp,
};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::sync::Arc;

pub struct SqliteBroker {
    pool: SqlitePool,
    directory: Arc<dyn AgentDirectory>,
    credentials: Arc<dyn Credentials>,
    wake: Arc<dyn WakeSink>,
}

impl SqliteBroker {
    /// Wraps a pool and brings the schema up to date.
    pub async fn new(
        pool: SqlitePool,
        directory: Arc<dyn AgentDirectory>,
        credentials: Arc<dyn Credentials>,
        wake: Arc<dyn WakeSink>,
    ) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self {
            pool,
            directory,
            credentials,
            wake,
        })
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
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.task_id = m.task_id
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
        self.authorize(caller, &message.to).await?;

        // A Run token must not leave its agent (D24).
        // TODO(journal): record the rejection once Agent Journal exists.
        for text in [Some(&message.body), message.subject.as_ref()]
            .into_iter()
            .flatten()
        {
            if self.credentials.contains_active_run_token(text).await? {
                return Err(Error::Forbidden(
                    "message contains an active Run token".to_owned(),
                ));
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
                 (id, sender, recipient, body, subject, task_id, in_reply_to, correlation_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(stored.id.to_string())
        .bind(stored.from.as_str())
        .bind(stored.to.as_str())
        .bind(&stored.body)
        .bind(&stored.subject)
        .bind(task_key(stored.task_id))
        .bind(stored.in_reply_to.map(|id| id.to_string()))
        .bind(stored.correlation_id.to_string())
        .bind(stored.created_at.as_unix_millis())
        .execute(&self.pool)
        .await
        .map_err(internal)?;

        // Only agents are woken. The operator reads their inbox through the CLI.
        if self.directory.agent(&stored.to).await.is_ok() {
            self.wake.wake(&stored.to).await;
        }
        Ok(stored)
    }

    async fn read_messages(
        &self,
        caller: &Caller,
        task_id: Option<TaskId>,
    ) -> Result<Vec<Message>> {
        let mut unread = self.unread(caller.alias()).await?;
        unread.retain(|m| m.task_id == task_id);
        Ok(unread)
    }
}

#[async_trait]
impl InboxDelivery for SqliteBroker {
    async fn agents_with_unread(&self) -> Result<Vec<Alias>> {
        let recipients: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT m.recipient
             FROM messages m
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.task_id = m.task_id
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
        let task_id = oldest.task_id;
        unread.retain(|m| m.task_id == task_id);
        Ok(Some(InboxBatch {
            task_id,
            messages: unread,
        }))
    }

    async fn advance_cursor(
        &self,
        agent: &Alias,
        task_id: Option<TaskId>,
        up_to: MessageId,
    ) -> Result<()> {
        let seq: Option<i64> = sqlx::query_scalar(
            "SELECT seq FROM messages WHERE id = ? AND recipient = ? AND task_id = ?",
        )
        .bind(up_to.to_string())
        .bind(agent.as_str())
        .bind(task_key(task_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        let seq = seq.ok_or_else(|| {
            Error::InvalidInput(format!("message {up_to} is not in this session of {agent}"))
        })?;

        sqlx::query(
            "INSERT INTO cursors (recipient, task_id, up_to) VALUES (?, ?, ?)
             ON CONFLICT (recipient, task_id) DO UPDATE SET up_to = MAX(up_to, excluded.up_to)",
        )
        .bind(agent.as_str())
        .bind(task_key(task_id))
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
    task_id: String,
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
            task_id: (!self.task_id.is_empty())
                .then(|| parse(&self.task_id))
                .transpose()?,
            in_reply_to: self.in_reply_to.as_deref().map(parse).transpose()?,
            correlation_id: parse(&self.correlation_id)?,
            created_at: Timestamp::from_unix_millis(self.created_at),
        })
    }
}

fn task_key(task_id: Option<TaskId>) -> String {
    task_id.map(|id| id.to_string()).unwrap_or_default()
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
