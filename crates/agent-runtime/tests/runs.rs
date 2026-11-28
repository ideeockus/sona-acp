//! The Run loop against in-memory stand-ins for Registry, Broker, Journal and Pi.

use async_trait::async_trait;
use control_plane_agent_runtime::launcher::{Launched, Launcher, Process, RunSpec};
use control_plane_agent_runtime::{Limits, Runtime, RuntimeConfig};
use control_plane_common::broker::{InboxBatch, InboxDelivery, Message, Notifications};
use control_plane_common::journal::{Event, JournalWriter, Outcome};
use control_plane_common::registry::{
    Access, Agent, AgentCard, AgentDirectory, AgentState, Credentials, ModelRef, Secret, Session,
};
use control_plane_common::runtime::{RunControl, RunFilter, RunOutcome, Usage};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, RunId, SessionKey, TaskId, Timestamp,
};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, broadcast, oneshot};

// ---- Registry ----

/// `coder` and `manager`; the disabled set is shared with the inbox, as Registry is
/// with Broker.
struct Directory(Arc<Mutex<HashSet<Alias>>>);

#[async_trait]
impl AgentDirectory for Directory {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        let (title, allow_message_to) = match alias.as_str() {
            "coder" => ("Writes code", vec![alias_of("manager"), alias_of("user")]),
            "manager" => ("Coordinates work", vec![alias_of("coder")]),
            _ => return Err(Error::NotFound(format!("agent {alias}"))),
        };
        Ok(AgentCard {
            alias: alias.clone(),
            title: title.to_owned(),
            model: ModelRef {
                provider: "fake".to_owned(),
                id: "fake-1".to_owned(),
            },
            access: vec![Access::ReadFiles, Access::SendMessages],
            allow_message_to,
            prompt: format!("You are {alias}."),
            hue: None,
        })
    }

    async fn record(&self, alias: &Alias) -> Result<Agent> {
        let card = self.agent(alias).await?;
        let disabled = self.0.lock().unwrap().contains(alias);
        Ok(Agent {
            card,
            version: 7,
            state: if disabled {
                AgentState::Disabled
            } else {
                AgentState::Active
            },
        })
    }
}

#[derive(Default)]
struct Tokens {
    issued: Mutex<Vec<RunId>>,
    revoked: Mutex<Vec<RunId>>,
    revoked_all: AtomicBool,
}

#[async_trait]
impl Credentials for Tokens {
    async fn create_operator(&self, _: &Alias) -> Result<Secret> {
        unimplemented!()
    }
    async fn issue_run_token(&self, _: &Alias, run_id: RunId, _: CorrelationId) -> Result<Secret> {
        self.issued.lock().unwrap().push(run_id);
        Ok(Secret::new(format!("sona_plane_rt_{run_id}")))
    }
    async fn revoke_run_token(&self, run_id: RunId) -> Result<()> {
        self.revoked.lock().unwrap().push(run_id);
        Ok(())
    }
    async fn revoke_all_run_tokens(&self) -> Result<()> {
        self.revoked_all.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn authenticate(&self, _: &str) -> Result<Caller> {
        unimplemented!()
    }
    async fn contains_active_run_token(&self, _: &str) -> Result<bool> {
        unimplemented!()
    }
    async fn open_session(&self, _: &str) -> Result<Session> {
        unimplemented!()
    }
    async fn session_caller(&self, _: &str) -> Result<Caller> {
        unimplemented!()
    }
    async fn close_session(&self, _: &str) -> Result<()> {
        unimplemented!()
    }
}

// ---- Broker ----

/// Messages in delivery order, the ids already read, failures in a row and stuck
/// sessions, and notifications like Broker's.
struct Inbox {
    messages: Mutex<Vec<Message>>,
    read: Mutex<HashSet<MessageId>>,
    failures: Mutex<HashMap<SessionKey, u32>>,
    stuck: Mutex<HashSet<SessionKey>>,
    stopped: Mutex<HashSet<CorrelationId>>,
    /// Why the system halted a chain (D100).
    halted: Mutex<HashMap<CorrelationId, String>>,
    resumed: Mutex<HashMap<CorrelationId, Timestamp>>,
    /// Disabled agents get nothing, as from Broker (D96).
    disabled: Arc<Mutex<HashSet<Alias>>>,
    notifications: broadcast::Sender<Alias>,
}

impl Default for Inbox {
    fn default() -> Self {
        Self {
            messages: Mutex::default(),
            read: Mutex::default(),
            failures: Mutex::default(),
            stuck: Mutex::default(),
            stopped: Mutex::default(),
            halted: Mutex::default(),
            resumed: Mutex::default(),
            disabled: Arc::default(),
            notifications: broadcast::channel(16).0,
        }
    }
}

impl Inbox {
    /// Stores a message and notifies, as Broker does on send.
    fn add(&self, message: Message) {
        let to = message.to.clone();
        self.messages.lock().unwrap().push(message);
        let _ = self.notifications.send(to);
    }

