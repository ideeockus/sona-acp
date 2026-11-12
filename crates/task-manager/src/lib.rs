//! Task Manager on SQLite: tasks, their status, result and history.

use async_trait::async_trait;
use control_plane_common::journal::{Component, Decision, Event, JournalWriter};
use control_plane_common::registry::{AgentDirectory, Credentials};
use control_plane_common::tasks::{
    Change, HistoryEntry, NewTask, Task, TaskManager, TaskStatus, TaskUpdate, TaskView,
};
use control_plane_common::{Alias, Caller, Error, Result, TaskId, Timestamp};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::{SqliteExecutor, SqlitePool};
use std::str::FromStr;
use std::sync::Arc;

pub struct SqliteTaskManager {
    pool: SqlitePool,
    directory: Arc<dyn AgentDirectory>,
    credentials: Arc<dyn Credentials>,
    journal: Arc<dyn JournalWriter>,
}

impl SqliteTaskManager {
    /// Wraps a pool and brings the schema up to date.
    pub async fn new(
        pool: SqlitePool,
        directory: Arc<dyn AgentDirectory>,
        credentials: Arc<dyn Credentials>,
        journal: Arc<dyn JournalWriter>,
    ) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self {
            pool,
            directory,
            credentials,
            journal,
        })
    }

    async fn task(&self, id: TaskId) -> Result<Task> {
        let row: Option<Row> = sqlx::query_as(
            "SELECT id, title, context, status, creator, created_at, parent_id,
                    assignee, members, deadline, result
             FROM tasks WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.ok_or_else(|| Error::NotFound(format!("task {id}")))?
            .into_task()
    }

    /// The task, if `caller` may see it (D72).
    async fn visible_task(&self, caller: &Caller, id: TaskId) -> Result<Task> {
        let task = self.task(id).await?;
        match caller {
            Caller::Operator { .. } => Ok(task),
            Caller::Agent { alias, .. } if task.is_participant(alias) => Ok(task),
            // The same answer as for a missing task: others do not learn it exists.
            _ => Err(Error::NotFound(format!("task {id}"))),
        }
    }

    /// A Run token must not leave its agent (D24). A refusal is recorded in Agent Journal.
    async fn reject_tokens(
        &self,
        caller: &Caller,
        action: &str,
        task_id: Option<TaskId>,
        texts: &[&str],
    ) -> Result<()> {
        for text in texts {
            if self.credentials.contains_active_run_token(text).await? {
                let reason = "text contains an active Run token";
                self.journal
                    .record(Event {
                        decision: Some(Decision::Deny),
                        reason: Some(reason.to_owned()),
                        task_id,
                        ..Event::new(caller, Component::TaskManager, action)
                    })
                    .await?;
                return Err(Error::Forbidden(reason.to_owned()));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TaskManager for SqliteTaskManager {
    async fn create_task(&self, caller: &Caller, new: NewTask) -> Result<Task> {
        if let Caller::Adapter { alias } = caller {
            return Err(Error::Forbidden(format!(
                "adapter {alias} is not supported yet"
            )));
        }
        self.reject_tokens(caller, "create_task", None, &[&new.title, &new.context])
            .await?;
        if let Some(parent) = new.parent_id {
            self.visible_task(caller, parent).await?;
        }
        for agent in new.assignee.iter().chain(&new.members) {
            self.directory.agent(agent).await?;
        }

        let task = Task {
            id: TaskId::new(),
            title: new.title,
            context: new.context,
            status: TaskStatus::Open,
            creator: caller.alias().clone(),
            created_at: Timestamp::now(),
            parent_id: new.parent_id,
            assignee: new.assignee,
            members: new.members,
            deadline: new.deadline,
            result: None,
        };

        let mut tx = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "INSERT INTO tasks (id, title, context, status, creator, created_at, parent_id,
                                assignee, members, deadline, result)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(task.id.to_string())
        .bind(&task.title)
        .bind(&task.context)
        .bind(to_json_str(&task.status)?)
        .bind(task.creator.as_str())
        .bind(task.created_at.as_unix_millis())
        .bind(task.parent_id.map(|id| id.to_string()))
        .bind(task.assignee.as_ref().map(Alias::as_str))
        .bind(to_json(&task.members)?)
        .bind(task.deadline.map(Timestamp::as_unix_millis))
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
        record(&mut *tx, task.id, &task.creator, &Change::Created).await?;
        tx.commit().await.map_err(internal)?;
        Ok(task)
    }

    async fn get_task(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.visible_task(caller, id).await?;
        let rows: Vec<(i64, String, String)> =
            sqlx::query_as("SELECT at, who, change FROM history WHERE task_id = ? ORDER BY seq")
                .bind(id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(internal)?;
        let history = rows
            .into_iter()
            .map(|(at, who, change)| {
                Ok(HistoryEntry {
                    at: Timestamp::from_unix_millis(at)?,
                    who: parse(&who)?,
                    change: from_json(&change)?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(TaskView { task, history })
    }

    async fn update_task(&self, caller: &Caller, id: TaskId, update: TaskUpdate) -> Result<Task> {
        let mut task = self.visible_task(caller, id).await?;
        if task.status.is_closed() {
            return Err(Error::InvalidInput(format!("task {id} is closed")));
        }
        let mut changes = Vec::new();

        if let Some(to) = update.status {
            if to == task.status {
                return Err(Error::InvalidInput(format!("task {id} is already {to:?}")));
            }
            let may_close =
                matches!(caller, Caller::Operator { .. }) || caller.alias() == &task.creator;
            if to.is_closed() && !may_close {
                return Err(Error::Forbidden(
                    "only the creator or the operator closes a task".to_owned(),
                ));
            }
            changes.push(Change::Status {
                from: task.status,
                to,
            });
        }
        if let Some(result) = update.result {
            let mut texts = vec![result.text.as_str()];
            texts.extend(result.links.iter().map(String::as_str));
            self.reject_tokens(caller, "update_task", Some(id), &texts)
                .await?;
            changes.push(Change::Result { result });
        }
        if let Some(text) = update.note {
            self.reject_tokens(caller, "update_task", Some(id), &[&text])
                .await?;
            changes.push(Change::Note { text });
        }
        if changes.is_empty() {
            return Err(Error::InvalidInput("empty update".to_owned()));
        }

        let previous = task.status;
        for change in &changes {
            match change {
                Change::Status { to, .. } => task.status = *to,
                Change::Result { result } => task.result = Some(result.clone()),
                Change::Created | Change::Note { .. } => {}
            }
        }

        let mut tx = self.pool.begin().await.map_err(internal)?;
        // Guarded by the status read above, so a concurrent close is not overwritten.
        let updated =
            sqlx::query("UPDATE tasks SET status = ?, result = ? WHERE id = ? AND status = ?")
                .bind(to_json_str(&task.status)?)
                .bind(task.result.as_ref().map(to_json).transpose()?)
                .bind(id.to_string())
                .bind(to_json_str(&previous)?)
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
        if updated.rows_affected() == 0 {
            return Err(Error::InvalidInput(format!(
                "task {id} changed concurrently, retry"
            )));
        }
        for change in &changes {
            record(&mut *tx, id, caller.alias(), change).await?;
        }
        tx.commit().await.map_err(internal)?;
        Ok(task)
    }
}

async fn record(
    db: impl SqliteExecutor<'_>,
    id: TaskId,
    who: &Alias,
    change: &Change,
) -> Result<()> {
    sqlx::query("INSERT INTO history (task_id, at, who, change) VALUES (?, ?, ?, ?)")
        .bind(id.to_string())
        .bind(Timestamp::now().as_unix_millis())
        .bind(who.as_str())
        .bind(to_json(change)?)
        .execute(db)
        .await
        .map_err(internal)?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    title: String,
    context: String,
    status: String,
    creator: String,
    created_at: i64,
    parent_id: Option<String>,
    assignee: Option<String>,
    members: String,
    deadline: Option<i64>,
    result: Option<String>,
}

impl Row {
    fn into_task(self) -> Result<Task> {
        Ok(Task {
            id: parse(&self.id)?,
            title: self.title,
            context: self.context,
            status: from_json(&format!("\"{}\"", self.status))?,
            creator: parse(&self.creator)?,
            created_at: Timestamp::from_unix_millis(self.created_at)?,
            parent_id: self.parent_id.as_deref().map(parse).transpose()?,
            assignee: self.assignee.as_deref().map(parse).transpose()?,
            members: from_json(&self.members)?,
            deadline: self.deadline.map(Timestamp::from_unix_millis).transpose()?,
            result: self.result.as_deref().map(from_json).transpose()?,
        })
    }
}

fn to_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|e| Error::Internal(e.to_string()))
}

/// A unit enum variant as its bare name, e.g. `in_progress`.
fn to_json_str(value: &impl Serialize) -> Result<String> {
    Ok(to_json(value)?.trim_matches('"').to_owned())
}

fn from_json<T: DeserializeOwned>(json: &str) -> Result<T> {
    serde_json::from_str(json).map_err(|e| Error::Internal(e.to_string()))
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
