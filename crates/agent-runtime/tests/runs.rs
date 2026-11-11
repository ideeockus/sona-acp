//! The Run loop against in-memory stand-ins for Registry, Broker, Journal and Pi.

use async_trait::async_trait;
use control_plane_agent_runtime::launcher::{Launched, Launcher, Process, RunSpec};
use control_plane_agent_runtime::{Runtime, RuntimeConfig};
use control_plane_common::broker::{InboxBatch, InboxDelivery, Message, WakeSink};
use control_plane_common::journal::{Event, JournalWriter, Outcome};
use control_plane_common::registry::{AgentCard, AgentDirectory, Credentials, ModelRef, Secret};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, RunId, SessionKey, TaskId, Timestamp,
};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, oneshot};

// ---- Registry ----

struct Directory;

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
            allow_tools: vec!["read".to_owned(), "send_message".to_owned()],
            allow_message_to,
            prompt: format!("You are {alias}."),
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
}

// ---- Broker ----

/// Messages in delivery order and the ids already read.
#[derive(Default)]
struct Inbox {
    messages: Mutex<Vec<Message>>,
    read: Mutex<HashSet<MessageId>>,
}

impl Inbox {
    fn add(&self, message: Message) {
        self.messages.lock().unwrap().push(message);
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
        let unread = self.unread(agent);
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
        Ok(())
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
        self.specs.lock().unwrap().push(spec);
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
            // Reads commands until Runtime closes stdin.
            let reader = tokio::spawn(async move {
                let mut lines = BufReader::new(pi_stdin).lines();
                let mut first_line = Some(first_line);
                while let Ok(Some(line)) = lines.next_line().await {
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
            let tool = br#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"read","result":{},"isError":false}"#;
            pi_stdout.write_all(tool).await.unwrap();
            pi_stdout.write_all(b"\n").await.unwrap();
            if let Script::SettleWhen(release) = script {
                release.notified().await;
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

// ---- Helpers ----

struct Setup {
    runtime: Runtime,
    data_dir: PathBuf,
    tokens: Arc<Tokens>,
    inbox: Arc<Inbox>,
    journal: Arc<Journal>,
    pi: Arc<FakePi>,
}

fn setup(scripts: Vec<Script>) -> Setup {
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
    };
    let runtime = Runtime::new(
        config,
        Arc::new(Directory),
        tokens.clone(),
        inbox.clone(),
        journal.clone(),
        pi.clone(),
    )
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
    let s = setup(vec![Script::Settle]);
    let task = TaskId::new();
    let chain = CorrelationId::new();
    let (first, second) = (
        message("coder", Some(task), chain),
        message("coder", Some(task), chain),
    );
    s.inbox.add(first.clone());
    s.inbox.add(second.clone());

    s.runtime.wake(&alias_of("coder")).await;
    eventually("both messages read", || {
        s.inbox.is_read(&first) && s.inbox.is_read(&second)
    })
    .await;
    eventually("run finished", || s.journal.actions().len() == 3).await;

    let spec = s.pi.specs.lock().unwrap()[0].clone();
    assert_eq!(arg_after(&spec, "--session-id"), format!("task-{task}"));
    assert_eq!(arg_after(&spec, "--model"), "fake/fake-1");
    assert_eq!(arg_after(&spec, "--tools"), "read,send_message");
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
    let s = setup(vec![Script::SettleWhen(release.clone()), Script::Settle]);
    let (task, other) = (TaskId::new(), TaskId::new());
    let chain = CorrelationId::new();
    let first = message("coder", Some(task), chain);
    s.inbox.add(first.clone());

    s.runtime.wake(&alias_of("coder")).await;
    eventually("prompt", || s.pi.received.lock().unwrap().len() == 1).await;

    let follow_up = message("coder", Some(task), chain);
    let elsewhere = message("coder", Some(other), CorrelationId::new());
    s.inbox.add(follow_up.clone());
    s.inbox.add(elsewhere.clone());
    s.runtime.wake(&alias_of("coder")).await;
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
async fn a_crashed_run_leaves_messages_unread() {
    let s = setup(vec![Script::Crash]);
    let first = message("coder", None, CorrelationId::new());
    s.inbox.add(first.clone());

    s.runtime.wake(&alias_of("coder")).await;
    eventually("run finished", || s.journal.actions().len() == 2).await;

    assert!(!s.inbox.is_read(&first));
    assert_eq!(
        s.journal.actions()[1],
        ("run_finished".to_owned(), Some(Outcome::Error))
    );
    assert_eq!(
        *s.tokens.issued.lock().unwrap(),
        *s.tokens.revoked.lock().unwrap()
    );
    // No retry loop: the agent waits for its next wake (D77 retries come later).
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.pi.specs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn start_revokes_old_tokens_and_wakes_agents_with_mail() {
    let s = setup(vec![Script::Settle]);
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
    };
    let result = Runtime::new(
        config,
        Arc::new(Directory),
        Arc::new(Tokens::default()),
        Arc::new(Inbox::default()),
        Arc::new(Journal::default()),
        Arc::new(FakePi::default()),
    );
    assert!(matches!(result, Err(Error::InvalidInput(_))));
}
