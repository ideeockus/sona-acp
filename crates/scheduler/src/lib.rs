//! Scheduler: messages to agents on cron schedules, sent as `@scheduler` (D6, D35, D107).
//!
//! Each fire starts a chain of its own. A fire is skipped while the agent has not read
//! the previous one, so a stuck agent does not wake up to a pile; fires missed while
//! the control plane was down are not made up. Both go to Journal.

use async_trait::async_trait;
use control_plane_common::broker::{InboxDelivery, MessageBroker, NewMessage};
use control_plane_common::journal::{Component, Event, JournalWriter, Outcome};
use control_plane_common::registry::AgentDirectory;
use control_plane_common::schedules::{
    NewSchedule, SCHEDULER, Schedule, ScheduleUpdate, Schedules,
};
use control_plane_common::{Alias, Caller, Error, MessageId, Result, ScheduleId, Timestamp};
use croner::Cron;
use jiff::tz::TimeZone;
use sqlx::SqlitePool;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// The time now, replaceable in tests.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The longest the loop sleeps without looking: the wall clock may jump, a laptop may
/// sleep, and the loop must notice within a minute.
const MAX_SLEEP: Duration = Duration::from_secs(60);

const COLUMNS: &str = "id, agent, cron, timezone, subject, body, enabled, created_at, \
     last_fired_at, last_message, next_fire_at";

pub struct SqliteScheduler {
    inner: Arc<Inner>,
}

struct Inner {
    pool: SqlitePool,
    broker: Arc<dyn MessageBroker>,
    inbox: Arc<dyn InboxDelivery>,
    directory: Arc<dyn AgentDirectory>,
    journal: Arc<dyn JournalWriter>,
    clock: Clock,
    /// Wakes the loop when schedules change.
    changed: Notify,
}

impl SqliteScheduler {
    pub async fn new(
        pool: SqlitePool,
        broker: Arc<dyn MessageBroker>,
        inbox: Arc<dyn InboxDelivery>,
        directory: Arc<dyn AgentDirectory>,
        journal: Arc<dyn JournalWriter>,
        clock: Clock,
    ) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                pool,
                broker,
                inbox,
                directory,
                journal,
                clock,
                changed: Notify::new(),
            }),
        })
    }

    /// Call once on startup: records what was missed while down, then fires on time.
    pub async fn start(&self) -> Result<()> {
        self.inner.skip_missed().await?;
        tokio::spawn(self.inner.clone().run());
        Ok(())
    }

    /// Fires what is due at `now`; returns when the next one is due. The loop's step,
    /// public for tests.
    pub async fn tick(&self, now: Timestamp) -> Result<Option<Timestamp>> {
        self.inner.tick(now).await
    }
}

impl Inner {
    async fn run(self: Arc<Self>) {
        loop {
            let now = (self.clock)();
            let sleep = match self.tick(now).await {
                Ok(Some(next)) => {
                    let millis = next.as_unix_millis().saturating_sub(now.as_unix_millis());
                    Duration::from_millis(u64::try_from(millis).unwrap_or(0)).min(MAX_SLEEP)
                }
                Ok(None) | Err(_) => MAX_SLEEP,
            };
            tokio::select! {
                () = tokio::time::sleep(sleep) => {}
                () = self.changed.notified() => {}
            }
        }
    }

