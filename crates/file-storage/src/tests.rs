use super::*;
use control_plane_common::registry::{Secret, Session};
use control_plane_common::tasks::{NewTask, Task, TaskStatus, TaskUpdate, TaskView};
use control_plane_common::{CorrelationId, RunId};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::sync::Mutex;

const LEAKED: &str = "sona_plane_rt_leaked";

/// Objects in memory, by key.
#[derive(Default)]
struct Memory(Mutex<HashMap<String, (Vec<u8>, String)>>);

#[async_trait]
impl ObjectStore for Memory {
    async fn put(&self, key: &str, content: &[u8], content_type: &str) -> Result<()> {
        let object = (content.to_vec(), content_type.to_owned());
        self.0.lock().unwrap().insert(key.to_owned(), object);
        Ok(())
    }
    async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let objects = self.0.lock().unwrap();
        let (content, _) = objects
            .get(key)
            .ok_or_else(|| Error::Internal(format!("no {key}")))?;
        Ok(content.clone())
    }
}

/// Only `LEAKED` counts as an active Run token.
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

/// One task, `TASK`, with @coder and @manager on it.
struct OneTask;

const TASK: &str = "01a22200-0000-7000-8000-000000000001";

#[async_trait]
impl TaskManager for OneTask {
    async fn create_task(&self, _: &Caller, _: NewTask) -> Result<Task> {
        unimplemented!()
    }
    async fn get_task(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        if id.to_string() != TASK {
            return Err(Error::NotFound(format!("task {id}")));
        }
        let on_it = ["coder", "manager"].contains(&caller.alias().as_str());
        if !on_it && !matches!(caller, Caller::Operator { .. }) {
            return Err(Error::Forbidden(format!("{} is not on it", caller.alias())));
        }
        Ok(TaskView {
            task: Task {
                id,
                title: "Calculator".to_owned(),
                context: String::new(),
                status: TaskStatus::InProgress,
                creator: alias("manager"),
                created_at: Timestamp::now(),
                parent_id: None,
                assignee: Some(alias("coder")),
                members: Vec::new(),
                deadline: None,
                result: None,
            },
            history: Vec::new(),
        })
    }
    async fn update_task(&self, _: &Caller, _: TaskId, _: TaskUpdate) -> Result<Task> {
        unimplemented!()
    }
    async fn list_tasks(&self, _: &Caller, _: Option<TaskStatus>) -> Result<Vec<Task>> {
        unimplemented!()
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

struct Setup {
    files: SqliteFileStorage,
    objects: Arc<Memory>,
    journal: Arc<Recorded>,
}

async fn setup() -> Setup {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let objects = Arc::new(Memory::default());
    let journal = Arc::new(Recorded::default());
    let files = SqliteFileStorage::new(
        pool,
        objects.clone(),
        Arc::new(OneActiveToken),
        Arc::new(OneTask),
        journal.clone(),
    )
    .await
    .unwrap();
    Setup {
        files,
        objects,
        journal,
    }
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

#[tokio::test]
async fn a_file_is_kept_under_its_owner_and_comes_back_whole() {
    let s = setup().await;
    let content = b"<h1>calculator</h1>".to_vec();
    let file = s
        .files
        .upload(
            &agent("coder"),
            "calculator/index.html",
            "text/html; charset=utf-8",
            content.clone(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (
            file.name.as_str(),
            file.size,
            file.content_type.as_str(),
            file.owner.as_str()
        ),
        ("index.html", content.len() as u64, "text/html", "coder")
    );
    // The key is the owner's space and the id: the name never reaches it.
    let keys: Vec<String> = s.objects.0.lock().unwrap().keys().cloned().collect();
    assert_eq!(keys, [format!("coder/{}", file.id)]);

    let (info, back) = s.files.download(&agent("coder"), file.id).await.unwrap();
    assert_eq!((info, back), (file, content));
}

#[tokio::test]
async fn others_see_a_file_only_once_it_is_shared_with_them() {
    let s = setup().await;
    let file = s
        .files
        .upload(
            &agent("coder"),
            "notes.md",
            "text/markdown",
            b"hi".to_vec(),
            None,
        )
        .await
        .unwrap();
    let manager = agent("manager");
    assert!(matches!(
        s.files.info(&manager, file.id).await,
        Err(Error::NotFound(_))
    ));
    assert!(s.files.list(&manager, None).await.unwrap().is_empty());
    // The operator reads everything.
    assert_eq!(s.files.info(&operator(), file.id).await.unwrap(), file);

    // Sharing needs the right to read: the manager cannot share it onward yet.
    assert!(matches!(
        s.files.share(&manager, &[file.id], &alias("tester")).await,
        Err(Error::NotFound(_))
    ));
    let shared = s
        .files
        .share(&agent("coder"), &[file.id], &alias("manager"))
        .await
        .unwrap();
    assert_eq!(shared, std::slice::from_ref(&file));
    assert_eq!(s.files.download(&manager, file.id).await.unwrap().1, b"hi");
    assert_eq!(
        s.files.list(&manager, None).await.unwrap(),
        std::slice::from_ref(&file)
    );
    // Sharing twice is fine.
    s.files
        .share(&agent("coder"), &[file.id], &alias("manager"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_file_with_an_active_run_token_is_refused_and_journaled() {
    let s = setup().await;
    let content = format!("export TOKEN={LEAKED}").into_bytes();
    assert!(matches!(
        s.files
            .upload(&agent("coder"), "env.sh", "text/plain", content, None)
            .await,
        Err(Error::Forbidden(_))
    ));
    assert!(s.objects.0.lock().unwrap().is_empty());
    let journal = s.journal.0.lock().unwrap();
    assert_eq!(journal[0].decision, Some(Decision::Deny));
    assert_eq!(journal[0].component, Component::FileStorage);
}

#[tokio::test]
async fn names_and_types_are_cleaned_and_size_is_capped() {
    let s = setup().await;
    let coder = agent("coder");
    let file = s
        .files
        .upload(
            &coder,
            "..\\..\\evil\u{7}.sh",
            "not a type",
            b"x".to_vec(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (file.name.as_str(), file.content_type.as_str()),
        ("evil.sh", "application/octet-stream")
    );
    for bad in ["", "dir/", "..", "a/.."] {
        assert!(
            matches!(
                s.files
                    .upload(&coder, bad, "text/plain", b"x".to_vec(), None)
                    .await,
                Err(Error::InvalidInput(_))
            ),
            "{bad:?}"
        );
    }
    let huge = vec![0; MAX_FILE_BYTES + 1];
    assert!(matches!(
        s.files
            .upload(&coder, "big.bin", "application/octet-stream", huge, None)
            .await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn without_storage_configured_every_call_says_so() {
    let files = NoFileStorage;
    assert!(matches!(
        files
            .upload(&operator(), "a", "text/plain", Vec::new(), None)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(files.list(&operator(), None).await.unwrap().is_empty());
    // A message without attachments needs no storage.
    assert!(
        files
            .share(&operator(), &[], &alias("coder"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_tasks_files_are_shared_by_whoever_is_on_it() {
    let s = setup().await;
    let task: TaskId = TASK.parse().unwrap();
    let result = s
        .files
        .upload(
            &agent("coder"),
            "game.html",
            "text/html",
            b"<canvas>".to_vec(),
            Some(task),
        )
        .await
        .unwrap();
    assert_eq!(result.task_id, Some(task));
    let keys: Vec<String> = s.objects.0.lock().unwrap().keys().cloned().collect();
    assert_eq!(keys, [format!("tasks/{task}/{}", result.id)]);

    // The manager is on the task: reads it and sees it in the task's files.
    assert_eq!(
        s.files
            .download(&agent("manager"), result.id)
            .await
            .unwrap()
            .1,
        b"<canvas>"
    );
    assert_eq!(
        s.files.list(&agent("manager"), Some(task)).await.unwrap(),
        [result.clone()]
    );
    // Someone off the task neither reads it nor lists the task, nor uploads into it.
    let tester = agent("tester");
    assert!(matches!(
        s.files.download(&tester, result.id).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        s.files.list(&tester, Some(task)).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        s.files
            .upload(&tester, "x.txt", "text/plain", b"x".to_vec(), Some(task))
            .await,
        Err(Error::Forbidden(_))
    ));
}
