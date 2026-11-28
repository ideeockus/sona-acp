//! Agent Memory on SQLite: records each agent chose to keep, searched by substring
//! through FTS5's trigram tokenizer (D16, D31, D108).

use async_trait::async_trait;
use control_plane_common::memory::{
    AgentMemory, MAX_RECORD_BYTES, MAX_RECORDS, MAX_TOTAL_BYTES, MemoryRecord,
};
use control_plane_common::registry::Credentials;
use control_plane_common::{Alias, Caller, Error, MemoryId, Result, Timestamp};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::sync::Arc;

/// Records one search returns.
const SEARCH_LIMIT: i64 = 20;

const COLUMNS: &str = "r.id, r.agent, r.text, r.run_id, r.correlation_id, r.created_at";

pub struct SqliteMemory {
    pool: SqlitePool,
    credentials: Arc<dyn Credentials>,
}

impl SqliteMemory {
    pub async fn new(pool: SqlitePool, credentials: Arc<dyn Credentials>) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self { pool, credentials })
    }
}

/// An agent reaches its own memory; the operator, anyone's.
fn authorize(caller: &Caller, agent: &Alias) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        Caller::Agent { alias, .. } if alias == agent => Ok(()),
        other => Err(Error::Forbidden(format!(
            "{} may not read the memory of {agent}",
            other.alias()
        ))),
    }
}

#[async_trait]
impl AgentMemory for SqliteMemory {
    async fn add(&self, caller: &Caller, text: &str) -> Result<MemoryRecord> {
        let Caller::Agent {
            alias,
            run_id,
            correlation_id,
        } = caller
        else {
            return Err(Error::Forbidden(
                "only an agent writes its memory".to_owned(),
            ));
        };
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::InvalidInput("a memory record needs text".to_owned()));
        }
        if text.len() > MAX_RECORD_BYTES {
            return Err(Error::InvalidInput(format!(
                "a memory record is at most {MAX_RECORD_BYTES} bytes; keep a fact, put documents in files"
            )));
        }
        // A Run token must not outlive its Run in memory (D24).
        if self.credentials.contains_active_run_token(text).await? {
            return Err(Error::Forbidden(
                "memory record contains an active Run token".to_owned(),
            ));
        }
        let (count, bytes): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(text AS BLOB))), 0)
             FROM records WHERE agent = ?",
        )
        .bind(alias.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        let full = usize::try_from(count).unwrap_or(usize::MAX) >= MAX_RECORDS
            || usize::try_from(bytes).unwrap_or(usize::MAX) + text.len() > MAX_TOTAL_BYTES;
        if full {
            // The agent decides what to forget (D31).
            return Err(Error::InvalidInput(format!(
                "memory is full ({count} records, {bytes} bytes; at most {MAX_RECORDS} \
                 records and {MAX_TOTAL_BYTES} bytes): delete what is no longer true or needed"
            )));
        }

        let record = MemoryRecord {
            id: MemoryId::new(),
            agent: alias.clone(),
            text: text.to_owned(),
            run_id: Some(*run_id),
            correlation_id: Some(*correlation_id),
            created_at: Timestamp::now(),
        };
        sqlx::query(
            "INSERT INTO records (id, agent, text, run_id, correlation_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.to_string())
        .bind(record.agent.as_str())
        .bind(&record.text)
        .bind(record.run_id.map(|id| id.to_string()))
        .bind(record.correlation_id.map(|id| id.to_string()))
        .bind(record.created_at.as_unix_millis())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(record)
    }

    async fn list(&self, caller: &Caller, agent: &Alias) -> Result<Vec<MemoryRecord>> {
        authorize(caller, agent)?;
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM records r WHERE r.agent = ? ORDER BY r.seq DESC"
        ))
        .bind(agent.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter().map(Row::into_record).collect()
    }

    async fn search(
        &self,
        caller: &Caller,
        agent: &Alias,
        query: &str,
    ) -> Result<Vec<MemoryRecord>> {
        authorize(caller, agent)?;
        let query = query.trim();
        if query.is_empty() {
            return Err(Error::InvalidInput("a search needs a query".to_owned()));
        }
        // The trigram index needs three characters; a shorter query is a plain scan,
        // fine for two hundred records.
        let rows: Vec<Row> = if query.chars().count() >= 3 {
            sqlx::query_as(&format!(
                "SELECT {COLUMNS} FROM records_text t JOIN records r ON r.seq = t.rowid
                 WHERE records_text MATCH ? AND r.agent = ? ORDER BY t.rank LIMIT ?"
            ))
            .bind(phrase(query))
            .bind(agent.as_str())
            .bind(SEARCH_LIMIT)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(&format!(
                "SELECT {COLUMNS} FROM records r
                 WHERE r.agent = ? AND instr(lower(r.text), lower(?)) > 0
                 ORDER BY r.seq DESC LIMIT ?"
            ))
            .bind(agent.as_str())
            .bind(query)
            .bind(SEARCH_LIMIT)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(internal)?;
        rows.into_iter().map(Row::into_record).collect()
    }

    async fn delete(&self, caller: &Caller, agent: &Alias, id: MemoryId) -> Result<()> {
        authorize(caller, agent)?;
        let deleted = sqlx::query("DELETE FROM records WHERE id = ? AND agent = ?")
            .bind(id.to_string())
            .bind(agent.as_str())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        if deleted.rows_affected() == 0 {
            return Err(Error::NotFound(format!("memory record {id}")));
        }
        Ok(())
    }
}

/// The query as one FTS5 phrase: its own syntax (`OR`, `*`, `:`) stays text.
fn phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    agent: String,
    text: String,
    run_id: Option<String>,
    correlation_id: Option<String>,
    created_at: i64,
}

impl Row {
    fn into_record(self) -> Result<MemoryRecord> {
        Ok(MemoryRecord {
            id: parse(&self.id)?,
            agent: parse(&self.agent)?,
            text: self.text,
            run_id: self.run_id.as_deref().map(parse).transpose()?,
            correlation_id: self.correlation_id.as_deref().map(parse).transpose()?,
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