    /// Resumes a chain and wakes its agent, as Broker does for the operator.
    fn resume(&self, chain: CorrelationId, agent: &str) {
        self.stopped.lock().unwrap().remove(&chain);
        self.halted.lock().unwrap().remove(&chain);
        self.resumed.lock().unwrap().insert(chain, Timestamp::now());
        let _ = self.notifications.send(alias_of(agent));
    }

    fn halted(&self, chain: CorrelationId) -> Option<String> {
        self.halted.lock().unwrap().get(&chain).cloned()
    }

    fn is_read(&self, message: &Message) -> bool {
        self.read.lock().unwrap().contains(&message.id)
    }

    fn unread(&self, agent: &Alias) -> Vec<Message> {
        let read = self.read.lock().unwrap();
        self.messages
            .lock()
            .unwrap()
            .iter()
            .filter(|m| &m.to == agent && !read.contains(&m.id))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl InboxDelivery for Inbox {
    fn subscribe(&self) -> Notifications {
        self.notifications.subscribe()
    }

    async fn agents_with_unread(&self) -> Result<Vec<Alias>> {
        let mut agents: Vec<Alias> = ["coder", "manager"]
            .map(alias_of)
            .into_iter()
            .filter(|a| !self.unread(a).is_empty())
            .collect();
        agents.dedup();
        Ok(agents)
    }

    async fn next_batch(&self, agent: &Alias) -> Result<Option<InboxBatch>> {
        if self.disabled.lock().unwrap().contains(agent) {
            return Ok(None);
        }
        let mut unread = self.unread(agent);
        let stuck = self.stuck.lock().unwrap().clone();
        let stopped = self.stopped.lock().unwrap().clone();
        unread.retain(|m| !stuck.contains(&m.session()) && !stopped.contains(&m.correlation_id));
        let Some(session) = unread.first().map(Message::session) else {
            return Ok(None);
        };
        let messages = unread
            .into_iter()
            .filter(|m| m.session() == session)
            .collect();
        Ok(Some(InboxBatch { session, messages }))
    }

    async fn unread_in(&self, agent: &Alias, session: SessionKey) -> Result<Vec<Message>> {
        let mut unread = self.unread(agent);
        unread.retain(|m| m.session() == session);
        Ok(unread)
    }

    async fn advance_cursor(
        &self,
        agent: &Alias,
        session: SessionKey,
        up_to: MessageId,
    ) -> Result<()> {
        let messages = self.messages.lock().unwrap();
        let mut read = self.read.lock().unwrap();
        for message in messages
            .iter()
            .filter(|m| &m.to == agent && m.session() == session)
        {
            read.insert(message.id);
            if message.id == up_to {
                break;
            }
        }
        self.failures.lock().unwrap().remove(&session);
        Ok(())
    }

    async fn record_failure(&self, _: &Alias, session: SessionKey, _: &str) -> Result<u32> {
        let mut failures = self.failures.lock().unwrap();
        let count = failures.entry(session).or_default();
        *count += 1;
        Ok(*count)
    }

    async fn mark_stuck(&self, _: &Alias, session: SessionKey) -> Result<()> {
        self.stuck.lock().unwrap().insert(session);
        Ok(())
    }

    async fn is_stopped(&self, chain: CorrelationId) -> Result<bool> {
        Ok(self.stopped.lock().unwrap().contains(&chain))
    }

    async fn halt_chain(&self, chain: CorrelationId, reason: &str) -> Result<()> {
        self.stopped.lock().unwrap().insert(chain);
        self.halted.lock().unwrap().insert(chain, reason.to_owned());
        Ok(())
    }

    async fn resumed_at(&self, chain: CorrelationId) -> Result<Option<Timestamp>> {
        Ok(self.resumed.lock().unwrap().get(&chain).copied())
    }
    async fn is_read(&self, _: MessageId) -> Result<bool> {
        unimplemented!()
    }
}

// ---- Journal ----

#[derive(Default)]
struct Journal(Mutex<Vec<Event>>);

#[async_trait]
impl JournalWriter for Journal {
    async fn record(&self, event: Event) -> Result<()> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

impl Journal {
    fn actions(&self) -> Vec<(String, Option<Outcome>)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|e| (e.action.clone(), e.result))
            .collect()
    }
}

// ---- Pi ----

/// How the next fake Pi behaves after the prompt.
enum Script {
    /// Uses one tool and settles.
    Settle,
    /// Uses one tool and settles once released, reading steers meanwhile.
    SettleWhen(Arc<Notify>),
    /// Exits with an error without settling.
    Crash,
    /// Settles normally after the model provider failed.
    ModelError,
    /// Uses one tool and settles only when aborted.
    UntilAbort,
    /// Starts working once released, then settles only when aborted.
    StartWhen(Arc<Notify>),
}

#[derive(Default)]
struct FakePi {
    scripts: Mutex<VecDeque<Script>>,
    specs: Mutex<Vec<RunSpec>>,
    /// Command lines received by every launched Pi, in order.
    received: Arc<Mutex<Vec<String>>>,
}

struct FakeProcess(Option<oneshot::Receiver<i32>>);

#[async_trait]
impl Process for FakeProcess {
    async fn wait(&mut self) -> Result<Option<i32>> {
        let exit = self.0.take().expect("waited twice");
        Ok(exit.await.ok())
    }
    async fn kill(&mut self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Launcher for FakePi {
    async fn start(&self, spec: RunSpec) -> Result<Launched> {
        let listing = spec.args.iter().any(|a| a == "--no-session");
        self.specs.lock().unwrap().push(spec);
        if listing {
            return Ok(list_models());
        }
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Script::Settle);
        let (stdin, pi_stdin) = tokio::io::duplex(4096);
        let (mut pi_stdout, stdout) = tokio::io::duplex(4096);
        let (exit, exited) = oneshot::channel();
        let received = self.received.clone();

        tokio::spawn(async move {
            let (first_line, first) = oneshot::channel();
            let aborted = Arc::new(Notify::new());
            let abort = aborted.clone();
            // Reads commands until Runtime closes stdin.
            let reader = tokio::spawn(async move {
                let mut lines = BufReader::new(pi_stdin).lines();
                let mut first_line = Some(first_line);
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.contains(r#""type":"abort""#) {
                        abort.notify_one();
                    }
                    received.lock().unwrap().push(line);
                    if let Some(signal) = first_line.take() {
                        let _ = signal.send(());
                    }
                }
            });
            let _ = first.await;

            if let Script::Crash = script {
                drop(pi_stdout);
                let _ = exit.send(1);
                return;
            }
            if let Script::StartWhen(release) = &script {
                release.notified().await;
            }
            pi_stdout
                .write_all(b"{\"type\":\"agent_start\"}\n")
                .await
                .unwrap();
            let answer = br#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"toolUse","usage":{"input":100,"output":20,"cacheRead":30,"cacheWrite":0,"totalTokens":150,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0.0015}}}}"#;
            pi_stdout.write_all(answer).await.unwrap();
            pi_stdout.write_all(b"\n").await.unwrap();
            let tool = br#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"read","result":{},"isError":false}"#;
            pi_stdout.write_all(tool).await.unwrap();
            pi_stdout.write_all(b"\n").await.unwrap();
            if let Script::SettleWhen(release) = &script {
                release.notified().await;
            }
            if let Script::UntilAbort | Script::StartWhen(_) = script {
                aborted.notified().await;
            }
            if let Script::ModelError = script {
                let failed = br#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"503 from provider"}}"#;
                pi_stdout.write_all(failed).await.unwrap();
                pi_stdout.write_all(b"\n").await.unwrap();
            }
            pi_stdout
                .write_all(b"{\"type\":\"agent_settled\"}\n")
                .await
                .unwrap();
            let _ = reader.await;
            let _ = exit.send(0);
        });

        Ok(Launched {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            process: Box::new(FakeProcess(Some(exited))),
        })
    }
}

/// A Pi asked only for its models (D104).
fn list_models() -> Launched {
    let (stdin, pi_stdin) = tokio::io::duplex(4096);
    let (mut pi_stdout, stdout) = tokio::io::duplex(4096);
    let (exit, exited) = oneshot::channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(pi_stdin).lines();
        if let Ok(Some(line)) = lines.next_line().await {
            assert_eq!(line, r#"{"type":"get_available_models"}"#);
            let answer = br#"{"type":"response","command":"get_available_models","success":true,"data":{"models":[{"id":"fake-1","name":"Fake","provider":"fake","contextWindow":1000,"cost":{"input":1,"output":2}}]}}"#;
            pi_stdout.write_all(answer).await.unwrap();
            pi_stdout.write_all(b"\n").await.unwrap();
        }
        while let Ok(Some(_)) = lines.next_line().await {}
        let _ = exit.send(0);
    });
    Launched {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        process: Box::new(FakeProcess(Some(exited))),
    }
}

// ---- Helpers ----

struct Setup {
    runtime: Runtime,
    data_dir: PathBuf,
    tokens: Arc<Tokens>,
    inbox: Arc<Inbox>,
    journal: Arc<Journal>,
    pi: Arc<FakePi>,
}

/// An in-memory database that lives as long as the pool.
async fn memory_pool() -> SqlitePool {
    // One connection that never closes: every connection gets its own in-memory database.
    SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap()
}

fn operator() -> Caller {
    Caller::Operator {
        alias: alias_of("user"),
    }
}

async fn setup(scripts: Vec<Script>) -> Setup {
    setup_on(memory_pool().await, scripts).await
}

async fn setup_on(pool: SqlitePool, scripts: Vec<Script>) -> Setup {
    setup_with(pool, scripts, Limits::default()).await
}

async fn setup_with(pool: SqlitePool, scripts: Vec<Script>, limits: Limits) -> Setup {
    let data_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(uuid_dir());
    let tokens = Arc::new(Tokens::default());
    let inbox = Arc::new(Inbox::default());
    let journal = Arc::new(Journal::default());
    let pi = Arc::new(FakePi {
        scripts: Mutex::new(scripts.into()),
        ..FakePi::default()
    });
    let config = RuntimeConfig {
        data_dir: data_dir.clone(),
        extension: PathBuf::from("/opt/sona/extension"),
        api_url: "http://api.test".to_owned(),
        models_json: None,
        provider_env: vec![("FAKE_KEY".to_owned(), "k".to_owned())],
        retry_delays: vec![Duration::from_millis(10), Duration::from_millis(20)],
        limits,
    };
    let runtime = Runtime::new(
        config,
        pool,
        Arc::new(Directory(inbox.disabled.clone())),
        tokens.clone(),
        inbox.clone(),
        journal.clone(),
        pi.clone(),
    )
    .await
    .unwrap();
    Setup {
        runtime,
        data_dir,
        tokens,
        inbox,
        journal,
        pi,
    }
}

fn uuid_dir() -> String {
    format!("runtime-{}", RunId::new())
}

fn alias_of(name: &str) -> Alias {
    name.parse().unwrap()
}

fn message(to: &str, task_id: Option<TaskId>, chain: CorrelationId) -> Message {
    Message {
        id: MessageId::new(),
        from: alias_of("manager"),
        to: alias_of(to),
        body: "do it".to_owned(),
        subject: None,
        task_id,
        in_reply_to: None,
        attachments: Vec::new(),
        correlation_id: chain,
        created_at: Timestamp::now(),
    }
}

/// Waits up to five seconds for `condition`.
async fn eventually(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn arg_after<'a>(spec: &'a RunSpec, flag: &str) -> &'a str {
    let at = spec.args.iter().position(|a| a == flag).unwrap();
    &spec.args[at + 1]
}

