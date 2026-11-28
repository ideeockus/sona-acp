//! The external HTTP API: JSON over HTTP for our Pi extension and the operator CLI (D52, D85).
//!
//! Every handler authenticates the caller first and passes it to the component
//! explicitly (D11). The components decide what the caller may do.

mod extract;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, SET_COOKIE,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use control_plane_common::broker::{
    Conversation, DeliveryControl, Message, MessageBroker, NewMessage, Thread,
};
use control_plane_common::files::{FileInfo, FileStorage, MAX_FILE_BYTES};
use control_plane_common::gateway::{ToolCall, ToolGateway, Verdict};
use control_plane_common::journal::{JournalEntry, JournalFilter, JournalReader};
use control_plane_common::registry::{
    Agent, AgentCard, AgentRegistry, AgentVersion, Credentials, ModelRef,
};
use control_plane_common::runtime::{ModelInfo, RunControl, RunFilter, RunRecord, TranscriptEntry};
use control_plane_common::status::{AgentStatus, Status};
use control_plane_common::tasks::{NewTask, Task, TaskManager, TaskStatus, TaskUpdate, TaskView};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, FileId, MessageId, RunId, SessionKey, TaskId, Timestamp,
};
use extract::{ApiError, Authenticated, Body, SESSION_COOKIE, session_cookie};
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
    pub files: Arc<dyn FileStorage>,
}

type Shared = State<Arc<Services>>;
type Reply<T> = Result<Json<T>, ApiError>;

pub fn router(services: Services) -> Router {
    Router::new()
        .route(
            "/v1/session",
            post(open_session)
                .get(current_session)
                .delete(close_session),
        )
        .route("/v1/messages", post(send_message))
        .route("/v1/sessions/{session}/messages", get(read_messages))
        .route("/v1/conversations", get(conversations))
        .route("/v1/conversations/{agent}/threads", get(threads))
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
        .route("/v1/runs/{id}/transcript", get(run_transcript))
        .route("/v1/journal", get(read_journal))
        .route("/v1/models", get(list_models))
        .route(
            "/v1/files",
            post(upload_file)
                .get(list_files)
                // A little over a file, for the request itself.
                .layer(DefaultBodyLimit::max(MAX_FILE_BYTES + 64 * 1024)),
        )
        .route("/v1/files/{id}", get(download_file))
        .route("/v1/files/{id}/info", get(file_info))
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

#[derive(Deserialize)]
struct Login {
    credential: String,
}

/// Who is logged in, and until when.
#[derive(Serialize)]
struct SessionInfo {
    alias: Alias,
    expires_at: Option<Timestamp>,
}

/// Exchanges the operator's credential for a session cookie (D103). The credential
/// itself never reaches the browser's storage.
async fn open_session(
    State(s): Shared,
    Body(login): Body<Login>,
) -> Result<impl IntoResponse, ApiError> {
    let session = s.credentials.open_session(&login.credential).await?;
    let alias = s
        .credentials
        .session_caller(session.secret.expose())
        .await?
        .alias()
        .clone();
    let max_age = (session.expires_at.as_unix_millis() - Timestamp::now().as_unix_millis()) / 1000;
    let cookie = session_cookie_header(session.secret.expose(), max_age);
    let info = SessionInfo {
        alias,
        expires_at: Some(session.expires_at),
    };
    Ok((StatusCode::CREATED, [(SET_COOKIE, cookie)], Json(info)))
}

async fn current_session(Authenticated(caller): Authenticated) -> Reply<SessionInfo> {
    Ok(Json(SessionInfo {
        alias: caller.alias().clone(),
        expires_at: None,
    }))
}

/// Ends the session, even an expired one, and clears the cookie.
async fn close_session(
    State(s): Shared,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(session) = session_cookie(&headers) {
        s.credentials.close_session(session).await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, session_cookie_header("", 0))],
    ))
}

/// Only our API reads it, never a script, and only over HTTPS from our own pages.
fn session_cookie_header(value: &str, max_age: i64) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/v1; HttpOnly; Secure; SameSite=Strict; Max-Age={max_age}"
    )
}

async fn conversations(
    State(s): Shared,
    Authenticated(caller): Authenticated,
) -> Reply<Vec<Conversation>> {
    Ok(Json(s.broker.conversations(&caller).await?))
}

async fn threads(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(agent): Path<String>,
) -> Reply<Vec<Thread>> {
    let agent: Alias = parse(&agent)?;
    Ok(Json(s.broker.threads(&caller, &agent).await?))
}

