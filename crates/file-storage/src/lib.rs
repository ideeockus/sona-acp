//! File Storage: who may read which file, kept in SQLite; the files themselves in an
//! S3-compatible store, RustFS (D5, D40, D106).
//!
//! A file belongs to whoever uploaded it. It is read by its owner, by those it was
//! shared with through a message, and by the operator (D56).

mod s3;

pub use s3::{S3Config, S3Store};

use async_trait::async_trait;
use control_plane_common::files::{FileInfo, FileStorage, MAX_FILE_BYTES};
use control_plane_common::journal::{Component, Decision, Event, JournalWriter};
use control_plane_common::registry::Credentials;
use control_plane_common::tasks::TaskManager;
use control_plane_common::{Alias, Caller, Error, FileId, Result, TaskId, Timestamp};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::sync::Arc;

/// Files one listing returns.
const LIST_LIMIT: i64 = 200;

const COLUMNS: &str = "id, name, size, content_type, owner, task_id, created_at";

/// Where the content of files goes.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &str, content: &[u8], content_type: &str) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Vec<u8>>;
}

pub struct SqliteFileStorage {
    pool: SqlitePool,
    objects: Arc<dyn ObjectStore>,
    credentials: Arc<dyn Credentials>,
    /// Who takes part in a task: they share its space (D111).
    tasks: Arc<dyn TaskManager>,
    journal: Arc<dyn JournalWriter>,
}