    async fn tick(&self, now: Timestamp) -> Result<Option<Timestamp>> {
        let due: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE next_fire_at <= ? ORDER BY next_fire_at"
        ))
        .bind(now.as_unix_millis())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        for row in due {
            let schedule = row.into_schedule()?;
            self.fire(&schedule, now).await?;
        }
        let next: Option<i64> = sqlx::query_scalar("SELECT MIN(next_fire_at) FROM schedules")
            .fetch_one(&self.pool)
            .await
            .map_err(internal)?;
        next.map(Timestamp::from_unix_millis).transpose()
    }

    /// One fire: a message in a new chain, unless the previous one waits unread.
    async fn fire(&self, schedule: &Schedule, now: Timestamp) -> Result<()> {
        let unread = match schedule.last_message {
            Some(last) => match self.inbox.is_read(last).await {
                Ok(read) => !read,
                Err(Error::NotFound(_)) => false,
                Err(e) => return Err(e),
            },
            None => false,
        };
        let mut sent = None;
        if unread {
            self.record(
                schedule,
                "schedule_skipped",
                Outcome::Error,
                Some("the previous message is still unread".to_owned()),
            )
            .await?;
        } else {
            let message = NewMessage {
                to: schedule.agent.clone(),
                body: schedule.body.clone(),
                subject: schedule.subject.clone(),
                task_id: None,
                in_reply_to: None,
                attachments: Vec::new(),
            };
            // A disabled or deleted agent refuses; Broker records why, and so do we.
            match self.broker.send(&scheduler(), message).await {
                Ok(message) => {
                    sent = Some(message.id);
                    self.record(schedule, "schedule_fired", Outcome::Ok, None)
                        .await?;
                }
                Err(e) => {
                    self.record(
                        schedule,
                        "schedule_fired",
                        Outcome::Error,
                        Some(e.to_string()),
                    )
                    .await?;
                }
            }
        }
        let next = next_fire(&schedule.cron, &schedule.timezone, now)?;
        sqlx::query(
            "UPDATE schedules SET next_fire_at = ?,
                 last_fired_at = CASE WHEN ? IS NULL THEN last_fired_at ELSE ? END,
                 last_message = COALESCE(?, last_message)
             WHERE id = ?",
        )
        .bind(next.as_unix_millis())
        .bind(sent.map(|_| now.as_unix_millis()))
        .bind(now.as_unix_millis())
        .bind(sent.map(|id| id.to_string()))
        .bind(schedule.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    /// Fires that came while the control plane was down are recorded, not sent.
    async fn skip_missed(&self) -> Result<()> {
        let now = (self.clock)();
        let missed: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE next_fire_at < ?"
        ))
        .bind(now.as_unix_millis())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        for row in missed {
            let schedule = row.into_schedule()?;
            self.record(
                &schedule,
                "schedule_missed",
                Outcome::Error,
                Some("the control plane was down".to_owned()),
            )
            .await?;
            let next = next_fire(&schedule.cron, &schedule.timezone, now)?;
            self.set_next(schedule.id, Some(next)).await?;
        }
        Ok(())
    }

    async fn set_next(&self, id: ScheduleId, next: Option<Timestamp>) -> Result<()> {
        sqlx::query("UPDATE schedules SET next_fire_at = ? WHERE id = ?")
            .bind(next.map(Timestamp::as_unix_millis))
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }

    async fn record(
        &self,
        schedule: &Schedule,
        action: &str,
        result: Outcome,
        reason: Option<String>,
    ) -> Result<()> {
        let reason = Some(match reason {
            Some(reason) => format!("{} for {}: {reason}", schedule.id, schedule.agent),
            None => format!("{} for {}", schedule.id, schedule.agent),
        });
        self.journal
            .record(Event {
                result: Some(result),
                reason,
                ..Event::new(&scheduler(), Component::Scheduler, action)
            })
            .await
    }

    async fn get(&self, id: ScheduleId) -> Result<Schedule> {
        let row: Option<Row> =
            sqlx::query_as(&format!("SELECT {COLUMNS} FROM schedules WHERE id = ?"))
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        row.ok_or_else(|| Error::NotFound(format!("schedule {id}")))?
            .into_schedule()
    }

    /// The cron and zone parse, the agent exists, the message is not empty.
    async fn validate(&self, agent: &Alias, cron: &str, timezone: &str, body: &str) -> Result<()> {
        self.directory.agent(agent).await?;
        next_fire(cron, timezone, (self.clock)())?;
        if body.trim().is_empty() {
            return Err(Error::InvalidInput("a schedule needs a message".to_owned()));
        }
        Ok(())
    }
}

