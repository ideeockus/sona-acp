//! The external HTTP API: JSON over HTTP for our Pi extension and the operator CLI (D52, D85).
//!
//! Every handler authenticates the caller first and passes it to the component
//! explicitly (D11). The components decide what the caller may do.

mod extract;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use control_plane_common::broker::{
    Conversation, DeliveryControl, Message, MessageBroker, NewMessage,
};
use control_plane_common::gateway::{ToolCall, ToolGateway, Verdict};
use control_plane_common::journal::{JournalEntry, JournalFilter, JournalReader};
use control_plane_common::registry::{Agent, AgentCard, AgentRegistry, AgentVersion, Credentials};
use control_plane_common::runtime::{RunControl, RunFilter, RunRecord};
use control_plane_common::status::{AgentStatus, Status};
use control_plane_common::tasks::{NewTask, Task, TaskManager, TaskStatus, TaskUpdate, TaskView};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, RunId, SessionKey, TaskId,
};
use extract::{ApiError, Authenticated, Body};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The components the API exposes, each through its external trait.
pub struct Services {
    pub credentials: Arc<dyn Credentials>,
    pub registry: Arc<dyn AgentRegistry>,
    pub broker: Arc<dyn MessageBroker>,
    pub delivery: Arc<dyn DeliveryControl>,
    pub runs: Arc<dyn RunControl>,
    pub tasks: Arc<dyn TaskManager>,
    pub gateway: Arc<dyn ToolGateway>,
    pub journal: Arc<dyn JournalReader>,
}

type Shared = State<Arc<Services>>;
type Reply<T> = Result<Json<T>, ApiError>;

pub fn router(services: Services) -> Router {
    Router::new()
        .route("/v1/messages", post(send_message))
        .route("/v1/sessions/{session}/messages", get(read_messages))
        .route("/v1/conversations", get(conversations))
        .route("/v1/conversations/{agent}/messages", get(conversation))
        .route("/v1/conversations/{agent}/read", post(mark_read))
        .route("/v1/tasks", post(create_task).get(list_tasks))
        .route("/v1/tasks/{id}", get(get_task).patch(update_task))
        .route("/v1/gateway/check", post(check_tool))
        .route("/v1/agents", post(create_agent).get(list_agents))
        .route(
            "/v1/agents/{alias}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/v1/agents/{alias}/disable", post(disable_agent))
        .route("/v1/agents/{alias}/enable", post(enable_agent))
        .route("/v1/agents/{alias}/versions", get(agent_versions))
        .route("/v1/agents/{alias}/sessions/{session}/retry", post(retry))
        .route("/v1/agents/{alias}/sessions/{session}/skip", post(skip))
        .route("/v1/chains/{id}/stop", post(stop_chain))
        .route("/v1/chains/{id}/resume", post(resume_chain))
        .route("/v1/status", get(status))
        .route("/v1/runs", get(list_runs))
        .route("/v1/runs/{id}", get(get_run))
        .route("/v1/journal", get(read_journal))
        .with_state(Arc::new(services))
}

/// A sent message, and whether it waits for its chain to be resumed (D27).
#[derive(Serialize)]
struct Sent {
    #[serde(flatten)]
    message: Message,
    chain_stopped: bool,
}

async fn send_message(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Body(message): Body<NewMessage>,
) -> Result<(StatusCode, Json<Sent>), ApiError> {
    let message = s.broker.send(&caller, message).await?;
    // Only the operator can write into a stopped chain; agents are refused.
    let chain_stopped = matches!(caller, Caller::Operator { .. })
        && s.delivery
            .stopped_chains(&caller)
            .await?
            .iter()
            .any(|stopped| stopped.chain == message.correlation_id);
    Ok((
        StatusCode::CREATED,
        Json(Sent {
            message,
            chain_stopped,
        }),
    ))
}

async fn read_messages(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(session): Path<String>,
) -> Reply<Vec<Message>> {
    let session: SessionKey = session.parse().map_err(Error::InvalidInput)?;
    Ok(Json(s.broker.read_messages(&caller, session).await?))
}

async fn conversations(
    State(s): Shared,
    Authenticated(caller): Authenticated,
) -> Reply<Vec<Conversation>> {
    Ok(Json(s.broker.conversations(&caller).await?))
}

#[derive(Deserialize)]
struct PageQuery {
    before: Option<String>,
}

async fn conversation(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(agent): Path<String>,
    Query(query): Query<PageQuery>,
) -> Reply<Vec<Message>> {
    let agent: Alias = parse(&agent)?;
    let before = query.before.as_deref().map(parse).transpose()?;
    Ok(Json(s.broker.conversation(&caller, &agent, before).await?))
}

#[derive(Deserialize)]
struct ReadUpTo {
    up_to: MessageId,
}

async fn mark_read(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(agent): Path<String>,
    Body(read): Body<ReadUpTo>,
) -> Result<StatusCode, ApiError> {
    let agent: Alias = parse(&agent)?;
    s.broker.mark_read(&caller, &agent, read.up_to).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_task(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Body(task): Body<NewTask>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let created = s.tasks.create_task(&caller, task).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Deserialize)]
struct TasksQuery {
    status: Option<String>,
}

async fn list_tasks(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Query(query): Query<TasksQuery>,
) -> Reply<Vec<Task>> {
    let status = query
        .status
        .map(|status| {
            serde_json::from_value::<TaskStatus>(serde_json::Value::String(status.clone()))
                .map_err(|_| Error::InvalidInput(format!("invalid status {status:?}")))
        })
        .transpose()?;
    Ok(Json(s.tasks.list_tasks(&caller, status).await?))
}

