use super::*;
use control_plane_common::RunId;
use control_plane_common::registry::{Agent, AgentCard, ModelRef, Secret};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::Mutex;

const LEAKED: &str = "sona_plane_rt_leaked";

/// Agents keyed by alias, each allowed to message the listed aliases.
struct Directory(Vec<AgentCard>);

#[async_trait]
impl AgentDirectory for Directory {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        self.0
            .iter()
            .find(|card| &card.alias == alias)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("agent {alias}")))
    }

    async fn versioned_agent(&self, alias: &Alias) -> Result<Agent> {
        let card = self.agent(alias).await?;
        Ok(Agent { card, version: 1 })
    }
}

/// Treats `LEAKED` as the only active token.
struct OneActiveToken;

#[async_trait]
impl Credentials for OneActiveToken {
    async fn create_operator(&self, _: &Alias) -> Result<Secret> {
        unimplemented!()
    }
    async fn issue_run_token(&self, _: &Alias, _: RunId, _: CorrelationId) -> Result<Secret> {
        unimplemented!()
    }
    async fn revoke_run_token(&self, _: RunId) -> Result<()> {
        unimplemented!()
    }
    async fn revoke_all_run_tokens(&self) -> Result<()> {
        unimplemented!()
    }
    async fn authenticate(&self, _: &str) -> Result<Caller> {
        unimplemented!()
    }
    async fn contains_active_run_token(&self, content: &str) -> Result<bool> {
        Ok(content.contains(LEAKED))
    }
}

/// Keeps every recorded event.
#[derive(Default)]
struct Recorded(Mutex<Vec<Event>>);

#[async_trait]
impl JournalWriter for Recorded {
    async fn record(&self, event: Event) -> Result<()> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

struct Setup {
    broker: SqliteBroker,
    journal: Arc<Recorded>,
    subscription: Mutex<Notifications>,
}

impl Setup {
    /// Aliases notified since the last call.
    fn notified(&self) -> Vec<Alias> {
        let mut subscription = self.subscription.lock().unwrap();
        std::iter::from_fn(|| subscription.try_recv().ok()).collect()
    }
}

fn alias(name: &str) -> Alias {
    name.parse().unwrap()
}

fn card(name: &str, allow_message_to: &[&str]) -> AgentCard {
    AgentCard {
        alias: alias(name),
        title: String::new(),
        model: ModelRef {
            provider: "fake".to_owned(),
            id: "fake".to_owned(),
        },
        access: Vec::new(),
        allow_message_to: allow_message_to.iter().map(|a| alias(a)).collect(),
        prompt: String::new(),
        hue: None,
    }
}

/// `@manager` and `@coder` may message each other; `@coder` may also message the operator.
async fn setup() -> Setup {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let directory = Directory(vec![
        card("manager", &["coder"]),
        card("coder", &["manager", "user"]),
    ]);
    let journal = Arc::new(Recorded::default());
    let broker = SqliteBroker::new(
        pool,
        Arc::new(directory),
        Arc::new(OneActiveToken),
        journal.clone(),
    )
    .await
    .unwrap();
    let subscription = Mutex::new(broker.subscribe());
    Setup {
        broker,
        journal,
        subscription,
    }
}

fn agent(name: &str, correlation_id: CorrelationId) -> Caller {
    Caller::Agent {
        alias: alias(name),
        run_id: RunId::new(),
        correlation_id,
    }
}

fn operator() -> Caller {
    Caller::Operator {
        alias: alias("user"),
    }
}

fn to(name: &str, body: &str, task_id: Option<TaskId>) -> NewMessage {
    NewMessage {
        to: alias(name),
        body: body.to_owned(),
        subject: None,
        task_id,
        in_reply_to: None,
    }
}

#[tokio::test]
async fn agent_message_takes_sender_and_chain_from_the_run() {
    let s = setup().await;
    let chain = CorrelationId::new();
    let sent = s
        .broker
        .send(&agent("manager", chain), to("coder", "hi", None))
        .await
        .unwrap();
    assert_eq!(sent.from, alias("manager"));
    assert_eq!(sent.correlation_id, chain);
    assert_eq!(s.notified(), [alias("coder")]);
}

#[tokio::test]
async fn agent_cannot_message_outside_its_list() {
    let s = setup().await;
    let result = s
        .broker
        .send(
            &agent("manager", CorrelationId::new()),
            to("user", "hi", None),
        )
        .await;
    assert!(matches!(result, Err(Error::Forbidden(_))));
    assert!(s.broker.agents_with_unread().await.unwrap().is_empty());

    let events = s.journal.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor, alias("manager"));
    assert_eq!(events[0].action, "send_message");
    assert_eq!(events[0].decision, Some(Decision::Deny));
}