fn env_of<'a>(spec: &'a RunSpec, name: &str) -> &'a str {
    &spec.env.iter().find(|(k, _)| k == name).unwrap().1
}

// ---- Tests ----

#[tokio::test]
async fn a_run_delivers_its_batch_and_cleans_up() {
    let s = setup(vec![Script::Settle]).await;
    let task = TaskId::new();
    let chain = CorrelationId::new();
    let (first, second) = (
        message("coder", Some(task), chain),
        message("coder", Some(task), chain),
    );
    s.inbox.add(first.clone());
    s.inbox.add(second.clone());

    s.runtime.start().await.unwrap();
    eventually("both messages read", || {
        s.inbox.is_read(&first) && s.inbox.is_read(&second)
    })
    .await;
    eventually("run finished", || s.journal.actions().len() == 3).await;

    let spec = s.pi.specs.lock().unwrap()[0].clone();
    assert_eq!(arg_after(&spec, "--session-id"), format!("task-{task}"));
    assert_eq!(arg_after(&spec, "--model"), "fake/fake-1");
    assert_eq!(
        arg_after(&spec, "--tools"),
        "read_messages,read,ls,grep,find,send_message"
    );
    assert_eq!(arg_after(&spec, "-e"), "/agent/extension");
    assert_eq!(arg_after(&spec, "--session-dir"), "/agent/sessions");
    assert_eq!(arg_after(&spec, "--system-prompt"), "You are @coder.");
    let context = arg_after(&spec, "--append-system-prompt");
    assert!(context.contains("- @manager: Coordinates work\n- @user\n"));
    for flag in [
        "--no-extensions",
        "--no-skills",
        "--no-context-files",
        "--no-approve",
    ] {
        assert!(spec.args.iter().any(|a| a == flag), "missing {flag}");
    }
    assert!(env_of(&spec, "SONA_PLANE_RUN_TOKEN").starts_with("sona_plane_rt_"));
    assert_eq!(env_of(&spec, "SONA_PLANE_API_URL"), "http://api.test");
    assert_eq!(env_of(&spec, "SONA_PLANE_SESSION"), format!("task-{task}"));
    assert_eq!(env_of(&spec, "FAKE_KEY"), "k");
    assert_eq!(env_of(&spec, "PI_CODING_AGENT_DIR"), "/agent/pi");
    let mounted = |inside: &str| {
        let mount = spec.mounts.iter().find(|m| m.inside == inside).unwrap();
        (mount.host.clone(), mount.read_only)
    };
    assert_eq!(
        mounted("/work"),
        (s.data_dir.join("agents/coder/work"), false)
    );
    assert_eq!(
        mounted("/agent/sessions"),
        (s.data_dir.join("agents/coder/sessions"), false)
    );
    assert_eq!(
        mounted("/agent/extension"),
        (PathBuf::from("/opt/sona/extension"), true)
    );
    assert!(s.data_dir.join("agents/coder/work").is_dir());

    // The prompt is a notification only: Pi reads the messages itself (D80).
    let received = s.pi.received.lock().unwrap().clone();
    assert_eq!(
        received,
        [r#"{"type":"prompt","message":"<new_messages count=\"2\" />"}"#]
    );

    assert_eq!(
        *s.tokens.issued.lock().unwrap(),
        *s.tokens.revoked.lock().unwrap()
    );
    // The per-Run config is gone after the Run (D81).
    let (pi_dir, _) = mounted("/agent/pi");
    assert!(pi_dir.starts_with(s.data_dir.join("runs")));
    assert!(!pi_dir.exists());
    assert_eq!(
        s.journal.actions(),
        [
            ("run_started".to_owned(), None),
            ("read".to_owned(), Some(Outcome::Ok)),
            ("run_finished".to_owned(), Some(Outcome::Ok)),
        ]
    );
}

#[tokio::test]
async fn same_session_is_steered_other_sessions_wait() {
    let release = Arc::new(Notify::new());
    let s = setup(vec![Script::SettleWhen(release.clone()), Script::Settle]).await;
    let (task, other) = (TaskId::new(), TaskId::new());
    let chain = CorrelationId::new();
    let first = message("coder", Some(task), chain);
    s.inbox.add(first.clone());

    s.runtime.start().await.unwrap();
    eventually("prompt", || s.pi.received.lock().unwrap().len() == 1).await;

    let follow_up = message("coder", Some(task), chain);
    let elsewhere = message("coder", Some(other), CorrelationId::new());
    s.inbox.add(follow_up.clone());
    s.inbox.add(elsewhere.clone());
    eventually("steer", || s.pi.received.lock().unwrap().len() == 2).await;
    assert_eq!(
        s.pi.received.lock().unwrap()[1],
        r#"{"type":"steer","message":"<new_message from=\"@manager\" />"}"#
    );

    release.notify_one();
    eventually("all read", || {
        [&first, &follow_up, &elsewhere]
            .iter()
            .all(|m| s.inbox.is_read(m))
    })
    .await;

    // The other task got its own Run, in its own session.
    let specs = s.pi.specs.lock().unwrap();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        arg_after(&specs[1], "--session-id"),
        format!("task-{other}")
    );
}

