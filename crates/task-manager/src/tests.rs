use super::*;
use control_plane_common::registry::{AgentCard, ModelRef, Secret};
use control_plane_common::tasks::TaskResult;
use control_plane_common::{CorrelationId, RunId};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const LEAKED: &str = "sona_plane_rt_leaked";

struct Directory(Vec<Alias>);

#[async_trait]
impl AgentDirectory for Directory {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        if !self.0.contains(alias) {
            return Err(Error::NotFound(format!("agent {alias}")));
        }
        Ok(AgentCard {
            alias: alias.clone(),
            title: String::new(),
            model: ModelRef {
                provider: "fake".to_owned(),
                id: "fake".to_owned(),
            },
            allow_tools: Vec::new(),
            allow_message_to: Vec::new(),
            prompt: String::new(),
        })
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

async fn manager() -> SqliteTaskManager {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let agents = ["manager", "coder", "tester", "stranger"]
        .map(alias)
        .to_vec();
    SqliteTaskManager::new(pool, Arc::new(Directory(agents)), Arc::new(OneActiveToken))
        .await
        .unwrap()
}

fn alias(name: &str) -> Alias {
    name.parse().unwrap()
}

fn agent(name: &str) -> Caller {
    Caller::Agent {
        alias: alias(name),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    }
}

fn operator() -> Caller {
    Caller::Operator {
        alias: alias("user"),
    }
}

/// Created by `@manager`, assigned to `@coder`, with `@tester` as a member.
fn new_task() -> NewTask {
    NewTask {
        title: "Parse config".to_owned(),
        context: "Use TOML.".to_owned(),
        parent_id: None,
        assignee: Some(alias("coder")),
        members: vec![alias("tester")],
        deadline: None,
    }
}

async fn created(manager: &SqliteTaskManager) -> Task {
    manager
        .create_task(&agent("manager"), new_task())
        .await
        .unwrap()
}

fn status(to: TaskStatus) -> TaskUpdate {
    TaskUpdate {
        status: Some(to),
        ..TaskUpdate::default()
    }
}

#[tokio::test]
async fn created_task_is_open_and_attributed() {
    let m = manager().await;
    let task = created(&m).await;
    assert_eq!(task.status, TaskStatus::Open);
    assert_eq!(task.creator, alias("manager"));

    let view = m.get_task(&agent("coder"), task.id).await.unwrap();
    assert_eq!(view.task, task);
    assert_eq!(view.history.len(), 1);
    assert_eq!(view.history[0].change, Change::Created);
    assert_eq!(view.history[0].who, alias("manager"));
}

#[tokio::test]
async fn only_participants_and_operator_see_a_task() {
    let m = manager().await;
    let task = created(&m).await;
    for who in [
        agent("manager"),
        agent("coder"),
        agent("tester"),
        operator(),
    ] {
        assert!(m.get_task(&who, task.id).await.is_ok());
    }
    // An outsider gets the same answer as for a missing task.
    assert!(matches!(
        m.get_task(&agent("stranger"), task.id).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        m.update_task(&agent("stranger"), task.id, status(TaskStatus::InProgress))
            .await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn participants_move_work_but_only_creator_closes() {
    let m = manager().await;
    let task = created(&m).await;

    m.update_task(&agent("coder"), task.id, status(TaskStatus::InProgress))
        .await
        .unwrap();
    m.update_task(&agent("tester"), task.id, status(TaskStatus::NeedsOwner))
        .await
        .unwrap();
    assert!(matches!(
        m.update_task(&agent("coder"), task.id, status(TaskStatus::Done))
            .await,
        Err(Error::Forbidden(_))
    ));
    let done = m
        .update_task(&agent("manager"), task.id, status(TaskStatus::Done))
        .await
        .unwrap();
    assert_eq!(done.status, TaskStatus::Done);
}

#[tokio::test]
async fn operator_may_close_any_task() {
    let m = manager().await;
    let task = created(&m).await;
    m.update_task(&operator(), task.id, status(TaskStatus::Cancelled))
        .await
        .unwrap();
}

#[tokio::test]
async fn closed_task_never_changes() {
    let m = manager().await;
    let task = created(&m).await;
    m.update_task(&agent("manager"), task.id, status(TaskStatus::Done))
        .await
        .unwrap();
    let note = TaskUpdate {
        note: Some("one more thing".to_owned()),
        ..TaskUpdate::default()
    };
    assert!(matches!(
        m.update_task(&agent("manager"), task.id, note).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        m.update_task(&operator(), task.id, status(TaskStatus::Open))
            .await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn result_and_note_land_in_history() {
    let m = manager().await;
    let task = created(&m).await;
    let result = TaskResult {
        text: "Done with serde".to_owned(),
        links: vec!["https://example.com/mr/1".to_owned()],
    };
    let updated = m
        .update_task(
            &agent("coder"),
            task.id,
            TaskUpdate {
                status: Some(TaskStatus::InProgress),
                result: Some(result.clone()),
                note: Some("see the MR".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.result.as_ref(), Some(&result));

    let view = m.get_task(&operator(), task.id).await.unwrap();
    assert_eq!(view.task, updated);
    let changes: Vec<_> = view.history.into_iter().map(|e| e.change).collect();
    assert_eq!(
        changes,
        [
            Change::Created,
            Change::Status {
                from: TaskStatus::Open,
                to: TaskStatus::InProgress
            },
            Change::Result { result },
            Change::Note {
                text: "see the MR".to_owned()
            },
        ]
    );
}

#[tokio::test]
async fn empty_or_repeated_updates_are_rejected() {
    let m = manager().await;
    let task = created(&m).await;
    assert!(matches!(
        m.update_task(&agent("coder"), task.id, TaskUpdate::default())
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        m.update_task(&agent("coder"), task.id, status(TaskStatus::Open))
            .await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn texts_with_active_token_are_rejected() {
    let m = manager().await;
    let mut leaky = new_task();
    leaky.context = format!("token: {LEAKED}");
    assert!(matches!(
        m.create_task(&agent("manager"), leaky).await,
        Err(Error::Forbidden(_))
    ));

    let task = created(&m).await;
    let leaky_result = TaskUpdate {
        result: Some(TaskResult {
            text: "ok".to_owned(),
            links: vec![format!("https://x/?t={LEAKED}")],
        }),
        ..TaskUpdate::default()
    };
    assert!(matches!(
        m.update_task(&agent("coder"), task.id, leaky_result).await,
        Err(Error::Forbidden(_))
    ));
    let leaky_note = TaskUpdate {
        note: Some(LEAKED.to_owned()),
        ..TaskUpdate::default()
    };
    assert!(matches!(
        m.update_task(&agent("coder"), task.id, leaky_note).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn subtask_needs_a_visible_parent() {
    let m = manager().await;
    let parent = created(&m).await;
    let mut sub = new_task();
    sub.parent_id = Some(parent.id);

    let child = m.create_task(&agent("coder"), sub.clone()).await.unwrap();
    assert_eq!(child.parent_id, Some(parent.id));
    assert!(matches!(
        m.create_task(&agent("stranger"), sub).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn assignee_and_members_must_be_agents() {
    let m = manager().await;
    let mut task = new_task();
    task.members.push(alias("ghost"));
    assert!(matches!(
        m.create_task(&agent("manager"), task).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn adapters_are_not_supported_yet() {
    let m = manager().await;
    let adapter = Caller::Adapter {
        alias: alias("gitlab"),
    };
    assert!(matches!(
        m.create_task(&adapter, new_task()).await,
        Err(Error::Forbidden(_))
    ));
}
