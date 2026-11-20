//! Runs as Agent Runtime keeps them, on SQLite (D33, D97).

use control_plane_common::runtime::{RunFilter, RunOutcome, RunRecord, Usage};
use control_plane_common::{Error, Result, RunId, Timestamp};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The newest Runs a listing returns.
const LIST_LIMIT: i64 = 100;

pub(crate) struct RunStore {
    pool: SqlitePool,
}

impl RunStore {
    /// Wraps a pool and brings the schema up to date.
    pub(crate) async fn new(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self { pool })
    }

    pub(crate) async fn begin(&self, run: &RunRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO runs (id, agent, session, correlation_id, started_at, outcome)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(run.run_id.to_string())
        .bind(run.agent.as_str())
        .bind(run.session.to_string())
        .bind(run.correlation_id.to_string())
        .bind(run.started_at.as_unix_millis())
        .bind(outcome_str(run.outcome))
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    /// Adds one model answer's usage, so a running Run shows what it spent so far.
    pub(crate) async fn add_usage(&self, run_id: RunId, usage: Usage) -> Result<()> {
        sqlx::query(
            "UPDATE runs SET tokens_in = tokens_in + ?, tokens_out = tokens_out + ?,
                             cost_micros = cost_micros + ?
             WHERE id = ?",
        )
        .bind(to_i64(usage.tokens_in)?)
        .bind(to_i64(usage.tokens_out)?)
        .bind(to_i64(usage.cost_micros)?)
        .bind(run_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    pub(crate) async fn finish(
        &self,
        run_id: RunId,
        outcome: RunOutcome,
        reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE runs SET finished_at = ?, outcome = ?, reason = ? WHERE id = ?")
            .bind(Timestamp::now().as_unix_millis())
            .bind(outcome_str(outcome))
            .bind(reason)
            .bind(run_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }

    /// No Run survives a restart (D24), so one still marked running has failed.
    pub(crate) async fn abandon_unfinished(&self) -> Result<()> {
        sqlx::query(
            "UPDATE runs SET finished_at = ?, outcome = 'error',
                             reason = 'the control plane restarted'
             WHERE outcome = 'running'",
        )
        .bind(Timestamp::now().as_unix_millis())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    pub(crate) async fn list(&self, filter: &RunFilter) -> Result<Vec<RunRecord>> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, agent, session, correlation_id, started_at, finished_at, outcome,
                    reason, tokens_in, tokens_out, cost_micros
             FROM runs
             WHERE (?1 IS NULL OR agent = ?1) AND (?2 IS NULL OR correlation_id = ?2)
             ORDER BY started_at DESC
             LIMIT ?3",
        )
        .bind(filter.agent.as_ref().map(|a| a.as_str().to_owned()))
        .bind(filter.chain.map(|c| c.to_string()))
        .bind(LIST_LIMIT)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter().map(Row::into_record).collect()
    }

    pub(crate) async fn get(&self, run_id: RunId) -> Result<RunRecord> {
        let row: Option<Row> = sqlx::query_as(
            "SELECT id, agent, session, correlation_id, started_at, finished_at, outcome,
                    reason, tokens_in, tokens_out, cost_micros
             FROM runs WHERE id = ?",
        )
        .bind(run_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.ok_or_else(|| Error::NotFound(format!("run {run_id}")))?
            .into_record()
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    agent: String,
    session: String,
    correlation_id: String,
    started_at: i64,
    finished_at: Option<i64>,
    outcome: String,
    reason: Option<String>,
    tokens_in: i64,
    tokens_out: i64,
    cost_micros: i64,
}

impl Row {
    fn into_record(self) -> Result<RunRecord> {
        Ok(RunRecord {
            run_id: parse(&self.id)?,
            agent: parse(&self.agent)?,
            session: parse(&self.session)?,
            correlation_id: parse(&self.correlation_id)?,
            started_at: Timestamp::from_unix_millis(self.started_at)?,
            finished_at: self
                .finished_at
                .map(Timestamp::from_unix_millis)
                .transpose()?,
            outcome: match self.outcome.as_str() {
                "running" => RunOutcome::Running,
                "ok" => RunOutcome::Ok,
                "error" => RunOutcome::Error,
                "stopped" => RunOutcome::Stopped,
                other => return Err(Error::Internal(format!("unknown outcome {other}"))),
            },
            reason: self.reason,
            usage: Usage {
                tokens_in: to_u64(self.tokens_in)?,
                tokens_out: to_u64(self.tokens_out)?,
                cost_micros: to_u64(self.cost_micros)?,
            },
        })
    }
}

fn outcome_str(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Running => "running",
        RunOutcome::Ok => "ok",
        RunOutcome::Error => "error",
        RunOutcome::Stopped => "stopped",
    }
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|e| Error::Internal(e.to_string()))
}

fn to_u64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|e| Error::Internal(e.to_string()))
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