impl SqliteFileStorage {
    pub async fn new(
        pool: SqlitePool,
        objects: Arc<dyn ObjectStore>,
        credentials: Arc<dyn Credentials>,
        tasks: Arc<dyn TaskManager>,
        journal: Arc<dyn JournalWriter>,
    ) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self {
            pool,
            objects,
            credentials,
            tasks,
            journal,
        })
    }

    /// The file, if the caller may read it. A file they may not read does not exist
    /// for them: knowing an id gives nothing.
    async fn readable(&self, caller: &Caller, file: FileId) -> Result<FileInfo> {
        let missing = || Error::NotFound(format!("file {file}"));
        let row: Option<Row> = sqlx::query_as(&format!("SELECT {COLUMNS} FROM files WHERE id = ?"))
            .bind(file.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?;
        let info = row.ok_or_else(missing)?.into_info()?;
        if matches!(caller, Caller::Operator { .. }) || &info.owner == caller.alias() {
            return Ok(info);
        }
        let shared: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM shares WHERE file = ? AND reader = ?")
                .bind(file.to_string())
                .bind(caller.alias().as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        if shared.is_some() {
            return Ok(info);
        }
        // Task Manager knows who takes part now: a member added later reads the files,
        // one removed no longer does.
        match info.task_id {
            Some(task) if self.takes_part(caller, task).await? => Ok(info),
            _ => Err(missing()),
        }
    }

    async fn takes_part(&self, caller: &Caller, task: TaskId) -> Result<bool> {
        match self.tasks.get_task(caller, task).await {
            Ok(_) => Ok(true),
            Err(Error::Forbidden(_) | Error::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn deny(&self, caller: &Caller, reason: &str) -> Result<()> {
        self.journal
            .record(Event {
                decision: Some(Decision::Deny),
                reason: Some(reason.to_owned()),
                ..Event::new(caller, Component::FileStorage, "file_upload")
            })
            .await
    }
}

#[async_trait]
impl FileStorage for SqliteFileStorage {
    async fn upload(
        &self,
        caller: &Caller,
        name: &str,
        content_type: &str,
        content: Vec<u8>,
        task: Option<TaskId>,
    ) -> Result<FileInfo> {
        let name = file_name(name)?;
        if let Some(task) = task
            && !self.takes_part(caller, task).await?
        {
            return Err(Error::Forbidden(format!(
                "{} does not take part in task {task}",
                caller.alias()
            )));
        }
        if content.len() > MAX_FILE_BYTES {
            return Err(Error::InvalidInput(format!(
                "a file may weigh at most {} MiB",
                MAX_FILE_BYTES / 1024 / 1024
            )));
        }
        // A token is text; in a binary file it would still show as its bytes.
        let text = String::from_utf8_lossy(&content);
        for checked in [name.as_str(), &text] {
            if self.credentials.contains_active_run_token(checked).await? {
                let reason = "file contains an active Run token";
                self.deny(caller, reason).await?;
                return Err(Error::Forbidden(reason.to_owned()));
            }
        }

        let info = FileInfo {
            id: FileId::new(),
            name,
            size: content.len() as u64,
            content_type: media_type(content_type),
            owner: caller.alias().clone(),
            task_id: task,
            created_at: Timestamp::now(),
        };
        // Content first: a row without its object would be a file nobody can download.
        self.objects
            .put(&key(&info), &content, &info.content_type)
            .await?;
        sqlx::query(
            "INSERT INTO files (id, name, size, content_type, owner, task_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(info.id.to_string())
        .bind(&info.name)
        .bind(i64::try_from(info.size).map_err(|e| Error::Internal(e.to_string()))?)
        .bind(&info.content_type)
        .bind(info.owner.as_str())
        .bind(info.task_id.map(|id| id.to_string()))
        .bind(info.created_at.as_unix_millis())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(info)
    }

    async fn info(&self, caller: &Caller, file: FileId) -> Result<FileInfo> {
        self.readable(caller, file).await
    }

    async fn download(&self, caller: &Caller, file: FileId) -> Result<(FileInfo, Vec<u8>)> {
        let info = self.readable(caller, file).await?;
        let content = self.objects.get(&key(&info)).await?;
        Ok((info, content))
    }

    async fn list(&self, caller: &Caller, task: Option<TaskId>) -> Result<Vec<FileInfo>> {
        let rows: Vec<Row> = match task {
            Some(task) => {
                if !self.takes_part(caller, task).await? {
                    return Err(Error::NotFound(format!("task {task}")));
                }
                sqlx::query_as(&format!(
                    "SELECT {COLUMNS} FROM files WHERE task_id = ?
                     ORDER BY created_at DESC, id DESC LIMIT ?"
                ))
                .bind(task.to_string())
                .bind(LIST_LIMIT)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as(&format!(
                    "SELECT {COLUMNS} FROM files
                     WHERE ?2 OR owner = ?1 OR id IN (SELECT file FROM shares WHERE reader = ?1)
                     ORDER BY created_at DESC, id DESC LIMIT ?3"
                ))
                .bind(caller.alias().as_str())
                .bind(matches!(caller, Caller::Operator { .. }))
                .bind(LIST_LIMIT)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(internal)?;
        rows.into_iter().map(Row::into_info).collect()
    }

    async fn share(
        &self,
        caller: &Caller,
        files: &[FileId],
        with: &Alias,
    ) -> Result<Vec<FileInfo>> {
        let mut shared = Vec::with_capacity(files.len());
        for &file in files {
            shared.push(self.readable(caller, file).await?);
        }
        for file in &shared {
            sqlx::query("INSERT INTO shares (file, reader) VALUES (?, ?) ON CONFLICT DO NOTHING")
                .bind(file.id.to_string())
                .bind(with.as_str())
                .execute(&self.pool)
                .await
                .map_err(internal)?;
        }
        Ok(shared)
    }
}

/// Without File Storage configured, every call says so.
pub struct NoFileStorage;

#[async_trait]
impl FileStorage for NoFileStorage {
    async fn upload(
        &self,
        _: &Caller,
        _: &str,
        _: &str,
        _: Vec<u8>,
        _: Option<TaskId>,
    ) -> Result<FileInfo> {
        Err(not_configured())
    }
    async fn info(&self, _: &Caller, _: FileId) -> Result<FileInfo> {
        Err(not_configured())
    }
    async fn download(&self, _: &Caller, _: FileId) -> Result<(FileInfo, Vec<u8>)> {
        Err(not_configured())
    }
    async fn list(&self, _: &Caller, _: Option<TaskId>) -> Result<Vec<FileInfo>> {
        Ok(Vec::new())
    }
    async fn share(&self, _: &Caller, files: &[FileId], _: &Alias) -> Result<Vec<FileInfo>> {
        if files.is_empty() {
            Ok(Vec::new())
        } else {
            Err(not_configured())
        }
    }
}

fn not_configured() -> Error {
    Error::InvalidInput("File Storage is not configured: see [storage] in the config".to_owned())
}

/// The object's key: the owner's or the task's space and the id, never the name.
fn key(file: &FileInfo) -> String {
    match file.task_id {
        Some(task) => format!("tasks/{task}/{}", file.id),
        None => format!("{}/{}", file.owner.as_str(), file.id),
    }
}

/// The last part of whatever path came as a name, without control characters.
fn file_name(name: &str) -> Result<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() || clean == "." || clean == ".." {
        return Err(Error::InvalidInput(format!("{name:?} is not a file name")));
    }
    if clean.chars().count() > 255 {
        return Err(Error::InvalidInput(
            "a file name is at most 255 characters".to_owned(),
        ));
    }
    Ok(clean.to_owned())
}

/// `type/subtype` as given, or bytes when it does not look like one.
fn media_type(given: &str) -> String {
    let essence = given
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let valid = essence.split_once('/').is_some_and(|(kind, sub)| {
        let token = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "!#$&^_.+-".contains(c))
        };
        token(kind) && token(sub)
    });
    if valid {
        essence
    } else {
        "application/octet-stream".to_owned()
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    name: String,
    size: i64,
    content_type: String,
    owner: String,
    task_id: Option<String>,
    created_at: i64,
}

impl Row {
    fn into_info(self) -> Result<FileInfo> {
        Ok(FileInfo {
            id: parse(&self.id)?,
            name: self.name,
            size: u64::try_from(self.size).map_err(|e| Error::Internal(e.to_string()))?,
            content_type: self.content_type,
            owner: parse(&self.owner)?,
            task_id: self.task_id.as_deref().map(parse).transpose()?,
            created_at: Timestamp::from_unix_millis(self.created_at)?,
        })
    }
}

fn internal(error: sqlx::Error) -> Error {
    Error::Internal(error.to_string())
}

fn parse<T: FromStr>(value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|e: T::Err| Error::Internal(e.to_string()))
}

#[cfg(test)]
mod tests;
