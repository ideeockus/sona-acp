//! The whole path with a real Pi (D25): the operator writes, Runtime starts Pi with our
//! extension in a container, every tool call goes past Gateway, and the agent answers.
//! The model is a stub playing a fixed script, so no provider and no tokens are involved.
//!
//! Needs Docker and the agent image, so a plain `cargo test` skips it (D79):
//!
//! ```text
//! docker build -t sona-plane-agent:dev images/agent
//! cargo test -p control-plane --test e2e -- --ignored
//! ```
//!
//! The defaults fit OrbStack and Docker Desktop. On Linux, agents join a network and
//! the control plane listens on its gateway (D88):
//!
//! ```text
//! SONA_E2E_LISTEN=172.30.0.1 SONA_E2E_HOST=172.30.0.1 SONA_E2E_NETWORK=sona-agents
//! ```

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

/// What the stub model answers, one step per request.
enum Step {
    Call(&'static str, Value),
    Say(&'static str),
}

/// Picked by how many tool results the request already carries.
fn script() -> Vec<Step> {
    vec![
        Step::Call("write", json!({ "path": "notes.md", "content": "hello" })),
        // Outside the workdir: Gateway must deny it.
        Step::Call("read", json!({ "path": "/etc/hosts" })),
        Step::Call("send_message", json!({ "to": "@user", "body": "done" })),
        Step::Say("finished"),
    ]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker and the sona-plane-agent:dev image"]
async fn an_agent_answers_the_operator_past_gateway() {
    let model = Arc::new(Model::default());
    let (root, control_plane) = start("e2e-answer", model.clone()).await;
    let operator = &control_plane.operator;
    add_coder(operator).await;
    let sent = send(operator, "start", None).await;
    let chain = sent["correlation_id"].as_str().unwrap().to_owned();

    // The agent answers in the operator's chain.
    let replies = eventually("the reply", || async {
        let replies = operator
            .call("GET", &format!("/v1/sessions/chain-{chain}/messages"), None)
            .await;
        replies
            .as_array()
            .is_some_and(|r| !r.is_empty())
            .then_some(replies)
    })
    .await;
    assert_eq!(replies[0]["from"], "coder");
    assert_eq!(replies[0]["body"], "done");

    // The allowed write landed in the agent's workdir.
    let notes = root.join("data/agents/coder/work/notes.md");
    assert_eq!(std::fs::read_to_string(notes).unwrap(), "hello");

    // Every tool call is in the journal with Gateway's decision, and the Run ended well.
    let journal = eventually("the finished Run", || async {
        let entries = operator
            .call("GET", &format!("/v1/journal?chain={chain}"), None)
            .await;
        let finished = entries
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "run_finished");
        finished.then_some(entries)
    })
    .await;
    let decisions: Vec<(String, String)> = journal
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["component"] == "gateway")
        .map(|e| (text(&e["action"]), text(&e["decision"])))
        .collect();
    let expected = [
        ("write", "allow"),
        ("read", "deny"),
        ("send_message", "allow"),
    ];
    assert_eq!(
        decisions,
        expected.map(|(a, d)| (a.to_owned(), d.to_owned())).to_vec()
    );
    let finished = journal
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "run_finished")
        .unwrap();
    assert_eq!(finished["result"], "ok");