#[tokio::test]
async fn operator_starts_a_new_chain() {
    let s = setup().await;
    let first = s
        .broker
        .send(&operator(), to("manager", "one", None))
        .await
        .unwrap();
    let second = s
        .broker
        .send(&operator(), to("manager", "two", None))
        .await
        .unwrap();
    assert_ne!(first.correlation_id, second.correlation_id);
}

#[tokio::test]
async fn operator_reply_stays_in_the_chain() {
    let s = setup().await;
    let chain = CorrelationId::new();
    let question = s
        .broker
        .send(&agent("coder", chain), to("user", "which db?", None))
        .await
        .unwrap();
    let mut answer = to("coder", "sqlite", None);
    answer.in_reply_to = Some(question.id);
    let answer = s.broker.send(&operator(), answer).await.unwrap();
    assert_eq!(answer.correlation_id, chain);
}

#[tokio::test]
async fn reply_to_unknown_message_is_rejected() {
    let s = setup().await;
    let mut reply = to("coder", "hi", None);
    reply.in_reply_to = Some(MessageId::new());
    assert!(matches!(
        s.broker.send(&operator(), reply).await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn operator_cannot_message_unknown_agent() {
    let s = setup().await;
    assert!(matches!(
        s.broker.send(&operator(), to("ghost", "hi", None)).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn message_with_active_token_is_rejected() {
    let s = setup().await;
    let caller = agent("coder", CorrelationId::new());
    let mut message = to("manager", "fine", None);
    message.subject = Some(format!("token {LEAKED}"));
    assert!(matches!(
        s.broker.send(&caller, message).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        s.broker
            .send(&caller, to("manager", &format!("here: {LEAKED}"), None))
            .await,
        Err(Error::Forbidden(_))
    ));
    assert!(s.notified().is_empty());
    let events = s.journal.0.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.decision == Some(Decision::Deny)));
}

#[tokio::test]
async fn message_to_the_operator_notifies_nobody() {
    let s = setup().await;
    let chain = CorrelationId::new();
    s.broker
        .send(&agent("coder", chain), to("user", "done", None))
        .await
        .unwrap();
    assert!(s.notified().is_empty());
    assert!(s.broker.agents_with_unread().await.unwrap().is_empty());
    let inbox = s
        .broker
        .read_messages(&operator(), SessionKey::Chain(chain))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
}

#[tokio::test]
async fn adapters_are_not_supported_yet() {
    let s = setup().await;
    let adapter = Caller::Adapter {
        alias: alias("gitlab"),
    };
    assert!(matches!(
        s.broker.send(&adapter, to("manager", "hi", None)).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn reading_is_per_session_and_keeps_the_cursor() {
    let s = setup().await;
    let task = TaskId::new();
    s.broker
        .send(&operator(), to("coder", "on task", Some(task)))
        .await
        .unwrap();
    let no_task = s
        .broker
        .send(&operator(), to("coder", "no task", None))
        .await
        .unwrap();
    let coder = agent("coder", CorrelationId::new());

    let on_task = s
        .broker
        .read_messages(&coder, SessionKey::Task(task))
        .await
        .unwrap();
    assert_eq!(on_task.len(), 1);
    assert_eq!(on_task[0].body, "on task");
    // Reading again returns the same: only a finished Run moves the cursor (D49).
    assert_eq!(
        s.broker
            .read_messages(&coder, SessionKey::Task(task))
            .await
            .unwrap(),
        on_task
    );
    let chain = s
        .broker
        .read_messages(&coder, no_task.session())
        .await
        .unwrap();
    assert_eq!(chain, [no_task]);
    assert_eq!(
        s.broker
            .unread_in(&alias("coder"), SessionKey::Task(task))
            .await
            .unwrap(),
        on_task
    );
}

#[tokio::test]
async fn messages_without_a_task_are_split_by_chain() {
    let s = setup().await;
    let first = s
        .broker
        .send(&operator(), to("coder", "first chain", None))
        .await
        .unwrap();
    s.broker
        .send(&operator(), to("coder", "second chain", None))
        .await
        .unwrap();
    let mut reply = to("coder", "first chain again", None);
    reply.in_reply_to = Some(first.id);
    s.broker.send(&operator(), reply).await.unwrap();

    let batch = s.broker.next_batch(&alias("coder")).await.unwrap().unwrap();
    assert_eq!(batch.session, SessionKey::Chain(first.correlation_id));
    let bodies: Vec<_> = batch.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["first chain", "first chain again"]);
}

#[tokio::test]
async fn batches_follow_the_oldest_message() {
    let s = setup().await;
    let (a, b) = (TaskId::new(), TaskId::new());
    for (body, task) in [("a1", a), ("b1", b), ("a2", a)] {
        s.broker
            .send(&operator(), to("coder", body, Some(task)))
            .await
            .unwrap();
    }
    let coder = alias("coder");

    let first = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(first.session, SessionKey::Task(a));
    let bodies: Vec<_> = first.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["a1", "a2"]);

    let last = first.messages.last().unwrap().id;
    s.broker
        .advance_cursor(&coder, first.session, last)
        .await
        .unwrap();

    let second = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(second.session, SessionKey::Task(b));
    s.broker
        .advance_cursor(&coder, second.session, second.messages[0].id)
        .await
        .unwrap();

    assert_eq!(s.broker.next_batch(&coder).await.unwrap(), None);
    assert!(s.broker.agents_with_unread().await.unwrap().is_empty());
}

#[tokio::test]
async fn cursor_never_moves_back() {
    let s = setup().await;
    let task = TaskId::new();
    let first = s
        .broker
        .send(&operator(), to("coder", "1", Some(task)))
        .await
        .unwrap();
    let second = s
        .broker
        .send(&operator(), to("coder", "2", Some(task)))
        .await
        .unwrap();
    let coder = alias("coder");
    let session = SessionKey::Task(task);
    s.broker
        .advance_cursor(&coder, session, second.id)
        .await
        .unwrap();
    s.broker
        .advance_cursor(&coder, session, first.id)
        .await
        .unwrap();
    assert_eq!(s.broker.next_batch(&coder).await.unwrap(), None);
}

#[tokio::test]
async fn cursor_only_moves_within_its_session() {
    let s = setup().await;
    let message = s
        .broker
        .send(&operator(), to("coder", "hi", Some(TaskId::new())))
        .await
        .unwrap();
    let other_session = SessionKey::Chain(message.correlation_id);
    assert!(matches!(
        s.broker
            .advance_cursor(&alias("coder"), other_session, message.id)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        s.broker
            .advance_cursor(&alias("manager"), message.session(), message.id)
            .await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn startup_reconcile_lists_agents_with_unread() {
    let s = setup().await;
    s.broker
        .send(&operator(), to("manager", "hi", None))
        .await
        .unwrap();
    s.broker
        .send(&operator(), to("coder", "hi", None))
        .await
        .unwrap();
    assert_eq!(
        s.broker.agents_with_unread().await.unwrap(),
        [alias("coder"), alias("manager")]
    );
}

#[tokio::test]
async fn failures_count_in_a_row_until_a_run_succeeds() {
    let s = setup().await;
    let sent = s
        .broker
        .send(&operator(), to("coder", "a", None))
        .await
        .unwrap();
    let (coder, session) = (alias("coder"), sent.session());

    assert_eq!(
        s.broker
            .record_failure(&coder, session, "boom")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        s.broker
            .record_failure(&coder, session, "bang")
            .await
            .unwrap(),
        2
    );
    let status = s.broker.inbox_status(&operator()).await.unwrap();
    assert_eq!(status[0].failing[0].failures, 2);
    assert_eq!(status[0].failing[0].last_error, "bang");

    s.broker
        .advance_cursor(&coder, session, sent.id)
        .await
        .unwrap();
    assert_eq!(
        s.broker
            .record_failure(&coder, session, "again")
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn stuck_sessions_are_not_delivered_until_retried() {
    let s = setup().await;
    let first = s
        .broker
        .send(&operator(), to("coder", "a", None))
        .await
        .unwrap();
    let second = s
        .broker
        .send(&operator(), to("coder", "b", None))
        .await
        .unwrap();
    let coder = alias("coder");

    s.broker
        .record_failure(&coder, first.session(), "boom")
        .await
        .unwrap();
    s.broker.mark_stuck(&coder, first.session()).await.unwrap();
    // Other sessions go on.
    let batch = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(batch.session, second.session());

    s.notified();
    s.broker
        .retry(&operator(), &coder, first.session())
        .await
        .unwrap();
    assert_eq!(s.notified(), std::slice::from_ref(&coder));
    let batch = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(batch.session, first.session());
}

#[tokio::test]
async fn skip_marks_the_session_read_and_forgets_failures() {
    let s = setup().await;
    let sent = s
        .broker
        .send(&operator(), to("coder", "a", None))
        .await
        .unwrap();
    let coder = alias("coder");
    s.broker
        .record_failure(&coder, sent.session(), "boom")
        .await
        .unwrap();
    s.broker.mark_stuck(&coder, sent.session()).await.unwrap();

    s.broker
        .skip(&operator(), &coder, sent.session())
        .await
        .unwrap();
    assert!(s.broker.next_batch(&coder).await.unwrap().is_none());
    assert!(s.broker.inbox_status(&operator()).await.unwrap().is_empty());
    // Nothing is left to skip or retry.
    let skip = s.broker.skip(&operator(), &coder, sent.session()).await;
    assert!(matches!(skip, Err(Error::NotFound(_))));
    let retry = s.broker.retry(&operator(), &coder, sent.session()).await;
    assert!(matches!(retry, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn inbox_status_shows_agents_only_to_the_operator() {
    let s = setup().await;
    s.broker
        .send(&operator(), to("coder", "a", None))
        .await
        .unwrap();
    let chain = CorrelationId::new();
    s.broker
        .send(&agent("coder", chain), to("user", "hi", None))
        .await
        .unwrap();

    let status = s.broker.inbox_status(&operator()).await.unwrap();
    // The operator's own mail is not listed.
    assert_eq!(status.len(), 1);
    assert_eq!((status[0].agent.as_str(), status[0].unread), ("coder", 1));

    let session = SessionKey::Chain(chain);
    for denied in [
        s.broker
            .inbox_status(&agent("coder", chain))
            .await
            .map(drop),
        s.broker
            .retry(&agent("coder", chain), &alias("coder"), session)
            .await,
        s.broker
            .skip(&agent("coder", chain), &alias("coder"), session)
            .await,
    ] {
        assert!(matches!(denied, Err(Error::Forbidden(_))));
    }
}

#[tokio::test]
async fn a_stopped_chain_waits_for_resume() {
    let s = setup().await;
    let stopped = s
        .broker
        .send(&operator(), to("coder", "a", None))
        .await
        .unwrap();
    let going = s
        .broker
        .send(&operator(), to("coder", "b", None))
        .await
        .unwrap();
    let (coder, chain) = (alias("coder"), stopped.correlation_id);

    s.broker.stop_chain(&operator(), chain).await.unwrap();
    s.broker.stop_chain(&operator(), chain).await.unwrap();
    assert!(s.broker.is_stopped(chain).await.unwrap());
    let chains = s.broker.stopped_chains(&operator()).await.unwrap();
    assert_eq!(chains.len(), 1);
    assert_eq!(
        (chains[0].chain, chains[0].reason.as_deref()),
        (chain, None)
    );
    let batch = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(batch.session, going.session());
    s.broker
        .advance_cursor(&coder, going.session(), going.id)
        .await
        .unwrap();
    assert!(s.broker.next_batch(&coder).await.unwrap().is_none());

    s.notified();
    s.broker.resume_chain(&operator(), chain).await.unwrap();
    assert_eq!(s.notified(), std::slice::from_ref(&coder));
    let batch = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(batch.messages[0].id, stopped.id);
    let again = s.broker.resume_chain(&operator(), chain).await;
    assert!(matches!(again, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn stopping_a_chain_leaves_other_chains_of_the_task() {
    let s = setup().await;
    let task = Some(TaskId::new());
    let stopped = s
        .broker
        .send(&operator(), to("coder", "a", task))
        .await
        .unwrap();
    let going = s
        .broker
        .send(&operator(), to("coder", "b", task))
        .await
        .unwrap();
    let coder = alias("coder");

    s.broker
        .stop_chain(&operator(), stopped.correlation_id)
        .await
        .unwrap();
    let batch = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(
        batch.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        [going.id]
    );
    let steerable = s.broker.unread_in(&coder, going.session()).await.unwrap();
    assert_eq!(steerable.len(), 1);
    let chain = going.correlation_id;
    let read = s
        .broker
        .read_messages(&agent("coder", chain), going.session())
        .await;
    assert_eq!(read.unwrap().len(), 1);
}

#[tokio::test]
async fn the_operator_still_reads_a_stopped_chain() {
    let s = setup().await;
    let chain = CorrelationId::new();
    let sent = s
        .broker
        .send(&agent("coder", chain), to("user", "done", None))
        .await
        .unwrap();
    s.broker.stop_chain(&operator(), chain).await.unwrap();

    let read = s
        .broker
        .read_messages(&operator(), sent.session())
        .await
        .unwrap();
    assert_eq!(read[0].id, sent.id);
}

#[tokio::test]
async fn agents_cannot_write_into_a_stopped_chain() {
    let s = setup().await;
    let chain = CorrelationId::new();
    s.broker.stop_chain(&operator(), chain).await.unwrap();

    let sent = s
        .broker
        .send(&agent("coder", chain), to("manager", "more", None))
        .await;
    assert!(matches!(sent, Err(Error::Forbidden(_))));
    let denied = s.journal.0.lock().unwrap().pop().unwrap();
    assert_eq!(denied.reason.as_deref(), Some("the chain is stopped"));
}

#[tokio::test]
async fn the_operator_may_write_into_a_stopped_chain() {
    let s = setup().await;
    let question = s
        .broker
        .send(
            &agent("coder", CorrelationId::new()),
            to("user", "which db?", None),
        )
        .await
        .unwrap();
    let chain = question.correlation_id;
    s.broker.stop_chain(&operator(), chain).await.unwrap();

    let mut answer = to("coder", "postgres", None);
    answer.in_reply_to = Some(question.id);
    let answer = s.broker.send(&operator(), answer).await.unwrap();
    assert_eq!(answer.correlation_id, chain);
    // Kept for later, delivered after `resume`.
    assert!(
        s.broker
            .next_batch(&alias("coder"))
            .await
            .unwrap()
            .is_none()
    );
    s.broker.resume_chain(&operator(), chain).await.unwrap();
    assert!(
        s.broker
            .next_batch(&alias("coder"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn only_the_operator_stops_and_resumes_chains() {
    let s = setup().await;
    let chain = CorrelationId::new();
    let coder = agent("coder", chain);
    for denied in [
        s.broker.stop_chain(&coder, chain).await,
        s.broker.resume_chain(&coder, chain).await,
        s.broker.stopped_chains(&coder).await.map(drop),
    ] {
        assert!(matches!(denied, Err(Error::Forbidden(_))));
    }
}

#[tokio::test]
async fn a_halted_chain_keeps_its_reason_and_resume_restarts_its_count() {
    let s = setup().await;
    let chain = CorrelationId::new();
    assert_eq!(s.broker.resumed_at(chain).await.unwrap(), None);

    s.broker
        .halt_chain(chain, "the chain reached its limit of 30 Runs")
        .await
        .unwrap();
    // A second trip, or the operator stopping it too, keeps the first reason.
    s.broker.halt_chain(chain, "another").await.unwrap();
    s.broker.stop_chain(&operator(), chain).await.unwrap();
    assert!(s.broker.is_stopped(chain).await.unwrap());
    let stopped = s.broker.stopped_chains(&operator()).await.unwrap();
    assert_eq!(
        stopped[0].reason.as_deref(),
        Some("the chain reached its limit of 30 Runs")
    );

    let before = Timestamp::now();
    s.broker.resume_chain(&operator(), chain).await.unwrap();
    let resumed = s.broker.resumed_at(chain).await.unwrap().unwrap();
    assert!(resumed >= before);
    assert!(
        s.broker
            .stopped_chains(&operator())
            .await
            .unwrap()
            .is_empty()
    );
}
