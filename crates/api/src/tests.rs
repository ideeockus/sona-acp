use super::*;
use async_trait::async_trait;
use axum::body::Body as HttpBody;
use axum::http::{Method, Request};
use control_plane_common::broker::{InboxStatus, StoppedChain};
use control_plane_common::registry::{Secret, Session};
use control_plane_common::runtime::{
    ActiveRun, ModelInfo, Price, RunFilter, RunRecord, TranscriptEntry,
};
use control_plane_common::{Caller, MessageId, Result, Timestamp};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Mutex;
use tower::ServiceExt;

const OPERATOR: &str = "sona_plane_op_test";
const AGENT: &str = "sona_plane_rt_test";
const SESSION: &str = "sona_plane_ss_test";

fn alias(name: &str) -> Alias {
    name.parse().unwrap()
}

fn agent_caller() -> Caller {
    Caller::Agent {
        alias: alias("coder"),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    }
}

/// Two known credentials: the operator's and one agent's.
struct Tokens;

#[async_trait]
impl Credentials for Tokens {
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
    async fn authenticate(&self, credential: &str) -> Result<Caller> {
        match credential {
            OPERATOR => Ok(Caller::Operator {
                alias: alias("user"),
            }),
            AGENT => Ok(agent_caller()),
            _ => Err(Error::Unauthenticated),
        }
    }
    async fn contains_active_run_token(&self, _: &str) -> Result<bool> {
        unimplemented!()
    }
    async fn open_session(&self, credential: &str) -> Result<Session> {
        match self.authenticate(credential).await? {
            Caller::Operator { .. } => Ok(Session {
                secret: Secret::new(SESSION.to_owned()),
                expires_at: Timestamp::from_unix_millis(
                    Timestamp::now().as_unix_millis() + 3_600_000,
                )?,
            }),
            _ => Err(Error::Forbidden("not the operator".to_owned())),
        }
    }
    async fn session_caller(&self, session: &str) -> Result<Caller> {
        match session {
            SESSION => Ok(Caller::Operator {
                alias: alias("user"),
            }),
            _ => Err(Error::Unauthenticated),
        }
    }
    async fn close_session(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// Only the operator manages agents, like the real Registry. Logs state changes.
struct Registry(Arc<Log>);

#[async_trait]
impl AgentRegistry for Registry {
    async fn create_agent(&self, caller: &Caller, _: AgentCard) -> Result<()> {
        operator_only(caller)
    }
    async fn get_agent(&self, caller: &Caller, _: &Alias) -> Result<Agent> {
        operator_only(caller)?;
        Err(Error::NotFound("agent @ghost".to_owned()))
    }
    async fn list_agents(&self, caller: &Caller) -> Result<Vec<Agent>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
    async fn update_agent(&self, caller: &Caller, edit: Agent) -> Result<Agent> {
        operator_only(caller)?;
        Err(Error::Conflict(format!(
            "{} is at version 3",
            edit.card.alias
        )))
    }
    async fn agent_versions(&self, caller: &Caller, _: &Alias) -> Result<Vec<AgentVersion>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
    async fn disable_agent(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        operator_only(caller)?;
        self.0.push(format!("disable {alias}"));
        Ok(())
    }
    async fn enable_agent(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        operator_only(caller)?;
        self.0.push(format!("enable {alias}"));
        Ok(())
    }
    async fn delete_agent(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        operator_only(caller)?;
        self.0.push(format!("delete {alias}"));
        Ok(())
    }
}

fn operator_only(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        _ => Err(Error::Forbidden("not the operator".to_owned())),
    }
}

/// Stores the sender it was given, so tests can see who the API said was calling.
/// Every message is in one chain.
struct Broker {
    callers: Mutex<Vec<Caller>>,
    chain: CorrelationId,
}

#[async_trait]
impl MessageBroker for Broker {
    async fn send(&self, caller: &Caller, message: NewMessage) -> Result<Message> {
        self.callers.lock().unwrap().push(caller.clone());
        Ok(Message {
            id: MessageId::new(),
            from: caller.alias().clone(),
            to: message.to,
            body: message.body,
            subject: message.subject,
            task_id: message.task_id,
            in_reply_to: message.in_reply_to,
            correlation_id: self.chain,
            created_at: Timestamp::now(),
        })
    }
    async fn read_messages(&self, _: &Caller, _: SessionKey) -> Result<Vec<Message>> {
        Ok(Vec::new())
    }
    async fn conversations(&self, caller: &Caller) -> Result<Vec<Conversation>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
    async fn conversation(
        &self,
        caller: &Caller,
        agent: &Alias,
        before: Option<MessageId>,
    ) -> Result<Vec<Message>> {
        operator_only(caller)?;
        self.callers.lock().unwrap().push(caller.clone());
        assert_eq!((agent.as_str(), before.is_some()), ("coder", true));
        Ok(Vec::new())
    }
    async fn mark_read(&self, caller: &Caller, agent: &Alias, _: MessageId) -> Result<()> {
        operator_only(caller)?;
        assert_eq!(agent.as_str(), "coder");
        Ok(())
    }
}

/// Fails every call with an internal error that must not leak.
struct BrokenTasks;

#[async_trait]
impl TaskManager for BrokenTasks {
    async fn create_task(&self, _: &Caller, _: NewTask) -> Result<Task> {
        Err(Error::Internal(
            "database /srv/data/tasks.db is locked".to_owned(),
        ))
    }
    async fn get_task(&self, _: &Caller, _: TaskId) -> Result<TaskView> {
        Err(Error::Internal(
            "database /srv/data/tasks.db is locked".to_owned(),
        ))
    }
    async fn update_task(&self, _: &Caller, _: TaskId, _: TaskUpdate) -> Result<Task> {
        unimplemented!()
    }
    async fn list_tasks(&self, caller: &Caller, status: Option<TaskStatus>) -> Result<Vec<Task>> {
        operator_only(caller)?;
        assert_eq!(status, Some(TaskStatus::NeedsHuman));
        Ok(Vec::new())
    }
}

struct DenyBash;

#[async_trait]
impl ToolGateway for DenyBash {
    async fn check(&self, _: &Caller, call: ToolCall) -> Result<Verdict> {
        Ok(if call.tool == "bash" {
            Verdict::Deny {
                reason: "bash is not granted".to_owned(),
            }
        } else {
            Verdict::Allow
        })
    }
}

#[derive(Default)]
struct Journal(Mutex<Vec<JournalFilter>>);

#[async_trait]
impl JournalReader for Journal {
    async fn read(&self, _: &Caller, filter: JournalFilter) -> Result<Vec<JournalEntry>> {
        self.0.lock().unwrap().push(filter);
        Ok(Vec::new())
    }
}

/// What Broker and Runtime were asked to do, in order.
#[derive(Default)]
struct Log(Mutex<Vec<String>>);

impl Log {
    fn push(&self, entry: String) {
        self.0.lock().unwrap().push(entry);
    }
}

/// Logs what it is asked and keeps the stopped chains; the operator only.
struct Delivery {
    log: Arc<Log>,
    stopped: Mutex<Vec<CorrelationId>>,
}

#[async_trait]
impl DeliveryControl for Delivery {
    async fn inbox_status(&self, caller: &Caller) -> Result<Vec<InboxStatus>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
    async fn retry(&self, caller: &Caller, agent: &Alias, session: SessionKey) -> Result<()> {
        operator_only(caller)?;
        self.log.push(format!("retry {} {session}", agent.as_str()));
        Ok(())
    }
    async fn skip(&self, caller: &Caller, agent: &Alias, session: SessionKey) -> Result<()> {
        operator_only(caller)?;
        self.log.push(format!("skip {} {session}", agent.as_str()));
        Ok(())
    }
    async fn stop_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()> {
        operator_only(caller)?;
        self.log.push(format!("stop {chain}"));
        self.stopped.lock().unwrap().push(chain);
        Ok(())
    }
    async fn resume_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()> {
        operator_only(caller)?;
        self.log.push(format!("resume {chain}"));
        self.stopped.lock().unwrap().retain(|c| *c != chain);
        Ok(())
    }
    async fn stopped_chains(&self, caller: &Caller) -> Result<Vec<StoppedChain>> {
        operator_only(caller)?;
        let stopped = self.stopped.lock().unwrap();
        Ok(stopped
            .iter()
            .map(|&chain| StoppedChain {
                chain,
                stopped_at: Timestamp::now(),
                reason: None,
            })
            .collect())
    }
    async fn deliver_pending(&self, caller: &Caller, agent: &Alias) -> Result<()> {
        operator_only(caller)?;
        self.log.push(format!("deliver {agent}"));
        Ok(())
    }
}

/// No Run is active; aborts are logged.
struct Runs(Arc<Log>);

#[async_trait]
impl RunControl for Runs {
    async fn active_runs(&self, caller: &Caller) -> Result<Vec<ActiveRun>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
    async fn abort_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()> {
        operator_only(caller)?;
        self.0.push(format!("abort {chain}"));
        Ok(())
    }
    async fn transcript(&self, caller: &Caller, _: RunId) -> Result<Vec<TranscriptEntry>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
    async fn abort_agent(&self, caller: &Caller, agent: &Alias, reason: &str) -> Result<()> {
        operator_only(caller)?;
        self.0.push(format!("abort {agent}: {reason}"));
        Ok(())
    }
    async fn runs(&self, caller: &Caller, filter: RunFilter) -> Result<Vec<RunRecord>> {
        operator_only(caller)?;
        let agent = filter.agent.map(|a| a.as_str().to_owned());
        self.0.push(format!("runs {agent:?} {:?}", filter.chain));
        Ok(Vec::new())
    }
    async fn run(&self, caller: &Caller, run_id: RunId) -> Result<RunRecord> {
        operator_only(caller)?;
        Err(Error::NotFound(format!("run {run_id}")))
    }
    /// Pi has one model, `p/m`.
    async fn models(&self, caller: &Caller) -> Result<Vec<ModelInfo>> {
        operator_only(caller)?;
        Ok(vec![ModelInfo {
            provider: "p".to_owned(),
            id: "m".to_owned(),
            name: "M".to_owned(),
            context_window: 1000,
            price: Price::default(),
        }])
    }
}

struct Setup {
    app: Router,
    broker: Arc<Broker>,
    log: Arc<Log>,
    journal: Arc<Journal>,
}

fn setup() -> Setup {
    let broker = Arc::new(Broker {
        callers: Mutex::default(),
        chain: CorrelationId::new(),
    });
    let log = Arc::new(Log::default());
    let delivery = Arc::new(Delivery {
        log: log.clone(),
        stopped: Mutex::default(),
    });
    let journal = Arc::new(Journal::default());
    let app = router(Services {
        credentials: Arc::new(Tokens),
        registry: Arc::new(Registry(log.clone())),
        broker: broker.clone(),
        delivery,
        runs: Arc::new(Runs(log.clone())),
        tasks: Arc::new(BrokenTasks),
        gateway: Arc::new(DenyBash),
        journal: journal.clone(),
    });
    Setup {
        app,
        broker,
        log,
        journal,
    }
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(json) => {
            request = request.header("content-type", "application/json");
            HttpBody::from(json.to_string())
        }
        None => HttpBody::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn message_body() -> Value {
    json!({"to": "manager", "body": "done", "subject": null, "task_id": null, "in_reply_to": null})
}

#[tokio::test]
async fn requests_without_a_valid_credential_are_rejected() {
    let s = setup();
    for token in [None, Some("sona_plane_rt_forged")] {
        let (status, body) = call(&s.app, Method::GET, "/v1/agents", token, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, json!({"kind": "unauthenticated"}));
    }
}

#[tokio::test]
async fn the_sender_comes_from_the_credential_not_the_body() {
    let s = setup();
    let mut body = message_body();
    body["from"] = json!("user");
    let (status, sent) = call(
        &s.app,
        Method::POST,
        "/v1/messages",
        Some(AGENT),
        Some(body),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(sent["from"], "coder");
    assert_eq!(s.broker.callers.lock().unwrap()[0].alias(), &alias("coder"));
}

#[tokio::test]
async fn component_errors_map_to_status_codes() {
    let s = setup();
    let (status, body) = call(&s.app, Method::GET, "/v1/agents", Some(AGENT), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["kind"], "forbidden");

    let (status, _) = call(
        &s.app,
        Method::GET,
        "/v1/agents/ghost",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let card = json!({"alias": "coder", "title": "", "model": {"provider": "p", "id": "m"},
                      "access": [], "allow_message_to": [], "prompt": ""});
    let (status, body) = call(
        &s.app,
        Method::POST,
        "/v1/agents",
        Some(OPERATOR),
        Some(card),
    )
    .await;
    assert_eq!((status, body), (StatusCode::CREATED, Value::Null));
}

#[tokio::test]
async fn an_edit_keeps_the_alias_and_a_stale_one_is_a_409() {
    let s = setup();
    let edit = |alias: &str| {
        json!({"alias": alias, "title": "", "model": {"provider": "p", "id": "m"},
               "access": [], "allow_message_to": [], "prompt": "", "version": 2})
    };
    let put = |path: &'static str, body: Value| {
        call(&s.app, Method::PUT, path, Some(OPERATOR), Some(body))
    };

    let (status, body) = put("/v1/agents/coder", edit("tester")).await;
    assert_eq!(
        (status, body["kind"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_input"))
    );

    // The stand-in Registry is always at a newer version.
    let (status, body) = put("/v1/agents/coder", edit("coder")).await;
    assert_eq!(
        (status, body["kind"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );

    let (status, _) = call(
        &s.app,
        Method::GET,
        "/v1/agents/coder/versions",
        Some(AGENT),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn internal_details_stay_out_of_responses() {
    let s = setup();
    let task = TaskId::new();
    let (status, body) = call(
        &s.app,
        Method::GET,
        &format!("/v1/tasks/{task}"),
        Some(AGENT),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body,
        json!({"kind": "internal", "detail": "internal error"})
    );
}

#[tokio::test]
async fn bad_input_is_a_json_400() {
    let s = setup();
    let broken = json!({"to": "Not An Alias", "body": "x"});
    let (status, body) = call(
        &s.app,
        Method::POST,
        "/v1/messages",
        Some(AGENT),
        Some(broken),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["kind"], "invalid_input");

    for uri in [
        "/v1/sessions/job-1/messages",
        "/v1/tasks/not-a-uuid",
        "/v1/agents/Bad%20Alias",
    ] {
        let (status, body) = call(&s.app, Method::GET, uri, Some(OPERATOR), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["kind"], "invalid_input", "{uri}");
    }
}

#[tokio::test]
async fn gateway_verdicts_pass_through() {
    let s = setup();
    let (status, verdict) = call(
        &s.app,
        Method::POST,
        "/v1/gateway/check",
        Some(AGENT),
        Some(json!({"tool": "bash", "path": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        verdict,
        json!({"verdict": "deny", "reason": "bash is not granted"})
    );
}

#[tokio::test]
async fn journal_takes_at_most_one_filter() {
    let s = setup();
    let chain = CorrelationId::new();
    let (status, _) = call(
        &s.app,
        Method::GET,
        &format!("/v1/journal?chain={chain}"),
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s.journal.0.lock().unwrap()[0], JournalFilter::Chain(chain));

    for (uri, filter) in [
        (
            "/v1/journal?agent=coder",
            JournalFilter::Agent(alias("coder")),
        ),
        ("/v1/journal", JournalFilter::Latest),
    ] {
        let (status, _) = call(&s.app, Method::GET, uri, Some(OPERATOR), None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(s.journal.0.lock().unwrap().last(), Some(&filter));
    }

    let run = RunId::new();
    let uri = format!("/v1/journal?chain={chain}&run={run}");
    let (status, _) = call(&s.app, Method::GET, &uri, Some(OPERATOR), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
}

#[tokio::test]
async fn sessions_are_read_by_key() {
    let s = setup();
    let session = SessionKey::Chain(CorrelationId::new());
    let (status, messages) = call(
        &s.app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Some(AGENT),
        None,
    )
    .await;
    assert_eq!((status, messages), (StatusCode::OK, json!([])));
}

#[tokio::test]
async fn status_is_for_the_operator() {
    let s = setup();
    let (status, body) = call(&s.app, Method::GET, "/v1/status", Some(OPERATOR), None).await;
    let empty = json!({ "agents": [], "stopped_chains": [] });
    assert_eq!((status, body), (StatusCode::OK, empty));
    let (status, _) = call(&s.app, Method::GET, "/v1/status", Some(AGENT), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn retry_and_skip_name_an_agent_and_a_session() {
    let s = setup();
    let session = SessionKey::Task(TaskId::new());
    for action in ["retry", "skip"] {
        let uri = format!("/v1/agents/coder/sessions/{session}/{action}");
        let (status, _) = call(&s.app, Method::POST, &uri, Some(OPERATOR), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        *s.log.0.lock().unwrap(),
        [
            format!("retry coder {session}"),
            format!("skip coder {session}")
        ]
    );

    let uri = "/v1/agents/coder/sessions/inbox/retry";
    let (status, _) = call(&s.app, Method::POST, uri, Some(OPERATOR), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stop_reaches_broker_before_runtime() {
    let s = setup();
    let chain = CorrelationId::new();
    for action in ["stop", "resume"] {
        let uri = format!("/v1/chains/{chain}/{action}");
        let (status, _) = call(&s.app, Method::POST, &uri, Some(OPERATOR), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        *s.log.0.lock().unwrap(),
        [
            format!("stop {chain}"),
            format!("abort {chain}"),
            format!("resume {chain}")
        ]
    );
    let (status, _) = call(
        &s.app,
        Method::POST,
        "/v1/chains/42/stop",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn disabling_and_deleting_reach_registry_before_runtime() {
    let s = setup();
    for (method, uri) in [
        (Method::POST, "/v1/agents/coder/disable"),
        (Method::POST, "/v1/agents/coder/enable"),
        (Method::DELETE, "/v1/agents/coder"),
    ] {
        let (status, _) = call(&s.app, method, uri, Some(OPERATOR), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{uri}");
    }
    assert_eq!(
        *s.log.0.lock().unwrap(),
        [
            "disable @coder",
            "abort @coder: the agent was disabled",
            "enable @coder",
            "deliver @coder",
            "delete @coder",
            "abort @coder: the agent was deleted",
        ]
    );
    let (status, _) = call(
        &s.app,
        Method::POST,
        "/v1/agents/coder/disable",
        Some(AGENT),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_operator_learns_that_the_chain_is_stopped() {
    let s = setup();
    let message = json!({ "to": "coder", "body": "hi", "subject": null,
                          "task_id": null, "in_reply_to": null });
    let send = |token| {
        call(
            &s.app,
            Method::POST,
            "/v1/messages",
            Some(token),
            Some(message.clone()),
        )
    };

    let (_, sent) = send(OPERATOR).await;
    assert_eq!(sent["chain_stopped"], false);
    let uri = format!("/v1/chains/{}/stop", s.broker.chain);
    call(&s.app, Method::POST, &uri, Some(OPERATOR), None).await;
    let (status, sent) = send(OPERATOR).await;
    assert_eq!(
        (status, &sent["chain_stopped"]),
        (StatusCode::CREATED, &json!(true))
    );
    assert_eq!(sent["body"], "hi");
}

#[tokio::test]
async fn runs_are_listed_by_agent_and_chain() {
    let s = setup();
    let (status, body) = call(
        &s.app,
        Method::GET,
        "/v1/runs?agent=coder",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!([])));
    assert_eq!(*s.log.0.lock().unwrap(), [r#"runs Some("coder") None"#]);

    let (status, _) = call(
        &s.app,
        Method::GET,
        "/v1/runs?chain=42",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let uri = format!("/v1/runs/{}", RunId::new());
    let (status, _) = call(&s.app, Method::GET, &uri, Some(OPERATOR), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&s.app, Method::GET, "/v1/runs", Some(AGENT), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tasks_are_listed_by_status() {
    let s = setup();
    let (status, body) = call(
        &s.app,
        Method::GET,
        "/v1/tasks?status=needs_human",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!([])));
    let (status, body) = call(
        &s.app,
        Method::GET,
        "/v1/tasks?status=waiting",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(
        (status, body["kind"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_input"))
    );
}

#[tokio::test]
async fn conversations_page_and_mark_read() {
    let s = setup();
    let (status, body) = call(
        &s.app,
        Method::GET,
        "/v1/conversations",
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!([])));
    let before = MessageId::new();
    let (status, _) = call(
        &s.app,
        Method::GET,
        &format!("/v1/conversations/coder/messages?before={before}"),
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &s.app,
        Method::POST,
        "/v1/conversations/coder/read",
        Some(OPERATOR),
        Some(json!({ "up_to": before })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&s.app, Method::GET, "/v1/conversations", Some(AGENT), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_transcript_is_for_the_operator() {
    let s = setup();
    let uri = format!("/v1/runs/{}/transcript", RunId::new());
    let (status, body) = call(&s.app, Method::GET, &uri, Some(OPERATOR), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!([])));
    let (status, _) = call(&s.app, Method::GET, &uri, Some(AGENT), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A request as the browser sends it: the session cookie and, maybe, an Origin.
async fn browser(
    app: &Router,
    method: Method,
    uri: &str,
    origin: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "dash.example.com")
        .header("cookie", format!("theme=dark; sona_session={SESSION}"));
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    app.clone()
        .oneshot(request.body(HttpBody::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn the_credential_opens_a_session_cookie() {
    let s = setup();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/session")
        .header("content-type", "application/json")
        .body(HttpBody::from(
            json!({ "credential": OPERATOR }).to_string(),
        ))
        .unwrap();
    let response = s.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        cookie.starts_with(&format!("sona_session={SESSION};")),
        "{cookie}"
    );
    for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/v1"] {
        assert!(cookie.contains(attribute), "{cookie}");
    }

    let (status, _) = call(
        &s.app,
        Method::POST,
        "/v1/session",
        None,
        Some(json!({ "credential": AGENT })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_session_reads_freely_but_changes_only_from_our_origin() {
    let s = setup();
    let response = browser(&s.app, Method::GET, "/v1/session", None).await;
    assert_eq!(response.status(), StatusCode::OK);

    let uri = format!("/v1/chains/{}/stop", CorrelationId::new());
    for origin in [
        None,
        Some("https://evil.example"),
        Some("https://dash.example.com.evil"),
    ] {
        let response = browser(&s.app, Method::POST, &uri, origin).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin:?}");
    }
    let response = browser(&s.app, Method::POST, &uri, Some("https://dash.example.com")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = browser(&s.app, Method::DELETE, "/v1/session", None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cleared = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cleared.starts_with("sona_session=;") && cleared.contains("Max-Age=0"));
}

#[tokio::test]
async fn a_card_needs_a_model_pi_has() {
    let s = setup();
    let (status, body) = call(&s.app, Method::GET, "/v1/models", Some(OPERATOR), None).await;
    assert_eq!(
        (status, body[0]["provider"].as_str()),
        (StatusCode::OK, Some("p"))
    );
    let (status, _) = call(&s.app, Method::GET, "/v1/models", Some(AGENT), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let card = json!({"alias": "coder", "title": "", "model": {"provider": "p", "id": "typo"},
                      "access": [], "allow_message_to": [], "prompt": ""});
    let (status, body) = call(
        &s.app,
        Method::POST,
        "/v1/agents",
        Some(OPERATOR),
        Some(card.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["detail"].as_str().unwrap().contains("p/typo"),
        "{body}"
    );

    let mut edit = card;
    edit["version"] = json!(2);
    let (status, _) = call(
        &s.app,
        Method::PUT,
        "/v1/agents/coder",
        Some(OPERATOR),
        Some(edit),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