#[tokio::test]
async fn a_crashed_run_is_retried_after_a_pause() {
    let s = setup(vec![Script::Crash, Script::Settle]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());

    s.runtime.start().await.unwrap();
    eventually("message read", || s.inbox.is_read(&first)).await;

    let finished: Vec<_> = s
        .journal
        .actions()
        .into_iter()
        .filter(|(action, _)| action == "run_finished")
        .map(|(_, result)| result)
        .collect();
    assert_eq!(finished, [Some(Outcome::Error), Some(Outcome::Ok)]);
    // Every Run's token died with it, the failed one too.
    assert_eq!(
        *s.tokens.issued.lock().unwrap(),
        *s.tokens.revoked.lock().unwrap()
    );
    assert!(s.inbox.failures.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_session_is_stuck_after_the_last_attempt() {
    let s = setup(vec![Script::Crash, Script::ModelError, Script::Crash]).await;
    let failing = message("coder", None, CorrelationId::new());
    let other = message("coder", None, CorrelationId::new());
    s.inbox.add(failing.clone());
    s.inbox.add(other.clone());

    s.runtime.start().await.unwrap();
    // Once the failing session is stuck, the agent's other sessions go on.
    eventually("other session read", || s.inbox.is_read(&other)).await;

    assert!(!s.inbox.is_read(&failing));
    assert!(s.inbox.stuck.lock().unwrap().contains(&failing.session()));
    assert_eq!(s.pi.specs.lock().unwrap().len(), 4);
    let events = s.journal.0.lock().unwrap();
    let stuck = events.iter().find(|e| e.action == "session_stuck").unwrap();
    assert_eq!(stuck.correlation_id, Some(failing.correlation_id));
    assert_eq!(
        stuck.reason.as_deref(),
        Some("internal error: Pi exited before settling")
    );
}

#[tokio::test]
async fn a_failed_model_fails_the_run() {
    let s = setup(vec![Script::ModelError, Script::Settle]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());

    s.runtime.start().await.unwrap();
    eventually("message read", || s.inbox.is_read(&first)).await;

    let events = s.journal.0.lock().unwrap();
    let failed = events
        .iter()
        .find(|e| e.action == "run_finished" && e.result == Some(Outcome::Error))
        .unwrap();
    assert_eq!(
        failed.reason.as_deref(),
        Some("internal error: the model failed: 503 from provider")
    );
}

#[tokio::test]
async fn the_operator_sees_active_runs() {
    let release = Arc::new(Notify::new());
    let s = setup(vec![Script::SettleWhen(release.clone())]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());
    let operator = Caller::Operator {
        alias: alias_of("user"),
    };

    s.runtime.start().await.unwrap();
    eventually("run started", || s.pi.specs.lock().unwrap().len() == 1).await;
    let runs = s.runtime.active_runs(&operator).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].agent.as_str(), runs[0].session),
        ("coder", first.session())
    );

    let agent = Caller::Agent {
        alias: alias_of("coder"),
        run_id: runs[0].run_id,
        correlation_id: first.correlation_id,
    };
    assert!(matches!(
        s.runtime.active_runs(&agent).await,
        Err(Error::Forbidden(_))
    ));

    release.notify_one();
    eventually("message read", || s.inbox.is_read(&first)).await;
    assert!(s.runtime.active_runs(&operator).await.unwrap().is_empty());
}

