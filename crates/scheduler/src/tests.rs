use super::*;
use control_plane_common::broker::{Conversation, InboxBatch, Message, Notifications, Thread};
use control_plane_common::journal::Decision;
use control_plane_common::registry::{Agent, AgentCard, AgentState, ModelRef};
use control_plane_common::{CorrelationId, RunId, SessionKey};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashSet;
use std::sync::Mutex;

/// Stores what was sent; refuses messages to `disabled`.
#[derive(Default)]
struct Broker {
    sent: Mutex<Vec<(Caller, Message)>>,
    read: Mutex<HashSet<MessageId>>,
}

#[async_trait]
impl MessageBroker for Broker {
    async fn send(&self, caller: &Caller, message: NewMessage) -> Result<Message> {
        if message.to.as_str() == "disabled" {
            return Err(Error::Forbidden("@disabled is disabled".to_owned()));
        }
        let stored = Message {
            id: MessageId::new(),
            from: caller.alias().clone(),
            to: message.to,
            body: message.body,
            subject: message.subject,
            task_id: message.task_id,
            in_reply_to: message.in_reply_to,
            correlation_id: CorrelationId::new(),
            created_at: Timestamp::now(),
            attachments: Vec::new(),
        };
        self.sent
            .lock()
            .unwrap()
            .push((caller.clone(), stored.clone()));
        Ok(stored)
    }
    async fn read_messages(&self, _: &Caller, _: SessionKey) -> Result<Vec<Message>> {
        unimplemented!()
    }
    async fn conversations(&self, _: &Caller) -> Result<Vec<Conversation>> {
        unimplemented!()
    }
    async fn threads(&self, _: &Caller, _: &Alias) -> Result<Vec<Thread>> {
        unimplemented!()
    }
    async fn conversation(
        &self,
        _: &Caller,
        _: &Alias,
        _: Option<SessionKey>,
        _: Option<MessageId>,
    ) -> Result<Vec<Message>> {
        unimplemented!()
    }
    async fn mark_read(&self, _: &Caller, _: &Alias, _: MessageId) -> Result<()> {
        unimplemented!()
    }
}

#[async_trait]
impl InboxDelivery for Broker {
    fn subscribe(&self) -> Notifications {
        unimplemented!()
    }
    async fn agents_with_unread(&self) -> Result<Vec<Alias>> {
        unimplemented!()
    }
    async fn next_batch(&self, _: &Alias) -> Result<Option<InboxBatch>> {
        unimplemented!()
    }
    async fn unread_in(&self, _: &Alias, _: SessionKey) -> Result<Vec<Message>> {
        unimplemented!()
    }
    async fn advance_cursor(&self, _: &Alias, _: SessionKey, _: MessageId) -> Result<()> {
        unimplemented!()
    }
    async fn record_failure(&self, _: &Alias, _: SessionKey, _: &str) -> Result<u32> {
        unimplemented!()
    }
    async fn mark_stuck(&self, _: &Alias, _: SessionKey) -> Result<()> {
        unimplemented!()
    }
    async fn is_stopped(&self, _: CorrelationId) -> Result<bool> {
        unimplemented!()
    }
    async fn halt_chain(&self, _: CorrelationId, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn resumed_at(&self, _: CorrelationId) -> Result<Option<Timestamp>> {
        unimplemented!()
    }
    async fn is_read(&self, message: MessageId) -> Result<bool> {
        Ok(self.read.lock().unwrap().contains(&message))
    }
}

/// Knows `coder` and `disabled`.
struct Directory;

#[async_trait]
impl AgentDirectory for Directory {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        if !["coder", "disabled"].contains(&alias.as_str()) {
            return Err(Error::NotFound(format!("agent {alias}")));
        }
        Ok(AgentCard {
            alias: alias.clone(),
            title: String::new(),
            model: ModelRef {
                provider: "p".to_owned(),
                id: "m".to_owned(),
            },
            access: Vec::new(),
            allow_message_to: Vec::new(),
            prompt: String::new(),
            hue: None,
            skills: Vec::new(),
        })
    }
    async fn record(&self, alias: &Alias) -> Result<Agent> {
        Ok(Agent {
            card: self.agent(alias).await?,
            version: 1,
            state: AgentState::Active,
        })
    }
}

#[derive(Default)]
struct Recorded(Mutex<Vec<Event>>);

#[async_trait]
impl JournalWriter for Recorded {
    async fn record(&self, event: Event) -> Result<()> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

impl Recorded {
    fn actions(&self) -> Vec<(String, Option<Outcome>)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|e| (e.action.clone(), e.result))
            .collect()
    }
}

