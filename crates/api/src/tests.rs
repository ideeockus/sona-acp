use super::*;
use async_trait::async_trait;
use axum::body::Body as HttpBody;
use axum::http::{Method, Request};
use control_plane_common::registry::Secret;
use control_plane_common::{Caller, MessageId, Result, Timestamp};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Mutex;
use tower::ServiceExt;

const OPERATOR: &str = "sona_plane_op_test";
const AGENT: &str = "sona_plane_rt_test";

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
}

/// Only the operator manages agents, like the real Registry.
struct Registry;

#[async_trait]
impl AgentRegistry for Registry {
    async fn create_agent(&self, caller: &Caller, _: AgentCard) -> Result<()> {
        operator_only(caller)
    }
    async fn get_agent(&self, caller: &Caller, _: &Alias) -> Result<AgentCard> {
        operator_only(caller)?;
        Err(Error::NotFound("agent @ghost".to_owned()))
    }
    async fn list_agents(&self, caller: &Caller) -> Result<Vec<AgentCard>> {
        operator_only(caller)?;
        Ok(Vec::new())
    }
}

fn operator_only(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        _ => Err(Error::Forbidden("not the operator".to_owned())),
    }
}

/// Stores the sender it was given, so tests can see who the API said was calling.
#[derive(Default)]
struct Broker(Mutex<Vec<Caller>>);

#[async_trait]
impl MessageBroker for Broker {
    async fn send(&self, caller: &Caller, message: NewMessage) -> Result<Message> {
        self.0.lock().unwrap().push(caller.clone());
        Ok(Message {
            id: MessageId::new(),
            from: caller.alias().clone(),
            to: message.to,
            body: message.body,
            subject: message.subject,
            task_id: message.task_id,
            in_reply_to: message.in_reply_to,
            correlation_id: CorrelationId::new(),
            created_at: Timestamp::now(),
        })
    }
    async fn read_messages(&self, _: &Caller, _: SessionKey) -> Result<Vec<Message>> {
        Ok(Vec::new())
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

struct Setup {
    app: Router,
    broker: Arc<Broker>,
    journal: Arc<Journal>,
}

fn setup() -> Setup {
    let broker = Arc::new(Broker::default());
    let journal = Arc::new(Journal::default());
    let app = router(Services {
        credentials: Arc::new(Tokens),
        registry: Arc::new(Registry),
        broker: broker.clone(),
        tasks: Arc::new(BrokenTasks),
        gateway: Arc::new(DenyBash),
        journal: journal.clone(),
    });
    Setup {
        app,
        broker,
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
    assert_eq!(s.broker.0.lock().unwrap()[0].alias(), &alias("coder"));
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
                      "allow_tools": [], "allow_message_to": [], "prompt": ""});
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
async fn journal_takes_exactly_one_filter() {
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

    let run = RunId::new();
    for uri in [
        "/v1/journal".to_owned(),
        format!("/v1/journal?chain={chain}&run={run}"),
    ] {
        let (status, _) = call(&s.app, Method::GET, &uri, Some(OPERATOR), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
    }
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