#[tokio::test]
async fn stopping_a_chain_aborts_its_run_without_a_failure() {
    let s = setup(vec![Script::UntilAbort]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());
    let operator = Caller::Operator {
        alias: alias_of("user"),
    };

    s.runtime.start().await.unwrap();
    eventually("run started", || s.pi.specs.lock().unwrap().len() == 1).await;
    // Broker first, then Runtime, as the API does.
    s.inbox.stopped.lock().unwrap().insert(first.correlation_id);
    s.runtime
        .abort_chain(&operator, first.correlation_id)
        .await
        .unwrap();
    eventually("run finished", || {
        s.journal.actions().iter().any(|(a, _)| a == "run_finished")
    })
    .await;

    {
        let events = s.journal.0.lock().unwrap();
        let finished = events.iter().find(|e| e.action == "run_finished").unwrap();
        assert_eq!(finished.result, None);
        assert_eq!(finished.reason.as_deref(), Some("the chain was stopped"));
    }
    assert!(
        s.pi.received
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("abort"))
    );
    assert!(!s.inbox.is_read(&first));
    assert!(s.inbox.failures.lock().unwrap().is_empty());
    assert_eq!(
        *s.tokens.issued.lock().unwrap(),
        *s.tokens.revoked.lock().unwrap()
    );
    // Not retried: the stopped chain's message is not delivered.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.pi.specs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_stop_before_pi_starts_working_aborts_once_it_does() {
    let start = Arc::new(Notify::new());
    let s = setup(vec![Script::StartWhen(start.clone())]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());
    let operator = Caller::Operator {
        alias: alias_of("user"),
    };

    s.runtime.start().await.unwrap();
    eventually("prompt sent", || !s.pi.received.lock().unwrap().is_empty()).await;
    s.inbox.stopped.lock().unwrap().insert(first.correlation_id);
    s.runtime
        .abort_chain(&operator, first.correlation_id)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // An abort now would be lost, so none is sent yet.
    assert!(
        !s.pi
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("abort"))
    );

    start.notify_one();
    eventually("run finished", || {
        s.journal.actions().iter().any(|(a, _)| a == "run_finished")
    })
    .await;
    assert!(
        s.pi.received
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("abort"))
    );
}