    // The Run is kept with the tokens Pi reported.
    let runs = operator.call("GET", "/v1/runs?agent=coder", None).await;
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["outcome"], "ok");
    assert_eq!(runs[0]["correlation_id"], chain.as_str());
    assert!(runs[0]["usage"]["tokens_in"].as_u64().unwrap() > 0);
    assert_eq!(runs[0]["card_version"], 1);

    // Afterwards the agent is idle with nothing left to deliver.
    let status = operator.call("GET", "/v1/status", None).await;
    assert_eq!(
        status,
        json!({ "agents": [{ "alias": "coder", "title": "Writes code", "state": "active", "run": null,
                             "unread": 0, "failing": [] }],
                "stopped_chains": [] })
    );

    // An edit is the next version; one based on the old version is refused.
    let mut agent = operator.call("GET", "/v1/agents/coder", None).await;
    agent["title"] = json!("Writes and tests code");
    let saved = operator
        .call("PUT", "/v1/agents/coder", Some(agent.clone()))
        .await;
    assert_eq!(saved["version"], 2);
    let stale = operator
        .try_call("PUT", "/v1/agents/coder", Some(agent))
        .await;
    assert!(stale.unwrap_err().starts_with("409"));

    // A disabled agent takes no messages until enabled.
    operator
        .call("POST", "/v1/agents/coder/disable", None)
        .await;
    let message = json!({ "to": "coder", "body": "hi", "subject": null,
                          "task_id": null, "in_reply_to": null });
    let refused = operator
        .try_call("POST", "/v1/messages", Some(message))
        .await;
    assert!(refused.unwrap_err().starts_with("403"));
    operator.call("POST", "/v1/agents/coder/enable", None).await;

    // The model saw why the read was refused.
    let requests = model.requests.lock().unwrap();
    let seen = serde_json::to_string(&requests.last().unwrap()["messages"]).unwrap();
    assert!(seen.contains("read may not access /etc/hosts"), "{seen}");

    drop(requests);
    drop(control_plane);
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker and the sona-plane-agent:dev image"]
async fn a_stopped_chain_wakes_nobody_until_resumed() {
    // The model never answers, so the Run stays busy until it is aborted.
    let model = Arc::new(Model {
        hang: true,
        ..Model::default()
    });
    let (root, control_plane) = start("e2e-stop", model).await;
    let operator = &control_plane.operator;
    add_coder(operator).await;
    let sent = send(operator, "start", None).await;
    let chain = text(&sent["correlation_id"]);
    let count = |action: &'static str| {
        let chain = chain.clone();
        async move {
            let journal = operator
                .call("GET", &format!("/v1/journal?chain={chain}"), None)
                .await;
            journal
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["action"] == action)
                .count()
        }
    };
    eventually("the busy Run", || async {
        let status = operator.call("GET", "/v1/status", None).await;
        (!status["agents"][0]["run"].is_null()).then_some(())
    })
    .await;

    operator
        .call("POST", &format!("/v1/chains/{chain}/stop"), None)
        .await;
    eventually("the aborted Run", || async {
        (count("run_finished").await == 1).then_some(())
    })
    .await;
    let journal = operator
        .call("GET", &format!("/v1/journal?chain={chain}"), None)
        .await;
    let finished = journal
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "run_finished")
        .unwrap();
    assert_eq!(finished["reason"], "the chain was stopped");

    // The operator's answer is kept and the operator is told, but nobody wakes.
    let answer = send(operator, "more", Some(text(&sent["id"]))).await;
    assert_eq!(answer["chain_stopped"], true);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(count("run_started").await, 1);
    let status = operator.call("GET", "/v1/status", None).await;
    assert_eq!(status["stopped_chains"][0]["chain"], json!(chain));
    assert_eq!(status["stopped_chains"][0]["reason"], Value::Null);
    assert_eq!(status["agents"][0]["run"], Value::Null);
    assert_eq!(status["agents"][0]["failing"], json!([]));

    // Resumed, the chain is delivered again, the answer with it.
    operator
        .call("POST", &format!("/v1/chains/{chain}/resume"), None)
        .await;
    eventually("the resumed Run", || async {
        (count("run_started").await == 2).then_some(())
    })
    .await;

    // Leave nothing running.
    operator
        .call("POST", &format!("/v1/chains/{chain}/stop"), None)
        .await;
    eventually("the second abort", || async {
        (count("run_finished").await == 2).then_some(())
    })
    .await;
    drop(control_plane);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A fresh control plane in its own directory, with the stub model.
async fn start(name: &str, model: Arc<Model>) -> (PathBuf, ControlPlane) {
    let site = Site::from_env();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let model_port = model.serve(site.listen).await;
    let control_plane = ControlPlane::start(&site, &root, model_port).await;
    (root, control_plane)
}

async fn add_coder(operator: &Operator) {
    let card = json!({
        "alias": "coder",
        "title": "Writes code",
        "model": { "provider": "fake", "id": "fake-1" },
        "access": ["read_files", "write_files", "send_messages"],
        "allow_message_to": ["user"],
        "prompt": "You are @coder.",
    });
    operator.call("POST", "/v1/agents", Some(card)).await;
}

/// The operator writes to `@coder`, in a new chain or in reply to a message.
async fn send(operator: &Operator, body: &str, in_reply_to: Option<String>) -> Value {
    let message = json!({ "to": "coder", "body": body, "subject": null,
                          "task_id": null, "in_reply_to": in_reply_to });
    operator.call("POST", "/v1/messages", Some(message)).await
}

/// Where things listen and how containers reach them.
struct Site {
    listen: IpAddr,
    host: String,
    network: Option<String>,
}

impl Site {
    fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            listen: var("SONA_E2E_LISTEN")
                .map_or(IpAddr::from([127, 0, 0, 1]), |ip| ip.parse().unwrap()),
            host: var("SONA_E2E_HOST").unwrap_or_else(|| "host.docker.internal".to_owned()),
            network: var("SONA_E2E_NETWORK"),
        }
    }
}

