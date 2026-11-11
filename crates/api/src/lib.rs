//! The external HTTP API: JSON over HTTP for our Pi extension and the operator CLI (D52, D85).
//!
//! Every handler authenticates the caller first and passes it to the component
//! explicitly (D11). The components decide what the caller may do.

mod extract;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use control_plane_common::broker::{Message, MessageBroker, NewMessage};
use control_plane_common::gateway::{ToolCall, ToolGateway, Verdict};
use control_plane_common::journal::{JournalEntry, JournalFilter, JournalReader};
use control_plane_common::registry::{AgentCard, AgentRegistry, Credentials};
use control_plane_common::tasks::{NewTask, Task, TaskManager, TaskUpdate, TaskView};
use control_plane_common::{Alias, CorrelationId, Error, RunId, SessionKey, TaskId};
use extract::{ApiError, Authenticated, Body};
use serde::Deserialize;
use std::sync::Arc;

/// The components the API exposes, each through its external trait.
pub struct Services {
    pub credentials: Arc<dyn Credentials>,
    pub registry: Arc<dyn AgentRegistry>,
    pub broker: Arc<dyn MessageBroker>,
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
        .route("/v1/tasks", post(create_task))
        .route("/v1/tasks/{id}", get(get_task).patch(update_task))
        .route("/v1/gateway/check", post(check_tool))
        .route("/v1/agents", post(create_agent).get(list_agents))
        .route("/v1/agents/{alias}", get(get_agent))
        .route("/v1/journal", get(read_journal))
        .with_state(Arc::new(services))
}

async fn send_message(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Body(message): Body<NewMessage>,
) -> Result<(StatusCode, Json<Message>), ApiError> {
    let sent = s.broker.send(&caller, message).await?;
    Ok((StatusCode::CREATED, Json(sent)))
}

async fn read_messages(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(session): Path<String>,
) -> Reply<Vec<Message>> {
    let session: SessionKey = session.parse().map_err(Error::InvalidInput)?;
    Ok(Json(s.broker.read_messages(&caller, session).await?))
}

async fn create_task(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Body(task): Body<NewTask>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let created = s.tasks.create_task(&caller, task).await?;
    Ok((StatusCode::CREATED, Json(created)))
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

async fn list_agents(
    State(s): Shared,
    Authenticated(caller): Authenticated,
) -> Reply<Vec<AgentCard>> {
    Ok(Json(s.registry.list_agents(&caller).await?))
}

async fn get_agent(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(alias): Path<String>,
) -> Reply<AgentCard> {
    let alias: Alias = parse(&alias)?;
    Ok(Json(s.registry.get_agent(&caller, &alias).await?))
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
