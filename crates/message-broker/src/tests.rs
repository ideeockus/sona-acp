use super::*;
use control_plane_common::RunId;
use control_plane_common::registry::{AgentCard, ModelRef, Secret};
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

#[derive(Default)]
struct Woken(Mutex<Vec<Alias>>);

#[async_trait]
impl WakeSink for Woken {
    async fn wake(&self, agent: &Alias) {
        self.0.lock().unwrap().push(agent.clone());
    }
}

struct Setup {
    broker: SqliteBroker,
    journal: Arc<Recorded>,
    woken: Arc<Woken>,
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
        allow_tools: Vec::new(),
        allow_message_to: allow_message_to.iter().map(|a| alias(a)).collect(),
        prompt: String::new(),
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
    let woken = Arc::new(Woken::default());
    let broker = SqliteBroker::new(
        pool,
        Arc::new(directory),
        Arc::new(OneActiveToken),
        journal.clone(),
        woken.clone(),
    )
    .await
    .unwrap();
    Setup {
        broker,
        journal,
        woken,
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
    assert_eq!(*s.woken.0.lock().unwrap(), [alias("coder")]);
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
    assert!(s.woken.0.lock().unwrap().is_empty());
    let events = s.journal.0.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.decision == Some(Decision::Deny)));
}

#[tokio::test]
async fn message_to_the_operator_wakes_nobody() {
    let s = setup().await;
    s.broker
        .send(
            &agent("coder", CorrelationId::new()),
            to("user", "done", None),
        )
        .await
        .unwrap();
    assert!(s.woken.0.lock().unwrap().is_empty());
    assert!(s.broker.agents_with_unread().await.unwrap().is_empty());
    assert_eq!(
        s.broker
            .read_messages(&operator(), None)
            .await
            .unwrap()
            .len(),
        1
    );
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
    s.broker
        .send(&operator(), to("coder", "no task", None))
        .await
        .unwrap();
    let coder = agent("coder", CorrelationId::new());

    let on_task = s.broker.read_messages(&coder, Some(task)).await.unwrap();
    assert_eq!(on_task.len(), 1);
    assert_eq!(on_task[0].body, "on task");
    // Reading again returns the same: only a finished Run moves the cursor (D49).
    assert_eq!(
        s.broker.read_messages(&coder, Some(task)).await.unwrap(),
        on_task
    );
    assert_eq!(
        s.broker.read_messages(&coder, None).await.unwrap()[0].body,
        "no task"
    );
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
    assert_eq!(first.task_id, Some(a));
    let bodies: Vec<_> = first.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["a1", "a2"]);

    let last = first.messages.last().unwrap().id;
    s.broker
        .advance_cursor(&coder, Some(a), last)
        .await
        .unwrap();

    let second = s.broker.next_batch(&coder).await.unwrap().unwrap();
    assert_eq!(second.task_id, Some(b));
    s.broker
        .advance_cursor(&coder, Some(b), second.messages[0].id)
        .await
        .unwrap();

    assert_eq!(s.broker.next_batch(&coder).await.unwrap(), None);
    assert!(s.broker.agents_with_unread().await.unwrap().is_empty());
}

#[tokio::test]
async fn cursor_never_moves_back() {
    let s = setup().await;
    let first = s
        .broker
        .send(&operator(), to("coder", "1", None))
        .await
        .unwrap();
    let second = s
        .broker
        .send(&operator(), to("coder", "2", None))
        .await
        .unwrap();
    let coder = alias("coder");
    s.broker
        .advance_cursor(&coder, None, second.id)
        .await
        .unwrap();
    s.broker
        .advance_cursor(&coder, None, first.id)
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
    assert!(matches!(
        s.broker
            .advance_cursor(&alias("coder"), None, message.id)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        s.broker
            .advance_cursor(&alias("manager"), message.task_id, message.id)
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