#[tokio::test]
async fn aborting_another_chain_leaves_the_run_alone() {
    let release = Arc::new(Notify::new());
    let s = setup(vec![Script::SettleWhen(release.clone())]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());
    let operator = Caller::Operator {
        alias: alias_of("user"),
    };

    s.runtime.start().await.unwrap();
    eventually("run started", || s.pi.specs.lock().unwrap().len() == 1).await;
    s.runtime
        .abort_chain(&operator, CorrelationId::new())
        .await
        .unwrap();
    release.notify_one();
    eventually("message read", || s.inbox.is_read(&first)).await;
    assert!(
        !s.pi
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("abort"))
    );
}

#[tokio::test]
async fn start_revokes_old_tokens_and_wakes_agents_with_mail() {
    let s = setup(vec![Script::Settle]).await;
    let waiting = message("manager", None, CorrelationId::new());
    s.inbox.add(waiting.clone());

    s.runtime.start().await.unwrap();
    assert!(s.tokens.revoked_all.load(Ordering::SeqCst));
    eventually("message read", || s.inbox.is_read(&waiting)).await;
    let spec = s.pi.specs.lock().unwrap()[0].clone();
    assert_eq!(
        arg_after(&spec, "--session-id"),
        format!("chain-{}", waiting.correlation_id)
    );
}

