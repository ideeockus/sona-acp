//! Message Broker on SQLite: messages, inboxes and cursors.

use async_trait::async_trait;
use control_plane_common::broker::{
    Conversation, DeliveryControl, FailingSession, InboxBatch, InboxDelivery, InboxStatus, Message,
    MessageBroker, NewMessage, Notifications, StoppedChain, Thread,
};
use control_plane_common::files::{Attachment, FileStorage};
use control_plane_common::journal::{Component, Decision, Event, JournalWriter};
use control_plane_common::registry::{AgentDirectory, AgentState, Credentials};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, SessionKey, TaskId, Timestamp,
};
use sqlx::{SqliteExecutor, SqlitePool};
use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Messages in one page of a conversation (D102).
const PAGE: i64 = 50;

/// Threads listed for one agent (D105).
const THREADS: i64 = 100;

/// Messages between the operator `?1` and the agent `?2`, as a `WHERE` condition.
const BETWEEN: &str = "((sender = ?1 AND recipient = ?2) OR (sender = ?2 AND recipient = ?1))";

/// Columns of a `Row`, for queries on `messages`.
const MESSAGE_COLUMNS: &str = "id, sender, recipient, body, subject, task_id, in_reply_to, \
     correlation_id, created_at, attachments";

/// Files one message may carry.
const MAX_ATTACHMENTS: usize = 20;

/// Notifications kept for a slow subscriber; beyond this it lags and reconciles (D86).
const NOTIFICATION_BUFFER: usize = 1024;

pub struct SqliteBroker {
    pool: SqlitePool,
    directory: Arc<dyn AgentDirectory>,
    credentials: Arc<dyn Credentials>,
    files: Arc<dyn FileStorage>,
    journal: Arc<dyn JournalWriter>,
    notifications: broadcast::Sender<Alias>,
}

impl SqliteBroker {
    /// Wraps a pool and brings the schema up to date.
    pub async fn new(
        pool: SqlitePool,
        directory: Arc<dyn AgentDirectory>,
        credentials: Arc<dyn Credentials>,
        files: Arc<dyn FileStorage>,
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
            files,
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
            // Only code inside the control plane makes an adapter caller so far: Scheduler
            // (D107). Registered adapters with their own limits come with D71.
            Caller::Adapter { .. } => self.directory.agent(to).await.map(drop),
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

    /// Where a message between `operator` and `agent` stands, and its session.
    async fn seq_in_conversation(
        &self,
        operator: &str,
        agent: &Alias,
        id: MessageId,
    ) -> Result<(i64, String)> {
        let found: Option<(i64, String)> = sqlx::query_as(&format!(
            "SELECT seq, session FROM messages WHERE id = ?3 AND {BETWEEN}"
        ))
        .bind(operator)
        .bind(agent.as_str())
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        found.ok_or_else(|| Error::NotFound(format!("message {id} with {agent}")))
    }

    /// Messages from `agent` the operator has not opened, in one session or in all.
    async fn unread_from(&self, operator: &str, agent: &str, session: Option<&str>) -> Result<u32> {
        let unread: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages m
             LEFT JOIN thread_reads r
               ON r.reader = ?2 AND r.agent = ?1 AND r.session = m.session
             WHERE m.sender = ?1 AND m.recipient = ?2 AND m.seq > COALESCE(r.up_to, 0)
               AND (?3 IS NULL OR m.session = ?3)",
        )
        .bind(agent)
        .bind(operator)
        .bind(session)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        u32::try_from(unread).map_err(|e| Error::Internal(e.to_string()))
    }

