//! Agent Registry on SQLite: agents, the operator and credentials.

mod token;

use async_trait::async_trait;
use control_plane_common::registry::{
    AgentCard, AgentDirectory, AgentRegistry, Credentials, Secret,
};
use control_plane_common::{Alias, Caller, CorrelationId, Error, Result, RunId};
use sqlx::{SqliteExecutor, SqlitePool};
use std::str::FromStr;
use token::{OPERATOR_PREFIX, RUN_PREFIX, TOKEN_HEX_LEN};

pub struct SqliteRegistry {
    pool: SqlitePool,
}

impl SqliteRegistry {
    /// Wraps a pool and brings the schema up to date.
    pub async fn new(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(Self { pool })
    }

    async fn card(&self, alias: &Alias) -> Result<AgentCard> {
        let card: Option<String> =
            sqlx::query_scalar("SELECT card FROM principals WHERE alias = ? AND kind = 'agent'")
                .bind(alias.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        let card = card.ok_or_else(|| Error::NotFound(format!("agent {alias}")))?;
        decode_card(&card)
    }

    async fn is_active(&self, credential: &str) -> Result<bool> {
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM credentials WHERE hash = ?")
            .bind(token::hash(credential))
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?;
        Ok(found.is_some())
    }
}

#[async_trait]
impl AgentRegistry for SqliteRegistry {
    async fn create_agent(&self, caller: &Caller, card: AgentCard) -> Result<()> {
        require_operator(caller)?;
        let json = serde_json::to_string(&card).map_err(|e| Error::Internal(e.to_string()))?;
        insert_principal(&self.pool, &card.alias, "agent", Some(json)).await
    }

    async fn get_agent(&self, caller: &Caller, alias: &Alias) -> Result<AgentCard> {
        require_operator(caller)?;
        self.card(alias).await
    }

    async fn list_agents(&self, caller: &Caller) -> Result<Vec<AgentCard>> {
        require_operator(caller)?;
        let cards: Vec<String> =
            sqlx::query_scalar("SELECT card FROM principals WHERE kind = 'agent' ORDER BY alias")
                .fetch_all(&self.pool)
                .await
                .map_err(internal)?;
        cards.iter().map(|card| decode_card(card)).collect()
    }
}

#[async_trait]
impl AgentDirectory for SqliteRegistry {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        self.card(alias).await
    }
}

#[async_trait]
impl Credentials for SqliteRegistry {
    async fn create_operator(&self, alias: &Alias) -> Result<Secret> {
        // One transaction: a failure must not leave the alias taken with no credential.
        let mut tx = self.pool.begin().await.map_err(internal)?;
        insert_principal(&mut *tx, alias, "operator", None).await?;
        let secret = token::generate(OPERATOR_PREFIX);
        store_credential(&mut *tx, &secret, alias, None).await?;
        tx.commit().await.map_err(internal)?;
        Ok(secret)
    }

    async fn issue_run_token(
        &self,
        agent: &Alias,
        run_id: RunId,
        correlation_id: CorrelationId,
    ) -> Result<Secret> {
        self.card(agent).await?;
        let secret = token::generate(RUN_PREFIX);
        store_credential(&self.pool, &secret, agent, Some((run_id, correlation_id))).await?;
        Ok(secret)
    }

    async fn revoke_run_token(&self, run_id: RunId) -> Result<()> {
        sqlx::query("DELETE FROM credentials WHERE run_id = ?")
            .bind(run_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }

    async fn revoke_all_run_tokens(&self) -> Result<()> {
        sqlx::query("DELETE FROM credentials WHERE run_id IS NOT NULL")
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }

    async fn authenticate(&self, credential: &str) -> Result<Caller> {
        let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT c.alias, p.kind, c.run_id, c.correlation_id
             FROM credentials c JOIN principals p ON p.alias = c.alias
             WHERE c.hash = ?",
        )
        .bind(token::hash(credential))
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;

        let (alias, kind, run_id, correlation_id) = row.ok_or(Error::Unauthenticated)?;
        let alias = parse(&alias)?;
        match (kind.as_str(), run_id, correlation_id) {
            ("operator", None, None) => Ok(Caller::Operator { alias }),
            ("agent", Some(run_id), Some(correlation_id)) => Ok(Caller::Agent {
                alias,
                run_id: parse(&run_id)?,
                correlation_id: parse(&correlation_id)?,
            }),
            _ => Err(Error::Internal(format!(
                "inconsistent credential of {alias}"
            ))),
        }
    }

    async fn contains_active_run_token(&self, content: &str) -> Result<bool> {
        for (start, _) in content.match_indices(RUN_PREFIX) {
            let end = start + RUN_PREFIX.len() + TOKEN_HEX_LEN;
            if let Some(candidate) = content.get(start..end)
                && self.is_active(candidate).await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

async fn insert_principal(
    db: impl SqliteExecutor<'_>,
    alias: &Alias,
    kind: &str,
    card: Option<String>,
) -> Result<()> {
    sqlx::query("INSERT INTO principals (alias, kind, card) VALUES (?, ?, ?)")
        .bind(alias.as_str())
        .bind(kind)
        .bind(card)
        .execute(db)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation())
            {
                Error::InvalidInput(format!("alias {alias} is taken"))
            } else {
                internal(e)
            }
        })?;
    Ok(())
}

async fn store_credential(
    db: impl SqliteExecutor<'_>,
    secret: &Secret,
    alias: &Alias,
    run: Option<(RunId, CorrelationId)>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO credentials (hash, alias, run_id, correlation_id) VALUES (?, ?, ?, ?)",
    )
    .bind(token::hash(secret.expose()))
    .bind(alias.as_str())
    .bind(run.map(|(id, _)| id.to_string()))
    .bind(run.map(|(_, id)| id.to_string()))
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(())
}

fn require_operator(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        other => Err(Error::Forbidden(format!(
            "{} is not the operator",
            other.alias()
        ))),
    }
}

fn internal(error: sqlx::Error) -> Error {
    Error::Internal(error.to_string())
}

fn decode_card(card: &str) -> Result<AgentCard> {
    serde_json::from_str(card).map_err(|e| Error::Internal(e.to_string()))
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