#[tokio::test]
async fn relative_paths_are_refused() {
    let config = RuntimeConfig {
        data_dir: PathBuf::from("data"),
        extension: PathBuf::from("/ext/sona.ts"),
        api_url: String::new(),
        models_json: None,
        provider_env: Vec::new(),
        retry_delays: Vec::new(),
        limits: Limits::default(),
    };
    let result = Runtime::new(
        config,
        memory_pool().await,
        Arc::new(Directory(Arc::default())),
        Arc::new(Tokens::default()),
        Arc::new(Inbox::default()),
        Arc::new(Journal::default()),
        Arc::new(FakePi::default()),
    )
    .await;
    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn a_run_is_kept_with_what_it_spent() {
    let s = setup(vec![Script::Settle]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());

    s.runtime.start().await.unwrap();
    eventually("message read", || s.inbox.is_read(&first)).await;

    let runs = s
        .runtime
        .runs(&operator(), RunFilter::default())
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!((run.agent.as_str(), run.outcome), ("coder", RunOutcome::Ok));
    assert_eq!(run.card_version, 7);
    assert_eq!(run.correlation_id, first.correlation_id);
    assert!(run.finished_at.is_some());
    assert_eq!(
        run.usage,
        Usage {
            tokens_in: 130,
            tokens_out: 20,
            cost_micros: 1500
        }
    );
    assert_eq!(s.runtime.run(&operator(), run.run_id).await.unwrap(), *run);
}

#[tokio::test]
async fn failed_runs_are_kept_with_the_reason() {
    let s = setup(vec![Script::Crash, Script::Settle]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());

    s.runtime.start().await.unwrap();
    eventually("message read", || s.inbox.is_read(&first)).await;

    let runs = s
        .runtime
        .runs(&operator(), RunFilter::default())
        .await
        .unwrap();
    let outcomes: Vec<_> = runs.iter().map(|r| r.outcome).collect();
    // Newest first.
    assert_eq!(outcomes, [RunOutcome::Ok, RunOutcome::Error]);
    assert_eq!(
        runs[1].reason.as_deref(),
        Some("internal error: Pi exited before settling")
    );
}

#[tokio::test]
async fn runs_left_running_fail_on_the_next_start() {
    let pool = memory_pool().await;
    let release = Arc::new(Notify::new());
    let before = setup_on(pool.clone(), vec![Script::SettleWhen(release)]).await;
    before
        .inbox
        .add(message("coder", None, CorrelationId::new()));
    before.runtime.start().await.unwrap();
    eventually("run started", || before.pi.specs.lock().unwrap().len() == 1).await;

    // The control plane restarts while the Run is going.
    let after = setup_on(pool, vec![]).await;
    after.runtime.start().await.unwrap();
    let runs = after
        .runtime
        .runs(&operator(), RunFilter::default())
        .await
        .unwrap();
    assert_eq!(runs[0].outcome, RunOutcome::Error);
    assert_eq!(
        runs[0].reason.as_deref(),
        Some("the control plane restarted")
    );
}

#[tokio::test]
async fn runs_are_filtered_and_for_the_operator_only() {
    let s = setup(vec![Script::Settle, Script::Settle]).await;
    let coder = message("coder", None, CorrelationId::new());
    let manager = message("manager", None, CorrelationId::new());
    s.inbox.add(coder.clone());
    s.inbox.add(manager.clone());

    s.runtime.start().await.unwrap();
    eventually("both read", || {
        s.inbox.is_read(&coder) && s.inbox.is_read(&manager)
    })
    .await;

    let filter = RunFilter {
        agent: Some(alias_of("manager")),
        chain: None,
    };
    let runs = s.runtime.runs(&operator(), filter).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].correlation_id, manager.correlation_id);

    let agent = Caller::Agent {
        alias: alias_of("coder"),
        run_id: RunId::new(),
        correlation_id: coder.correlation_id,
    };
    let denied = s.runtime.runs(&agent, RunFilter::default()).await;
    assert!(matches!(denied, Err(Error::Forbidden(_))));
}

fn settles(runs: usize) -> Vec<Script> {
    (0..runs).map(|_| Script::Settle).collect()
}

/// Sends `message` and waits until its Run read it.
async fn deliver(s: &Setup, message: &Message) {
    s.inbox.add(message.clone());
    eventually("message read", || s.inbox.is_read(message)).await;
}

/// Sends `message` and waits until a safeguard halts its chain instead of a Run.
async fn deliver_halted(s: &Setup, message: &Message) -> String {
    s.inbox.add(message.clone());
    eventually("chain halted", || {
        s.inbox.halted(message.correlation_id).is_some()
    })
    .await;
    assert!(!s.inbox.is_read(message));
    let halted = s.journal.actions();
    assert!(halted.contains(&("chain_halted".to_owned(), None)));
    s.inbox.halted(message.correlation_id).unwrap()
}