async fn get_task(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Reply<TaskView> {
    Ok(Json(s.tasks.get_task(&caller, parse(&id)?).await?))
}

async fn update_task(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
    Body(update): Body<TaskUpdate>,
) -> Reply<Task> {
    Ok(Json(
        s.tasks.update_task(&caller, parse(&id)?, update).await?,
    ))
}

async fn check_tool(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Body(call): Body<ToolCall>,
) -> Reply<Verdict> {
    Ok(Json(s.gateway.check(&caller, call).await?))
}

async fn create_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Body(card): Body<AgentCard>,
) -> Result<StatusCode, ApiError> {
    s.registry.create_agent(&caller, card).await?;
    Ok(StatusCode::CREATED)
}

async fn list_agents(State(s): Shared, Authenticated(caller): Authenticated) -> Reply<Vec<Agent>> {
    Ok(Json(s.registry.list_agents(&caller).await?))
}

async fn get_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
) -> Reply<Agent> {
    let alias: Alias = parse(&alias)?;
    Ok(Json(s.registry.get_agent(&caller, &alias).await?))
}

/// The whole card with the version it was based on (D101).
async fn update_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
    Body(edit): Body<Agent>,
) -> Reply<Agent> {
    let alias: Alias = parse(&alias)?;
    if edit.card.alias != alias {
        return Err(Error::InvalidInput("an agent's alias never changes".to_owned()).into());
    }
    Ok(Json(s.registry.update_agent(&caller, edit).await?))
}

/// Registry first, so no new Run starts; then the running one is aborted (D101).
async fn disable_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
) -> Result<StatusCode, ApiError> {
    let alias: Alias = parse(&alias)?;
    s.registry.disable_agent(&caller, &alias).await?;
    s.runs
        .abort_agent(&caller, &alias, "the agent was disabled")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The agent gets the mail that waited while it was disabled.
async fn enable_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
) -> Result<StatusCode, ApiError> {
    let alias: Alias = parse(&alias)?;
    s.registry.enable_agent(&caller, &alias).await?;
    s.delivery.deliver_pending(&caller, &alias).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
) -> Result<StatusCode, ApiError> {
    let alias: Alias = parse(&alias)?;
    s.registry.delete_agent(&caller, &alias).await?;
    s.runs
        .abort_agent(&caller, &alias, "the agent was deleted")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn agent_versions(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
) -> Reply<Vec<AgentVersion>> {
    let alias: Alias = parse(&alias)?;
    Ok(Json(s.registry.agent_versions(&caller, &alias).await?))
}

async fn retry(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path((alias, session)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let session: SessionKey = session.parse().map_err(Error::InvalidInput)?;
    s.delivery.retry(&caller, &parse(&alias)?, session).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn skip(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path((alias, session)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let session: SessionKey = session.parse().map_err(Error::InvalidInput)?;
    s.delivery.skip(&caller, &parse(&alias)?, session).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Stops the chain in Broker first, so that nothing starts it again, then aborts its
/// active Runs (D27).
async fn stop_chain(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let chain: CorrelationId = parse(&id)?;
    s.delivery.stop_chain(&caller, chain).await?;
    s.runs.abort_chain(&caller, chain).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_chain(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.delivery.resume_chain(&caller, parse(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every agent with its Run, unread mail and failing sessions (D77), and the
/// stopped chains (D27).
async fn status(State(s): Shared, Authenticated(caller): Authenticated) -> Reply<Status> {
    let agents = s.registry.list_agents(&caller).await?;
    let inboxes = s.delivery.inbox_status(&caller).await?;
    let runs = s.runs.active_runs(&caller).await?;
    Ok(Json(Status {
        agents: AgentStatus::of(agents, inboxes, runs),
        stopped_chains: s.delivery.stopped_chains(&caller).await?,
    }))
}

/// Either filter, both, or none.
#[derive(Deserialize)]
struct RunsQuery {
    agent: Option<String>,
    chain: Option<String>,
}

async fn list_runs(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Query(query): Query<RunsQuery>,
) -> Reply<Vec<RunRecord>> {
    let filter = RunFilter {
        agent: query.agent.as_deref().map(parse).transpose()?,
        chain: query.chain.as_deref().map(parse).transpose()?,
    };
    Ok(Json(s.runs.runs(&caller, filter).await?))
}

async fn get_run(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Reply<RunRecord> {
    Ok(Json(s.runs.run(&caller, parse(&id)?).await?))
}

/// Exactly one of `chain`, `run` and `task`.
#[derive(Deserialize)]
struct JournalQuery {
    chain: Option<String>,
    run: Option<String>,
    task: Option<String>,
}

async fn read_journal(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Query(query): Query<JournalQuery>,
) -> Reply<Vec<JournalEntry>> {
    let filter = match (query.chain, query.run, query.task) {
        (Some(id), None, None) => JournalFilter::Chain(parse::<CorrelationId>(&id)?),
        (None, Some(id), None) => JournalFilter::Run(parse::<RunId>(&id)?),
        (None, None, Some(id)) => JournalFilter::Task(parse::<TaskId>(&id)?),
        _ => {
            return Err(
                Error::InvalidInput("give exactly one of chain, run and task".to_owned()).into(),
            );
        }
    };
    Ok(Json(s.journal.read(&caller, filter).await?))
}

/// A value from the path or query.
fn parse<T: std::str::FromStr>(value: &str) -> Result<T, Error> {
    value
        .parse()
        .map_err(|_| Error::InvalidInput(format!("invalid value {value:?}")))
}

#[cfg(test)]
mod tests;