/// The stub model: an OpenAI-compatible endpoint that streams the next step.
#[derive(Default)]
struct Model {
    requests: Mutex<Vec<Value>>,
    /// Never answers.
    hang: bool,
}

impl Model {
    async fn serve(self: Arc<Self>, ip: IpAddr) -> u16 {
        let listener = TcpListener::bind((ip, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/v1/chat/completions", post(complete))
            .with_state(self);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }
}

async fn complete(
    State(model): State<Arc<Model>>,
    Json(request): Json<Value>,
) -> impl IntoResponse {
    let results = request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "tool")
        .count();
    model.requests.lock().unwrap().push(request);
    if model.hang {
        std::future::pending::<()>().await;
    }

    let script = script();
    let (delta, finish) = match &script[results.min(script.len() - 1)] {
        Step::Call(tool, args) => (
            json!({ "role": "assistant", "tool_calls": [{
                "index": 0, "id": format!("call_{results}"), "type": "function",
                "function": { "name": tool, "arguments": args.to_string() },
            }] }),
            "tool_calls",
        ),
        Step::Say(text) => (json!({ "role": "assistant", "content": text }), "stop"),
    };
    let chunk = |choices: Value| {
        json!({ "id": "stub", "object": "chat.completion.chunk", "created": 0,
                "model": "fake-1", "choices": choices })
    };
    let mut usage = chunk(json!([]));
    usage["usage"] = json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 });
    let events = [
        chunk(json!([{ "index": 0, "delta": delta, "finish_reason": null }])),
        chunk(json!([{ "index": 0, "delta": {}, "finish_reason": finish }])),
        usage,
    ];
    let mut body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

/// The real binary, initialized and serving. Killed when dropped.
struct ControlPlane {
    _process: Child,
    operator: Operator,
}

impl ControlPlane {
    async fn start(site: &Site, root: &Path, model_port: u16) -> Self {
        let api = SocketAddr::new(site.listen, free_port(site.listen).await);
        let extension = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extension/src");
        let models = root.join("models.json");
        let provider = json!({ "providers": { "fake": {
            "baseUrl": format!("http://{}:{model_port}/v1", site.host),
            "api": "openai-completions",
            "apiKey": "$FAKE_LLM_KEY",
            "models": [{ "id": "fake-1" }],
        } } });
        std::fs::write(&models, provider.to_string()).unwrap();

        let mut config = format!(
            "operator = \"user\"\n\
             data_dir = \"{}\"\n\
             listen = \"{api}\"\n\
             api_url = \"http://{}:{}\"\n\
             agent_image = \"sona-plane-agent:dev\"\n\
             extension = \"{}\"\n\
             models_json = \"{}\"\n\
             provider_env = [\"FAKE_LLM_KEY\"]\n",
            root.join("data").display(),
            site.host,
            api.port(),
            extension.canonicalize().unwrap().display(),
            models.display(),
        );
        if let Some(network) = &site.network {
            config.push_str(&format!("agent_network = \"{network}\"\n"));
        }
        let config_path = root.join("control-plane.toml");
        std::fs::write(&config_path, config).unwrap();

        let binary = env!("CARGO_BIN_EXE_control-plane");
        let init = Command::new(binary)
            .arg("--config")
            .arg(&config_path)
            .arg("init")
            .output()
            .await
            .unwrap();
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let token = String::from_utf8(init.stdout).unwrap().trim().to_owned();

        let process = Command::new(binary)
            .arg("--config")
            .arg(&config_path)
            .arg("serve")
            .env("FAKE_LLM_KEY", "stub")
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let operator = Operator {
            http: reqwest::Client::new(),
            url: format!("http://{api}"),
            token,
        };
        eventually("the API", || async {
            operator.try_call("GET", "/v1/agents", None).await.ok()
        })
        .await;
        Self {
            _process: process,
            operator,
        }
    }
}

struct Operator {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Operator {
    /// A successful call's JSON reply, `null` when empty.
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        self.try_call(method, path, body).await.unwrap()
    }

    async fn try_call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let method = method.parse().unwrap();
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let text = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("{status}: {text}"));
        }
        Ok(if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        })
    }
}

/// Polls until `check` yields a value. The first Run also starts a container.
async fn eventually<T, F: Future<Output = Option<T>>>(what: &str, check: impl Fn() -> F) -> T {
    for _ in 0..240 {
        if let Some(value) = check().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn free_port(ip: IpAddr) -> u16 {
    let listener = TcpListener::bind((ip, 0)).await.unwrap();
    listener.local_addr().unwrap().port()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}