#[tokio::test]
async fn a_chain_over_its_runs_is_halted_and_resume_counts_afresh() {
    let limits = Limits {
        runs_per_chain: 2,
        ..Limits::default()
    };
    let s = setup_with(memory_pool().await, settles(4), limits).await;
    s.runtime.start().await.unwrap();
    let chain = CorrelationId::new();
    deliver(&s, &message("coder", None, chain)).await;
    deliver(&s, &message("coder", None, chain)).await;

    let third = message("coder", None, chain);
    let reason = deliver_halted(&s, &third).await;
    assert_eq!(reason, "the chain reached its limit of 2 Runs");
    assert_eq!(s.pi.specs.lock().unwrap().len(), 2);

    // Resumed, the chain may take two Runs more.
    s.inbox.resume(chain, "coder");
    eventually("message read", || s.inbox.is_read(&third)).await;
    deliver(&s, &message("coder", None, chain)).await;
    assert_eq!(s.pi.specs.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn a_chain_over_its_cost_is_halted() {
    // One fake Run spends $0.0015.
    let limits = Limits {
        cost_per_chain_micros: 1500,
        ..Limits::default()
    };
    let s = setup_with(memory_pool().await, settles(2), limits).await;
    s.runtime.start().await.unwrap();
    let chain = CorrelationId::new();
    deliver(&s, &message("coder", None, chain)).await;

    let reason = deliver_halted(&s, &message("coder", None, chain)).await;
    assert_eq!(reason, "the chain reached its limit of $0.00 (spent $0.00)");
    // Other chains go on.
    deliver(&s, &message("coder", None, CorrelationId::new())).await;
}

#[tokio::test]
async fn an_agent_waking_too_often_halts_the_chain_that_wakes_it() {
    let limits = Limits {
        wakes_per_window: 2,
        ..Limits::default()
    };
    let s = setup_with(memory_pool().await, settles(3), limits).await;
    s.runtime.start().await.unwrap();
    deliver(&s, &message("coder", None, CorrelationId::new())).await;
    deliver(&s, &message("coder", None, CorrelationId::new())).await;

    let reason = deliver_halted(&s, &message("coder", None, CorrelationId::new())).await;
    assert_eq!(reason, "@coder reached its limit of 2 wakes in 10 minutes");
    // Other agents are counted on their own.
    deliver(&s, &message("manager", None, CorrelationId::new())).await;
}

#[tokio::test]
async fn disabling_an_agent_aborts_its_run_and_its_mail_waits() {
    let s = setup(vec![Script::UntilAbort, Script::Settle]).await;
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());
    s.runtime.start().await.unwrap();
    eventually("run started", || s.pi.specs.lock().unwrap().len() == 1).await;

    // Registry first, then Runtime, as the API does.
    s.inbox.disabled.lock().unwrap().insert(alias_of("coder"));
    s.runtime
        .abort_agent(&operator(), &alias_of("coder"), "the agent was disabled")
        .await
        .unwrap();
    eventually("run finished", || {
        s.journal.actions().iter().any(|(a, _)| a == "run_finished")
    })
    .await;
    let runs = s
        .runtime
        .runs(&operator(), RunFilter::default())
        .await
        .unwrap();
    assert_eq!(runs[0].outcome, RunOutcome::Stopped);
    assert_eq!(runs[0].reason.as_deref(), Some("the agent was disabled"));
    assert!(s.inbox.failures.lock().unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.pi.specs.lock().unwrap().len(), 1);
    assert!(!s.inbox.is_read(&first));

    // Enabled again, the agent gets what waited.
    s.inbox.disabled.lock().unwrap().clear();
    let _ = s.inbox.notifications.send(alias_of("coder"));
    eventually("message read", || s.inbox.is_read(&first)).await;
}

#[tokio::test]
async fn models_come_from_pi_once_and_for_the_operator_only() {
    let s = setup(vec![]).await;
    let models = s.runtime.models(&operator()).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        (models[0].provider.as_str(), models[0].id.as_str()),
        ("fake", "fake-1")
    );
    assert_eq!(models[0].price.output, 2.0);

    // Pi gets the provider keys, but nothing of a Run: no token, no agent's files.
    let spec = s.pi.specs.lock().unwrap()[0].clone();
    assert_eq!(env_of(&spec, "FAKE_KEY"), "k");
    assert!(
        spec.env
            .iter()
            .all(|(name, _)| name != "SONA_PLANE_RUN_TOKEN")
    );
    assert_eq!(spec.mounts.len(), 1);
    // Its scratch directory is gone with the answer.
    assert!(!spec.mounts[0].host.exists());

    // Kept: the second request starts no Pi.
    assert_eq!(s.runtime.models(&operator()).await.unwrap(), models);
    assert_eq!(s.pi.specs.lock().unwrap().len(), 1);

    let agent = Caller::Agent {
        alias: alias_of("coder"),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    };
    assert!(matches!(
        s.runtime.models(&agent).await,
        Err(Error::Forbidden(_))
    ));
}