#[async_trait]
impl Schedules for SqliteScheduler {
    async fn create_schedule(&self, caller: &Caller, new: NewSchedule) -> Result<Schedule> {
        require_operator(caller)?;
        let inner = &self.inner;
        let timezone = new.timezone.unwrap_or_else(|| "UTC".to_owned());
        inner
            .validate(&new.agent, &new.cron, &timezone, &new.body)
            .await?;
        let now = (inner.clock)();
        let id = ScheduleId::new();
        sqlx::query(
            "INSERT INTO schedules (id, agent, cron, timezone, subject, body, enabled, created_at,
                                    next_fire_at)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.agent.as_str())
        .bind(new.cron.trim())
        .bind(&timezone)
        .bind(&new.subject)
        .bind(&new.body)
        .bind(now.as_unix_millis())
        .bind(next_fire(&new.cron, &timezone, now)?.as_unix_millis())
        .execute(&inner.pool)
        .await
        .map_err(internal)?;
        inner.changed.notify_one();
        inner.get(id).await
    }

    async fn list_schedules(&self, caller: &Caller) -> Result<Vec<Schedule>> {
        require_operator(caller)?;
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM schedules ORDER BY agent, created_at"
        ))
        .fetch_all(&self.inner.pool)
        .await
        .map_err(internal)?;
        rows.into_iter().map(Row::into_schedule).collect()
    }

    async fn get_schedule(&self, caller: &Caller, id: ScheduleId) -> Result<Schedule> {
        require_operator(caller)?;
        self.inner.get(id).await
    }

    async fn update_schedule(
        &self,
        caller: &Caller,
        id: ScheduleId,
        update: ScheduleUpdate,
    ) -> Result<Schedule> {
        require_operator(caller)?;
        let inner = &self.inner;
        let mut schedule = inner.get(id).await?;
        if let Some(cron) = update.cron {
            schedule.cron = cron.trim().to_owned();
        }
        if let Some(timezone) = update.timezone {
            schedule.timezone = timezone;
        }
        if let Some(subject) = update.subject {
            schedule.subject = subject;
        }
        if let Some(body) = update.body {
            schedule.body = body;
        }
        if let Some(enabled) = update.enabled {
            schedule.enabled = enabled;
        }
        inner
            .validate(
                &schedule.agent,
                &schedule.cron,
                &schedule.timezone,
                &schedule.body,
            )
            .await?;
        // Counted afresh from now: a changed or re-enabled schedule makes up nothing.
        let next = if schedule.enabled {
            Some(next_fire(
                &schedule.cron,
                &schedule.timezone,
                (inner.clock)(),
            )?)
        } else {
            None
        };
        sqlx::query(
            "UPDATE schedules SET cron = ?, timezone = ?, subject = ?, body = ?, enabled = ?,
                 next_fire_at = ?
             WHERE id = ?",
        )
        .bind(&schedule.cron)
        .bind(&schedule.timezone)
        .bind(&schedule.subject)
        .bind(&schedule.body)
        .bind(schedule.enabled)
        .bind(next.map(Timestamp::as_unix_millis))
        .bind(id.to_string())
        .execute(&inner.pool)
        .await
        .map_err(internal)?;
        inner.changed.notify_one();
        inner.get(id).await
    }

    async fn delete_schedule(&self, caller: &Caller, id: ScheduleId) -> Result<()> {
        require_operator(caller)?;
        let deleted = sqlx::query("DELETE FROM schedules WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.inner.pool)
            .await
            .map_err(internal)?;
        if deleted.rows_affected() == 0 {
            return Err(Error::NotFound(format!("schedule {id}")));
        }
        self.inner.changed.notify_one();
        Ok(())
    }
}

/// The first fire strictly after `after`, read in the schedule's zone.
pub fn next_fire(cron: &str, timezone: &str, after: Timestamp) -> Result<Timestamp> {
    let cron = Cron::from_str(cron.trim())
        .map_err(|e| Error::InvalidInput(format!("cron {cron:?}: {e}")))?;
    let zone = TimeZone::get(timezone)
        .map_err(|_| Error::InvalidInput(format!("{timezone:?} is not a time zone")))?;
    let next = cron
        .find_next_occurrence(&after.as_jiff().to_zoned(zone), false)
        .map_err(|e| Error::InvalidInput(format!("cron {cron}: {e}", cron = cron.pattern)))?;
    Timestamp::from_unix_millis(next.timestamp().as_millisecond())
}

fn scheduler() -> Caller {
    Caller::Adapter {
        alias: SCHEDULER.parse().expect("a valid alias"),
    }
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
    agent: String,
    cron: String,
    timezone: String,
    subject: Option<String>,
    body: String,
    enabled: bool,
    created_at: i64,
    last_fired_at: Option<i64>,
    last_message: Option<String>,
    next_fire_at: Option<i64>,
}

impl Row {
    fn into_schedule(self) -> Result<Schedule> {
        Ok(Schedule {
            id: parse(&self.id)?,
            agent: parse(&self.agent)?,
            cron: self.cron,
            timezone: self.timezone,
            subject: self.subject,
            body: self.body,
            enabled: self.enabled,
            created_at: Timestamp::from_unix_millis(self.created_at)?,
            last_fired_at: self
                .last_fired_at
                .map(Timestamp::from_unix_millis)
                .transpose()?,
            last_message: self
                .last_message
                .as_deref()
                .map(parse::<MessageId>)
                .transpose()?,
            next_fire_at: self
                .next_fire_at
                .map(Timestamp::from_unix_millis)
                .transpose()?,
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