    async fn message_at(&self, seq: i64) -> Result<Message> {
        let row: Row = sqlx::query_as(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE seq = ?"
        ))
        .bind(seq)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        row.into_message()
    }

    /// Whether the recipient is an agent that may be woken: not the operator, not a
    /// disabled or deleted agent (D96).
    async fn works(&self, recipient: &Alias) -> Result<bool> {
        match self.directory.record(recipient).await {
            Ok(agent) => Ok(agent.state == AgentState::Active),
            Err(Error::NotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Unread messages of a recipient that may be delivered: none of stopped chains (D27).
    async fn deliverable(&self, recipient: &Alias) -> Result<Vec<Message>> {
        let stopped = self.stopped().await?;
        let mut unread = self.unread(recipient).await?;
        unread.retain(|m| !stopped.contains(&m.correlation_id));
        Ok(unread)
    }

    async fn stopped(&self) -> Result<HashSet<CorrelationId>> {
        let chains: Vec<String> = sqlx::query_scalar("SELECT correlation_id FROM stopped_chains")
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;
        chains.iter().map(|c| parse(c)).collect()
    }

    /// Unread messages of a recipient, oldest first.
    async fn unread(&self, recipient: &Alias) -> Result<Vec<Message>> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT m.id, m.sender, m.recipient, m.body, m.subject, m.task_id,
                    m.in_reply_to, m.correlation_id, m.created_at, m.attachments
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

        // A disabled agent takes no messages, not even from the operator (D96).
        if let Ok(recipient) = self.directory.record(&message.to).await
            && recipient.state == AgentState::Disabled
        {
            let reason = format!("{} is disabled", message.to);
            self.deny(caller, message.task_id, &reason).await?;
            return Err(Error::Forbidden(reason));
        }

        // A stopped chain takes nothing more from its agents (D27). The operator may still
        // write: the message waits for `resume`.
        if let Caller::Agent { correlation_id, .. } = caller
            && self.is_stopped(*correlation_id).await?
        {
            let reason = "the chain is stopped";
            self.deny(caller, message.task_id, reason).await?;
            return Err(Error::Forbidden(reason.to_owned()));
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

        if message.attachments.len() > MAX_ATTACHMENTS {
            return Err(Error::InvalidInput(format!(
                "a message carries at most {MAX_ATTACHMENTS} files"
            )));
        }
        // Only what the sender may read, and from now on the recipient may read it too.
        let attachments: Vec<Attachment> = self
            .files
            .share(caller, &message.attachments, &message.to)
            .await?
            .into_iter()
            .map(Attachment::from)
            .collect();

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
            attachments,
        };
        let attachments = serde_json::to_string(&stored.attachments)
            .map_err(|e| Error::Internal(e.to_string()))?;

        sqlx::query(
            "INSERT INTO messages
                 (id, sender, recipient, body, subject, task_id, in_reply_to, correlation_id,
                  session, created_at, attachments)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(attachments)
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
        let mut unread = match caller {
            Caller::Operator { alias } => self.unread(alias).await?,
            _ => self.deliverable(caller.alias()).await?,
        };
        unread.retain(|m| m.session() == session);
        Ok(unread)
    }

    async fn conversations(&self, caller: &Caller) -> Result<Vec<Conversation>> {
        require_operator(caller)?;
        let operator = caller.alias().as_str();
        let latest: Vec<(String, i64)> = sqlx::query_as(
            "SELECT CASE WHEN sender = ?1 THEN recipient ELSE sender END AS agent, MAX(seq)
             FROM messages WHERE sender = ?1 OR recipient = ?1
             GROUP BY agent ORDER BY MAX(seq) DESC",
        )
        .bind(operator)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut conversations = Vec::new();
        for (agent, seq) in latest {
            conversations.push(Conversation {
                unread: self.unread_from(operator, &agent, None).await?,
                agent: parse(&agent)?,
                last: self.message_at(seq).await?,
            });
        }
        Ok(conversations)
    }

    async fn threads(&self, caller: &Caller, agent: &Alias) -> Result<Vec<Thread>> {
        require_operator(caller)?;
        let operator = caller.alias().as_str();
        let sessions: Vec<(String, i64, i64)> = sqlx::query_as(&format!(
            "SELECT session, MIN(seq), MAX(seq) FROM messages WHERE {BETWEEN}
             GROUP BY session ORDER BY MAX(seq) DESC LIMIT ?3"
        ))
        .bind(operator)
        .bind(agent.as_str())
        .bind(THREADS)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut threads = Vec::new();
        for (session, first, last) in sessions {
            threads.push(Thread {
                unread: self
                    .unread_from(operator, agent.as_str(), Some(&session))
                    .await?,
                session: parse(&session)?,
                first: self.message_at(first).await?,
                last: self.message_at(last).await?,
            });
        }
        Ok(threads)
    }

    async fn conversation(
        &self,
        caller: &Caller,
        agent: &Alias,
        session: Option<SessionKey>,
        before: Option<MessageId>,
    ) -> Result<Vec<Message>> {
        require_operator(caller)?;
        let operator = caller.alias().as_str();
        let before = match before {
            Some(id) => self.seq_in_conversation(operator, agent, id).await?.0,
            None => i64::MAX,
        };
        let mut rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE {BETWEEN} AND seq < ?3 AND (?5 IS NULL OR session = ?5)
             ORDER BY seq DESC LIMIT ?4"
        ))
        .bind(operator)
        .bind(agent.as_str())
        .bind(before)
        .bind(PAGE)
        .bind(session.map(|s| s.to_string()))
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.reverse();
        rows.into_iter().map(Row::into_message).collect()
    }

    async fn mark_read(&self, caller: &Caller, agent: &Alias, up_to: MessageId) -> Result<()> {
        require_operator(caller)?;
        let operator = caller.alias().as_str();
        let (seq, session) = self.seq_in_conversation(operator, agent, up_to).await?;
        sqlx::query(
            "INSERT INTO thread_reads (reader, agent, session, up_to) VALUES (?, ?, ?, ?)
             ON CONFLICT (reader, agent, session) DO UPDATE SET up_to = MAX(up_to, excluded.up_to)",
        )
        .bind(operator)
        .bind(agent.as_str())
        .bind(session)
        .bind(seq)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
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
            if self.works(&alias).await? {
                agents.push(alias);
            }
        }
        Ok(agents)
    }

    async fn next_batch(&self, agent: &Alias) -> Result<Option<InboxBatch>> {
        if !self.works(agent).await? {
            return Ok(None);
        }
        let stuck: Vec<String> =
            sqlx::query_scalar("SELECT session FROM failures WHERE recipient = ? AND stuck = 1")
                .bind(agent.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(internal)?;
        let stuck: HashSet<String> = stuck.into_iter().collect();
        let mut unread = self.deliverable(agent).await?;
        unread.retain(|m| !stuck.contains(&m.session().to_string()));
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
        let mut unread = self.deliverable(agent).await?;
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

        let mut tx = self.pool.begin().await.map_err(internal)?;
        move_cursor(&mut *tx, agent, session, seq).await?;
        forget_failures(&mut *tx, agent, session).await?;
        tx.commit().await.map_err(internal)
    }

    async fn record_failure(&self, agent: &Alias, session: SessionKey, error: &str) -> Result<u32> {
        let count: i64 = sqlx::query_scalar(
            "INSERT INTO failures (recipient, session, count, last_error) VALUES (?, ?, 1, ?)
             ON CONFLICT (recipient, session)
             DO UPDATE SET count = count + 1, last_error = excluded.last_error
             RETURNING count",
        )
        .bind(agent.as_str())
        .bind(session.to_string())
        .bind(error)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        u32::try_from(count).map_err(|e| Error::Internal(e.to_string()))
    }

    async fn mark_stuck(&self, agent: &Alias, session: SessionKey) -> Result<()> {
        let updated =
            sqlx::query("UPDATE failures SET stuck = 1 WHERE recipient = ? AND session = ?")
                .bind(agent.as_str())
                .bind(session.to_string())
                .execute(&self.pool)
                .await
                .map_err(internal)?;
        if updated.rows_affected() == 0 {
            return Err(Error::Internal(format!(
                "{session} of {agent} has no failures to stop on"
            )));
        }
        Ok(())
    }

    async fn is_stopped(&self, chain: CorrelationId) -> Result<bool> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM stopped_chains WHERE correlation_id = ?")
                .bind(chain.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        Ok(found.is_some())
    }

    async fn halt_chain(&self, chain: CorrelationId, reason: &str) -> Result<()> {
        // A chain already stopped keeps the reason it was stopped for.
        sqlx::query(
            "INSERT INTO stopped_chains (correlation_id, stopped_at, reason) VALUES (?, ?, ?)
             ON CONFLICT (correlation_id) DO NOTHING",
        )
        .bind(chain.to_string())
        .bind(Timestamp::now().as_unix_millis())
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn resumed_at(&self, chain: CorrelationId) -> Result<Option<Timestamp>> {
        let at: Option<i64> =
            sqlx::query_scalar("SELECT resumed_at FROM chain_resumes WHERE correlation_id = ?")
                .bind(chain.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        at.map(Timestamp::from_unix_millis).transpose()
    }

    async fn is_read(&self, message: MessageId) -> Result<bool> {
        let read: Option<bool> = sqlx::query_scalar(
            "SELECT m.seq <= COALESCE(c.up_to, 0) FROM messages m
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.session = m.session
             WHERE m.id = ?",
        )
        .bind(message.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        read.ok_or_else(|| Error::NotFound(format!("message {message}")))
    }
}

#[async_trait]
impl DeliveryControl for SqliteBroker {
    async fn inbox_status(&self, caller: &Caller) -> Result<Vec<InboxStatus>> {
        require_operator(caller)?;
        let unread: Vec<(String, i64)> = sqlx::query_as(
            "SELECT m.recipient, COUNT(*)
             FROM messages m
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.session = m.session
             WHERE m.seq > COALESCE(c.up_to, 0)
             GROUP BY m.recipient",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        let failures: Vec<(String, String, i64, String, i64)> = sqlx::query_as(
            "SELECT recipient, session, count, last_error, stuck FROM failures
             ORDER BY recipient, session",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut inboxes = BTreeMap::new();
        for (recipient, count) in unread {
            inbox(&mut inboxes, &recipient)?.unread =
                u32::try_from(count).map_err(|e| Error::Internal(e.to_string()))?;
        }
        for (recipient, session, count, last_error, stuck) in failures {
            inbox(&mut inboxes, &recipient)?
                .failing
                .push(FailingSession {
                    session: parse(&session)?,
                    failures: u32::try_from(count).map_err(|e| Error::Internal(e.to_string()))?,
                    last_error,
                    stuck: stuck != 0,
                });
        }

        // The operator's own unread mail is not an agent's inbox.
        let mut agents = Vec::new();
        for (alias, status) in inboxes {
            if self.directory.agent(&alias).await.is_ok() {
                agents.push(status);
            }
        }
        Ok(agents)
    }

    async fn retry(&self, caller: &Caller, agent: &Alias, session: SessionKey) -> Result<()> {
        require_operator(caller)?;
        if forget_failures(&self.pool, agent, session).await? == 0 {
            return Err(Error::NotFound(format!("failures of {agent} in {session}")));
        }
        let _ = self.notifications.send(agent.clone());
        Ok(())
    }

    async fn skip(&self, caller: &Caller, agent: &Alias, session: SessionKey) -> Result<()> {
        require_operator(caller)?;
        let mut unread = self.unread(agent).await?;
        unread.retain(|m| m.session() == session);
        let newest = unread.last().map(|m| m.id);
        let newest = newest
            .ok_or_else(|| Error::NotFound(format!("unread messages of {agent} in {session}")))?;
        self.advance_cursor(agent, session, newest).await
    }

    async fn stop_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()> {
        require_operator(caller)?;
        sqlx::query(
            "INSERT INTO stopped_chains (correlation_id, stopped_at) VALUES (?, ?)
             ON CONFLICT (correlation_id) DO NOTHING",
        )
        .bind(chain.to_string())
        .bind(Timestamp::now().as_unix_millis())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn resume_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()> {
        require_operator(caller)?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let deleted = sqlx::query("DELETE FROM stopped_chains WHERE correlation_id = ?")
            .bind(chain.to_string())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        if deleted.rows_affected() == 0 {
            return Err(Error::NotFound(format!("stopped chain {chain}")));
        }
        // The chain starts its limits afresh (D100).
        sqlx::query(
            "INSERT INTO chain_resumes (correlation_id, resumed_at) VALUES (?, ?)
             ON CONFLICT (correlation_id) DO UPDATE SET resumed_at = excluded.resumed_at",
        )
        .bind(chain.to_string())
        .bind(Timestamp::now().as_unix_millis())
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        // Whoever has mail in the chain gets it now.
        let recipients: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT m.recipient
             FROM messages m
             LEFT JOIN cursors c ON c.recipient = m.recipient AND c.session = m.session
             WHERE m.correlation_id = ? AND m.seq > COALESCE(c.up_to, 0)",
        )
        .bind(chain.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        for recipient in recipients {
            let alias: Alias = parse(&recipient)?;
            if self.works(&alias).await? {
                let _ = self.notifications.send(alias);
            }
        }
        Ok(())
    }

    async fn stopped_chains(&self, caller: &Caller) -> Result<Vec<StoppedChain>> {
        require_operator(caller)?;
        let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
            "SELECT correlation_id, stopped_at, reason FROM stopped_chains ORDER BY stopped_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter()
            .map(|(chain, stopped_at, reason)| {
                Ok(StoppedChain {
                    chain: parse(&chain)?,
                    stopped_at: Timestamp::from_unix_millis(stopped_at)?,
                    reason,
                })
            })
            .collect()
    }

    async fn deliver_pending(&self, caller: &Caller, agent: &Alias) -> Result<()> {
        require_operator(caller)?;
        if self.next_batch(agent).await?.is_some() {
            let _ = self.notifications.send(agent.clone());
        }
        Ok(())
    }
}

/// The status of `recipient`, added empty if new.
fn inbox<'a>(
    inboxes: &'a mut BTreeMap<Alias, InboxStatus>,
    recipient: &str,
) -> Result<&'a mut InboxStatus> {
    let agent: Alias = parse(recipient)?;
    Ok(inboxes.entry(agent.clone()).or_insert_with(|| InboxStatus {
        agent,
        unread: 0,
        failing: Vec::new(),
    }))
}

async fn move_cursor(
    db: impl SqliteExecutor<'_>,
    agent: &Alias,
    session: SessionKey,
    seq: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO cursors (recipient, session, up_to) VALUES (?, ?, ?)
         ON CONFLICT (recipient, session) DO UPDATE SET up_to = MAX(up_to, excluded.up_to)",
    )
    .bind(agent.as_str())
    .bind(session.to_string())
    .bind(seq)
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(())
}

/// Returns how many rows went: 0 or 1.
async fn forget_failures(
    db: impl SqliteExecutor<'_>,
    agent: &Alias,
    session: SessionKey,
) -> Result<u64> {
    let deleted = sqlx::query("DELETE FROM failures WHERE recipient = ? AND session = ?")
        .bind(agent.as_str())
        .bind(session.to_string())
        .execute(db)
        .await
        .map_err(internal)?;
    Ok(deleted.rows_affected())
}

fn require_operator(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        other => Err(Error::Forbidden(format!(
            "{} is not the operator",
            other.alias()
        ))),
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
    attachments: String,
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
            attachments: serde_json::from_str(&self.attachments)
                .map_err(|e| Error::Internal(e.to_string()))?,
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