struct Setup {
    scheduler: SqliteScheduler,
    broker: Arc<Broker>,
    journal: Arc<Recorded>,
    now: Arc<Mutex<Timestamp>>,
}

impl Setup {
    fn at(&self, time: &str) -> Timestamp {
        let time = at(time);
        *self.now.lock().unwrap() = time;
        time
    }
}

async fn setup() -> Setup {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let broker = Arc::new(Broker::default());
    let journal = Arc::new(Recorded::default());
    let now = Arc::new(Mutex::new(at("2026-11-28T05:00:00Z")));
    let clock_now = now.clone();
    let scheduler = SqliteScheduler::new(
        pool,
        broker.clone(),
        broker.clone(),
        Arc::new(Directory),
        journal.clone(),
        Arc::new(move || *clock_now.lock().unwrap()),
    )
    .await
    .unwrap();
    Setup {
        scheduler,
        broker,
        journal,
        now,
    }
}

fn at(time: &str) -> Timestamp {
    let time: jiff::Timestamp = time.parse().unwrap();
    Timestamp::from_unix_millis(time.as_millisecond()).unwrap()
}

fn operator() -> Caller {
    Caller::Operator {
        alias: "user".parse().unwrap(),
    }
}

fn daily(agent: &str) -> NewSchedule {
    NewSchedule {
        agent: agent.parse().unwrap(),
        cron: "0 9 * * *".to_owned(),
        timezone: Some("Europe/Moscow".to_owned()),
        subject: Some("Morning".to_owned()),
        body: "Write the daily report.".to_owned(),
    }
}

#[test]
fn the_cron_reads_in_the_schedules_zone() {
    let after = at("2026-11-28T05:00:00Z");
    // 09:00 in Moscow is 06:00 UTC.
    assert_eq!(
        next_fire("0 9 * * *", "Europe/Moscow", after).unwrap(),
        at("2026-11-28T06:00:00Z")
    );
    assert_eq!(
        next_fire("0 9 * * *", "UTC", after).unwrap(),
        at("2026-11-28T09:00:00Z")
    );
    // Strictly after: a fire at this very minute is the next day's.
    assert_eq!(
        next_fire("0 6 * * *", "UTC", at("2026-11-28T06:00:00Z")).unwrap(),
        at("2026-11-29T06:00:00Z")
    );
    // Berlin keeps 09:00 local across the switch to summer time.
    assert_eq!(
        next_fire("0 9 * * *", "Europe/Berlin", at("2026-03-28T09:00:00Z")).unwrap(),
        at("2026-03-29T07:00:00Z")
    );
}

