//! The contract of Scheduler (D6, D35, D107).

use crate::{Alias, Caller, MessageId, Result, ScheduleId, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The sender of every scheduled message. Reserved: no agent may take it.
pub const SCHEDULER: &str = "scheduler";

/// A message sent to an agent on a cron schedule. Each fire starts a chain of its
/// own; a fire is skipped while the agent has not read the previous one, and fires
/// missed while the control plane was down are not made up (D107).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: ScheduleId,
    pub agent: Alias,
    /// Five fields: minute, hour, day of month, month, day of week.
    pub cron: String,
    /// An IANA zone such as `Europe/Moscow`; the cron reads in its local time.
    pub timezone: String,
    pub subject: Option<String>,
    pub body: String,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub last_fired_at: Option<Timestamp>,
    /// The message of the last fire: while it is unread, the next fire is skipped.
    pub last_message: Option<MessageId>,
    /// When it fires next; `None` while disabled.
    pub next_fire_at: Option<Timestamp>,
}

/// A schedule as the operator creates it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSchedule {
    pub agent: Alias,
    pub cron: String,
    /// UTC when not given.
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    pub body: String,
}

/// A change to a schedule: only the fields given change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleUpdate {
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub subject: Option<Option<String>>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// What the operator asks of Scheduler. Operator only.
#[async_trait]
pub trait Schedules: Send + Sync {
    async fn create_schedule(&self, caller: &Caller, schedule: NewSchedule) -> Result<Schedule>;

    /// Every schedule, by agent, then by creation.
    async fn list_schedules(&self, caller: &Caller) -> Result<Vec<Schedule>>;

    async fn get_schedule(&self, caller: &Caller, id: ScheduleId) -> Result<Schedule>;

    async fn update_schedule(
        &self,
        caller: &Caller,
        id: ScheduleId,
        update: ScheduleUpdate,
    ) -> Result<Schedule>;

    async fn delete_schedule(&self, caller: &Caller, id: ScheduleId) -> Result<()>;
}
