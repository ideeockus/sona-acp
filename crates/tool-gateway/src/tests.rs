use super::*;
use control_plane_common::registry::{AgentCard, ModelRef};
use control_plane_common::{Alias, CorrelationId, RunId};
use std::sync::Mutex;

/// `@coder` with a fixed set of tools.
struct Directory;

#[async_trait]
impl AgentDirectory for Directory {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        if alias.as_str() != "coder" {
            return Err(Error::NotFound(format!("agent {alias}")));
        }
        Ok(AgentCard {
            alias: alias.clone(),
            title: String::new(),
            model: ModelRef {
                provider: "fake".to_owned(),
                id: "fake".to_owned(),
            },
            allow_tools: [
                "read",
                "write",
                "edit",
                "grep",
                "ls",
                "send_message",
                "mcp__gitlab__get_issue",
            ]
            .map(String::from)
            .to_vec(),
            allow_message_to: Vec::new(),
            prompt: String::new(),
        })
    }
}

#[derive(Default)]
struct Recorded(Mutex<Vec<Event>>);

#[async_trait]
impl JournalWriter for Recorded {
    async fn record(&self, event: Event) -> Result<()> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

/// Fails every write, like a full disk.
struct BrokenJournal;

#[async_trait]
impl JournalWriter for BrokenJournal {
    async fn record(&self, _: Event) -> Result<()> {
        Err(Error::Internal("disk full".to_owned()))
    }
}

fn gateway() -> (Gateway, Arc<Recorded>) {
    let journal = Arc::new(Recorded::default());
    (Gateway::new(Arc::new(Directory), journal.clone()), journal)
}

fn coder() -> Caller {
    Caller::Agent {
        alias: "coder".parse().unwrap(),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    }
}

fn call(tool: &str, path: Option<&str>) -> ToolCall {
    ToolCall {
        tool: tool.to_owned(),
        path: path.map(str::to_owned),
    }
}

async fn verdict(tool: &str, path: Option<&str>) -> Verdict {
    gateway().0.check(&coder(), call(tool, path)).await.unwrap()
}

fn is_deny(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Deny { .. })
}

#[tokio::test]
async fn granted_tools_are_allowed() {
    assert_eq!(verdict("send_message", None).await, Verdict::Allow);
    assert_eq!(
        verdict("mcp__gitlab__get_issue", None).await,
        Verdict::Allow
    );
}

#[tokio::test]
async fn tools_outside_the_grant_are_denied() {
    assert_eq!(
        verdict("bash", None).await,
        Verdict::Deny {
            reason: "bash is not granted".to_owned()
        }
    );
}

#[tokio::test]
async fn writes_stay_in_the_workdir() {
    assert_eq!(
        verdict("write", Some("/work/notes.md")).await,
        Verdict::Allow
    );
    assert_eq!(
        verdict("edit", Some("/work/src/main.rs")).await,
        Verdict::Allow
    );
    assert!(is_deny(
        &verdict("write", Some("/agent/skills/x/SKILL.md")).await
    ));
    assert!(is_deny(&verdict("edit", Some("/etc/hosts")).await));
}

#[tokio::test]
async fn reads_may_also_reach_skills() {
    assert_eq!(
        verdict("read", Some("/work/notes.md")).await,
        Verdict::Allow
    );
    assert_eq!(
        verdict("read", Some("/agent/skills/review/SKILL.md")).await,
        Verdict::Allow
    );
    assert_eq!(verdict("ls", Some("/work")).await, Verdict::Allow);
    // Where a symlink in a cloned repo would lead, once the extension resolves it.
    assert!(is_deny(&verdict("read", Some("/proc/self/environ")).await));
    assert!(is_deny(&verdict("grep", Some("/agent")).await));
}

#[tokio::test]
async fn similar_prefixes_do_not_count() {
    assert!(is_deny(&verdict("read", Some("/workshop/secret")).await));
    assert!(is_deny(
        &verdict("read", Some("/agent/skills-other/x")).await
    ));
}

#[tokio::test]
async fn unresolved_paths_are_refused() {
    for path in ["/work/../etc/passwd", "../work/notes.md", "notes.md", ""] {
        assert!(
            is_deny(&verdict("read", Some(path)).await),
            "allowed {path:?}"
        );
    }
    // A `.` inside a path is dropped by `Path::components` and changes nothing.
    assert_eq!(
        verdict("read", Some("/work/./notes.md")).await,
        Verdict::Allow
    );
}

#[tokio::test]
async fn file_tools_need_a_path() {
    assert!(is_deny(&verdict("read", None).await));
}

#[tokio::test]
async fn every_check_is_recorded() {
    let (gateway, journal) = gateway();
    let caller = coder();
    gateway
        .check(&caller, call("read", Some("/work/a")))
        .await
        .unwrap();
    gateway.check(&caller, call("bash", None)).await.unwrap();

    let events = journal.0.lock().unwrap();
    let recorded: Vec<_> = events
        .iter()
        .map(|e| (e.action.as_str(), e.decision, e.component))
        .collect();
    assert_eq!(
        recorded,
        [
            ("read", Some(Decision::Allow), Component::Gateway),
            ("bash", Some(Decision::Deny), Component::Gateway),
        ]
    );
    assert_eq!(events[1].reason.as_deref(), Some("bash is not granted"));
    let Caller::Agent { run_id, .. } = caller else {
        unreachable!()
    };
    assert_eq!(events[0].run_id, Some(run_id));
}

#[tokio::test]
async fn journal_failure_fails_the_check() {
    let gateway = Gateway::new(Arc::new(Directory), Arc::new(BrokenJournal));
    assert!(
        gateway
            .check(&coder(), call("read", Some("/work/a")))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn only_agents_are_checked() {
    let (gateway, _) = gateway();
    let operator = Caller::Operator {
        alias: "user".parse().unwrap(),
    };
    assert!(matches!(
        gateway
            .check(&operator, call("read", Some("/work/a")))
            .await,
        Err(Error::Forbidden(_))
    ));
}