#[tokio::test]
async fn a_schedule_is_checked_and_for_the_operator_only() {
    let s = setup().await;
    let created = s
        .scheduler
        .create_schedule(&operator(), daily("coder"))
        .await
        .unwrap();
    assert!(created.enabled);
    assert_eq!(created.next_fire_at, Some(at("2026-11-28T06:00:00Z")));

    let bad = [
        NewSchedule {
            cron: "every morning".to_owned(),
            ..daily("coder")
        },
        NewSchedule {
            timezone: Some("Mars/Olympus".to_owned()),
            ..daily("coder")
        },
        NewSchedule {
            body: "  ".to_owned(),
            ..daily("coder")
        },
    ];
    for schedule in bad {
        assert!(matches!(
            s.scheduler.create_schedule(&operator(), schedule).await,
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(matches!(
        s.scheduler
            .create_schedule(&operator(), daily("ghost"))
            .await,
        Err(Error::NotFound(_))
    ));
    let agent = Caller::Agent {
        alias: "coder".parse().unwrap(),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    };
    assert!(matches!(
        s.scheduler.create_schedule(&agent, daily("coder")).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        s.scheduler.list_schedules(&agent).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn a_due_schedule_sends_from_scheduler_in_a_new_chain() {
    let s = setup().await;
    let created = s
        .scheduler
        .create_schedule(&operator(), daily("coder"))
        .await
        .unwrap();
    // Not yet.
    let next = s
        .scheduler
        .tick(s.at("2026-11-28T05:59:00Z"))
        .await
        .unwrap();
    assert_eq!(next, Some(at("2026-11-28T06:00:00Z")));
    assert!(s.broker.sent.lock().unwrap().is_empty());

    let next = s
        .scheduler
        .tick(s.at("2026-11-28T06:00:00Z"))
        .await
        .unwrap();
    assert_eq!(next, Some(at("2026-11-29T06:00:00Z")));
    let sent = s.broker.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let (caller, message) = &sent[0];
    assert!(matches!(caller, Caller::Adapter { alias } if alias.as_str() == SCHEDULER));
    assert_eq!(
        (
            message.body.as_str(),
            message.subject.as_deref(),
            message.in_reply_to
        ),
        ("Write the daily report.", Some("Morning"), None)
    );
    let schedule = s
        .scheduler
        .get_schedule(&operator(), created.id)
        .await
        .unwrap();
    assert_eq!(schedule.last_message, Some(message.id));
    assert_eq!(schedule.last_fired_at, Some(at("2026-11-28T06:00:00Z")));
    assert_eq!(
        s.journal.actions(),
        [("schedule_fired".to_owned(), Some(Outcome::Ok))]
    );
}

#[tokio::test]
async fn a_fire_waits_until_the_previous_message_is_read() {
    let s = setup().await;
    s.scheduler
        .create_schedule(&operator(), daily("coder"))
        .await
        .unwrap();
    s.scheduler
        .tick(s.at("2026-11-28T06:00:00Z"))
        .await
        .unwrap();
    // The agent has not read it by the next morning: skipped, not piled up.
    s.scheduler
        .tick(s.at("2026-11-29T06:00:00Z"))
        .await
        .unwrap();
    assert_eq!(s.broker.sent.lock().unwrap().len(), 1);
    assert_eq!(
        s.journal.actions()[1],
        ("schedule_skipped".to_owned(), Some(Outcome::Error))
    );

    let first = s.broker.sent.lock().unwrap()[0].1.id;
    s.broker.read.lock().unwrap().insert(first);
    s.scheduler
        .tick(s.at("2026-11-30T06:00:00Z"))
        .await
        .unwrap();
    assert_eq!(s.broker.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn fires_missed_while_down_are_recorded_not_sent() {
    let s = setup().await;
    s.scheduler
        .create_schedule(&operator(), daily("coder"))
        .await
        .unwrap();
    // Back up at noon, after the 09:00 fire.
    s.at("2026-11-28T09:00:00Z");
    s.scheduler.inner.skip_missed().await.unwrap();
    assert!(s.broker.sent.lock().unwrap().is_empty());
    assert_eq!(
        s.journal.actions(),
        [("schedule_missed".to_owned(), Some(Outcome::Error))]
    );
    let next = s
        .scheduler
        .tick(s.at("2026-11-28T09:00:00Z"))
        .await
        .unwrap();
    assert_eq!(next, Some(at("2026-11-29T06:00:00Z")));
}

#[tokio::test]
async fn a_refused_send_is_recorded_and_the_schedule_goes_on() {
    let s = setup().await;
    s.scheduler
        .create_schedule(&operator(), daily("disabled"))
        .await
        .unwrap();
    let next = s
        .scheduler
        .tick(s.at("2026-11-28T06:00:00Z"))
        .await
        .unwrap();
    assert_eq!(next, Some(at("2026-11-29T06:00:00Z")));
    let journal = s.journal.0.lock().unwrap();
    assert_eq!(journal[0].result, Some(Outcome::Error));
    assert_eq!(journal[0].decision, None::<Decision>);
    assert!(journal[0].reason.as_deref().unwrap().contains("disabled"));
}

#[tokio::test]
async fn disabled_schedules_do_not_fire_and_deleted_ones_are_gone() {
    let s = setup().await;
    let created = s
        .scheduler
        .create_schedule(&operator(), daily("coder"))
        .await
        .unwrap();
    let off = ScheduleUpdate {
        enabled: Some(false),
        ..ScheduleUpdate::default()
    };
    let updated = s
        .scheduler
        .update_schedule(&operator(), created.id, off)
        .await
        .unwrap();
    assert_eq!((updated.enabled, updated.next_fire_at), (false, None));
    assert_eq!(
        s.scheduler
            .tick(s.at("2026-11-28T06:00:00Z"))
            .await
            .unwrap(),
        None
    );
    assert!(s.broker.sent.lock().unwrap().is_empty());

    // Re-enabled, it counts from now.
    let on = ScheduleUpdate {
        enabled: Some(true),
        cron: Some("30 10 * * *".to_owned()),
        ..ScheduleUpdate::default()
    };
    let updated = s
        .scheduler
        .update_schedule(&operator(), created.id, on)
        .await
        .unwrap();
    assert_eq!(updated.next_fire_at, Some(at("2026-11-28T07:30:00Z")));

    s.scheduler
        .delete_schedule(&operator(), created.id)
        .await
        .unwrap();
    assert!(matches!(
        s.scheduler.get_schedule(&operator(), created.id).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        s.scheduler.delete_schedule(&operator(), created.id).await,
        Err(Error::NotFound(_))
    ));
}