#[derive(Deserialize)]
struct PageQuery {
    session: Option<String>,
    before: Option<String>,
}

async fn conversation(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(agent): Path<String>,
    Query(query): Query<PageQuery>,
) -> Reply<Vec<Message>> {
    let agent: Alias = parse(&agent)?;
    let session = query.session.as_deref().map(parse).transpose()?;
    let before = query.before.as_deref().map(parse).transpose()?;
    Ok(Json(
        s.broker
            .conversation(&caller, &agent, session, before)
            .await?,
    ))
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
    check_model(&s, &caller, &card.model).await?;
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
    check_model(&s, &caller, &edit.card.model).await?;
    Ok(Json(s.registry.update_agent(&caller, edit).await?))
}

/// A card runs only on a model Pi has a key for (D104): a typo or a missing key
/// fails here, not at the agent's first Run.
async fn check_model(s: &Services, caller: &Caller, model: &ModelRef) -> Result<(), Error> {
    let models = s.runs.models(caller).await?;
    if models
        .iter()
        .any(|m| m.provider == model.provider && m.id == model.id)
    {
        return Ok(());
    }
    Err(Error::InvalidInput(format!(
        "Pi has no model {}/{}: check the name, and that provider_env passes its key",
        model.provider, model.id
    )))
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
}

/// The body is the file as is; its type comes in `Content-Type` (D106).
async fn upload_file(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<FileInfo>), ApiError> {
    let body = body.map_err(|e| Error::InvalidInput(e.body_text()))?;
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    let file = s
        .files
        .upload(&caller, &query.name, content_type, body.to_vec())
        .await?;
    Ok((StatusCode::CREATED, Json(file)))
}

async fn list_files(
    State(s): Shared,
    Authenticated(caller): Authenticated,
) -> Reply<Vec<FileInfo>> {
    Ok(Json(s.files.list(&caller).await?))
}

async fn file_info(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Reply<FileInfo> {
    let id: FileId = parse(&id)?;
    Ok(Json(s.files.info(&caller, id).await?))
}

/// An agent's file must not run as a page of ours, next to the session cookie: only
/// plain images show inline, everything else downloads, and nothing may script.
async fn download_file(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id: FileId = parse(&id)?;
    let (file, content) = s.files.download(&caller, id).await?;
    let image = matches!(
        file.content_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    );
    let (content_type, disposition) = if image {
        (file.content_type.clone(), "inline")
    } else {
        ("application/octet-stream".to_owned(), "attachment")
    };
    let headers = [
        (CONTENT_TYPE, content_type),
        (
            CONTENT_DISPOSITION,
            format!("{disposition}; filename*=UTF-8''{}", rfc5987(&file.name)),
        ),
        (X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        (
            CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'".to_owned(),
        ),
        (CACHE_CONTROL, "private, no-store".to_owned()),
    ];
    Ok((headers, content))
}

/// A file name as a `filename*` value: every byte but the safe ones percent-encoded.
fn rfc5987(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn list_models(
    State(s): Shared,
    Authenticated(caller): Authenticated,
) -> Reply<Vec<ModelInfo>> {
    Ok(Json(s.runs.models(&caller).await?))
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

async fn run_transcript(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Path(id): Path<String>,
) -> Reply<Vec<TranscriptEntry>> {
    Ok(Json(s.runs.transcript(&caller, parse(&id)?).await?))
}

/// At most one of `chain`, `run`, `task` and `agent`; none reads the latest.
#[derive(Deserialize)]
struct JournalQuery {
    chain: Option<String>,
    run: Option<String>,
    task: Option<String>,
    agent: Option<String>,
}

async fn read_journal(
    State(s): Shared,
    Authenticated(caller): Authenticated,
    Query(query): Query<JournalQuery>,
) -> Reply<Vec<JournalEntry>> {
    let filter = match (query.chain, query.run, query.task, query.agent) {
        (Some(id), None, None, None) => JournalFilter::Chain(parse::<CorrelationId>(&id)?),
        (None, Some(id), None, None) => JournalFilter::Run(parse::<RunId>(&id)?),
        (None, None, Some(id), None) => JournalFilter::Task(parse::<TaskId>(&id)?),
        (None, None, None, Some(alias)) => JournalFilter::Agent(parse::<Alias>(&alias)?),
        (None, None, None, None) => JournalFilter::Latest,
        _ => {
            return Err(Error::InvalidInput(
                "give at most one of chain, run, task and agent".to_owned(),
            )
            .into());
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
