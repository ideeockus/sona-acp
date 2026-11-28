//! Agent Registry on SQLite: agents, the operator and credentials.

mod token;

use async_trait::async_trait;
use control_plane_common::registry::{
    Adapter, AdapterDirectory, Adapters, Agent, AgentCard, AgentDirectory, AgentRegistry,
    AgentState, AgentVersion, Credentials, Secret, Session,
};
use control_plane_common::{Alias, Caller, CorrelationId, Error, Result, RunId, Timestamp};
use sqlx::{SqliteExecutor, SqlitePool};
use std::str::FromStr;
use token::{ADAPTER_PREFIX, OPERATOR_PREFIX, RUN_PREFIX, SESSION_PREFIX, TOKEN_HEX_LEN};

/// How long a browser session lasts, counted from login (D103).
const SESSION_LIFETIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;

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

    async fn current(&self, alias: &Alias) -> Result<Agent> {
        let row: Option<AgentRow> = sqlx::query_as(
            "SELECT card, version, disabled FROM principals WHERE alias = ? AND kind = 'agent'",
        )
        .bind(alias.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.ok_or_else(|| Error::NotFound(format!("agent {alias}")))?
            .into_agent()
    }

    async fn adapter_of(&self, alias: &Alias) -> Result<Adapter> {
        let card: Option<String> =
            sqlx::query_scalar("SELECT card FROM principals WHERE alias = ? AND kind = 'adapter'")
                .bind(alias.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        let card = card.ok_or_else(|| Error::NotFound(format!("adapter {alias}")))?;
        decode_adapter(alias, &card)
    }

    /// An adapter writes to agents only, and only to ones that exist.
    async fn check_recipients(&self, adapter: &Adapter) -> Result<()> {
        if adapter.title.trim().is_empty() {
            return Err(Error::InvalidInput("an adapter needs a title".to_owned()));
        }
        for to in &adapter.allow_message_to {
            self.current(to)
                .await
                .map_err(|_| Error::InvalidInput(format!("{to} is not an agent")))?;
        }
        Ok(())
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
        let card = valid(card)?;
        let json = encode_card(&card)?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        insert_principal(&mut *tx, &card.alias, "agent", Some(json.clone())).await?;
        sqlx::query("UPDATE principals SET version = 1 WHERE alias = ?")
            .bind(card.alias.as_str())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        insert_version(&mut *tx, &card.alias, 1, &json, caller.alias()).await?;
        tx.commit().await.map_err(internal)
    }

    async fn get_agent(&self, caller: &Caller, alias: &Alias) -> Result<Agent> {
        require_operator(caller)?;
        self.current(alias).await
    }

    async fn list_agents(&self, caller: &Caller) -> Result<Vec<Agent>> {
        require_operator(caller)?;
        let rows: Vec<AgentRow> = sqlx::query_as(
            "SELECT card, version, disabled FROM principals WHERE kind = 'agent' ORDER BY alias",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter().map(AgentRow::into_agent).collect()
    }

    async fn update_agent(&self, caller: &Caller, edit: Agent) -> Result<Agent> {
        require_operator(caller)?;
        let card = valid(edit.card)?;
        let alias = card.alias.clone();
        let current = self.current(&alias).await?;
        if current.version != edit.version {
            return Err(stale(&alias, current.version, edit.version));
        }
        // Saving the same card again makes no version.
        if current.card == card {
            return Ok(current);
        }

        let json = encode_card(&card)?;
        let version = edit.version + 1;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        // The version in the condition catches an edit saved in between.
        let updated = sqlx::query(
            "UPDATE principals SET card = ?, version = ? WHERE alias = ? AND version = ?",
        )
        .bind(&json)
        .bind(version)
        .bind(alias.as_str())
        .bind(edit.version)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
        if updated.rows_affected() == 0 {
            return Err(stale(&alias, version, edit.version));
        }
        insert_version(&mut *tx, &alias, version, &json, caller.alias()).await?;
        tx.commit().await.map_err(internal)?;
        Ok(Agent {
            card,
            version,
            state: current.state,
        })
    }

    async fn agent_versions(&self, caller: &Caller, alias: &Alias) -> Result<Vec<AgentVersion>> {
        require_operator(caller)?;
        self.current(alias).await?;
        let rows: Vec<(i64, String, i64, String)> = sqlx::query_as(
            "SELECT version, author, saved_at, card FROM agent_versions
             WHERE alias = ? ORDER BY version DESC",
        )
        .bind(alias.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter()
            .map(|(version, author, saved_at, card)| {
                Ok(AgentVersion {
                    version: to_version(version)?,
                    author: parse(&author)?,
                    saved_at: Timestamp::from_unix_millis(saved_at)?,
                    card: decode_card(&card)?,
                })
            })
            .collect()
    }

    async fn disable_agent(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        require_operator(caller)?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        set_disabled(&mut *tx, alias, true).await?;
        // A Run going on cannot call the API any more, before Runtime aborts it.
        revoke_tokens_of(&mut *tx, alias).await?;
        tx.commit().await.map_err(internal)
    }

    async fn enable_agent(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        require_operator(caller)?;
        set_disabled(&self.pool, alias, false).await
    }

    async fn delete_agent(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        require_operator(caller)?;
        self.current(alias).await?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        revoke_tokens_of(&mut *tx, alias).await?;
        for table in ["agent_versions", "principals"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE alias = ?"))
                .bind(alias.as_str())
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
        }
        sqlx::query("INSERT INTO retired_aliases (alias, retired_at) VALUES (?, ?)")
            .bind(alias.as_str())
            .bind(Timestamp::now().as_unix_millis())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)
    }
}

#[async_trait]
impl Adapters for SqliteRegistry {
    async fn create_adapter(&self, caller: &Caller, adapter: Adapter) -> Result<Secret> {
        require_operator(caller)?;
        self.check_recipients(&adapter).await?;
        // One transaction: a failure must not leave the alias taken with no credential.
        let mut tx = self.pool.begin().await.map_err(internal)?;
        insert_principal(
            &mut *tx,
            &adapter.alias,
            "adapter",
            Some(encode_adapter(&adapter)?),
        )
        .await?;
        let secret = token::generate(ADAPTER_PREFIX);
        store_credential(&mut *tx, &secret, &adapter.alias, None).await?;
        tx.commit().await.map_err(internal)?;
        Ok(secret)
    }

    async fn list_adapters(&self, caller: &Caller) -> Result<Vec<Adapter>> {
        require_operator(caller)?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT alias, card FROM principals WHERE kind = 'adapter' ORDER BY alias",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.iter()
            .map(|(alias, card)| decode_adapter(&parse(alias)?, card))
            .collect()
    }

    async fn update_adapter(&self, caller: &Caller, adapter: Adapter) -> Result<Adapter> {
        require_operator(caller)?;
        self.adapter_of(&adapter.alias).await?;
        self.check_recipients(&adapter).await?;
        sqlx::query("UPDATE principals SET card = ? WHERE alias = ? AND kind = 'adapter'")
            .bind(encode_adapter(&adapter)?)
            .bind(adapter.alias.as_str())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        self.adapter_of(&adapter.alias).await
    }

    async fn rotate_adapter(&self, caller: &Caller, alias: &Alias) -> Result<Secret> {
        require_operator(caller)?;
        self.adapter_of(alias).await?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        revoke_tokens_of(&mut *tx, alias).await?;
        let secret = token::generate(ADAPTER_PREFIX);
        store_credential(&mut *tx, &secret, alias, None).await?;
        tx.commit().await.map_err(internal)?;
        Ok(secret)
    }

    async fn delete_adapter(&self, caller: &Caller, alias: &Alias) -> Result<()> {
        require_operator(caller)?;
        self.adapter_of(alias).await?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        revoke_tokens_of(&mut *tx, alias).await?;
        sqlx::query("DELETE FROM principals WHERE alias = ?")
            .bind(alias.as_str())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        sqlx::query("INSERT INTO retired_aliases (alias, retired_at) VALUES (?, ?)")
            .bind(alias.as_str())
            .bind(Timestamp::now().as_unix_millis())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)
    }
}

#[async_trait]
impl AdapterDirectory for SqliteRegistry {
    async fn adapter(&self, alias: &Alias) -> Result<Adapter> {
        self.adapter_of(alias).await
    }
}

#[async_trait]
impl AgentDirectory for SqliteRegistry {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard> {
        Ok(self.current(alias).await?.card)
    }

    async fn record(&self, alias: &Alias) -> Result<Agent> {
        self.current(alias).await
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
        if self.current(agent).await?.state == AgentState::Disabled {
            return Err(Error::InvalidInput(format!("{agent} is disabled")));
        }
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
            ("adapter", None, None) => Ok(Caller::Adapter { alias }),
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

    async fn open_session(&self, credential: &str) -> Result<Session> {
        let caller = self.authenticate(credential).await?;
        if !matches!(caller, Caller::Operator { .. }) {
            return Err(Error::Forbidden(format!(
                "{} is not the operator",
                caller.alias()
            )));
        }
        let now = Timestamp::now().as_unix_millis();
        // Ended sessions are cleared whenever a new one opens.
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        let secret = token::generate(SESSION_PREFIX);
        let expires_at = Timestamp::from_unix_millis(now + SESSION_LIFETIME_MS)?;
        sqlx::query("INSERT INTO sessions (hash, alias, expires_at) VALUES (?, ?, ?)")
            .bind(token::hash(secret.expose()))
            .bind(caller.alias().as_str())
            .bind(expires_at.as_unix_millis())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(Session { secret, expires_at })
    }

    async fn session_caller(&self, session: &str) -> Result<Caller> {
        let alias: Option<String> =
            sqlx::query_scalar("SELECT alias FROM sessions WHERE hash = ? AND expires_at > ?")
                .bind(token::hash(session))
                .bind(Timestamp::now().as_unix_millis())
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        let alias = alias.ok_or(Error::Unauthenticated)?;
        Ok(Caller::Operator {
            alias: parse(&alias)?,
        })
    }

    async fn close_session(&self, session: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE hash = ?")
            .bind(token::hash(session))
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }
}

/// Takes the alias, unless a deleted agent had it (D96).
async fn insert_principal(
    db: impl SqliteExecutor<'_>,
    alias: &Alias,
    kind: &str,
    card: Option<String>,
) -> Result<()> {
    let inserted = sqlx::query(
        "INSERT INTO principals (alias, kind, card)
         SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM retired_aliases WHERE alias = ?1)",
    )
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
    if inserted.rows_affected() == 0 {
        return Err(Error::InvalidInput(format!(
            "alias {alias} belonged to a deleted agent and stays taken"
        )));
    }
    Ok(())
}

async fn set_disabled(db: impl SqliteExecutor<'_>, alias: &Alias, disabled: bool) -> Result<()> {
    let updated =
        sqlx::query("UPDATE principals SET disabled = ? WHERE alias = ? AND kind = 'agent'")
            .bind(disabled)
            .bind(alias.as_str())
            .execute(db)
            .await
            .map_err(internal)?;
    if updated.rows_affected() == 0 {
        return Err(Error::NotFound(format!("agent {alias}")));
    }
    Ok(())
}

/// What `principals.card` keeps for an adapter: everything but the alias.
#[derive(serde::Serialize, serde::Deserialize)]
struct AdapterCard {
    title: String,
    allow_message_to: Vec<Alias>,
}

fn encode_adapter(adapter: &Adapter) -> Result<String> {
    serde_json::to_string(&AdapterCard {
        title: adapter.title.trim().to_owned(),
        allow_message_to: adapter.allow_message_to.clone(),
    })
    .map_err(|e| Error::Internal(e.to_string()))
}

fn decode_adapter(alias: &Alias, card: &str) -> Result<Adapter> {
    let card: AdapterCard =
        serde_json::from_str(card).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(Adapter {
        alias: alias.clone(),
        title: card.title,
        allow_message_to: card.allow_message_to,
    })
}

async fn revoke_tokens_of(db: impl SqliteExecutor<'_>, alias: &Alias) -> Result<()> {
    sqlx::query("DELETE FROM credentials WHERE alias = ?")
        .bind(alias.as_str())
        .execute(db)
        .await
        .map_err(internal)?;
    Ok(())
}

async fn insert_version(
    db: impl SqliteExecutor<'_>,
    alias: &Alias,
    version: u32,
    card: &str,
    author: &Alias,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO agent_versions (alias, version, card, author, saved_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(alias.as_str())
    .bind(version)
    .bind(card)
    .bind(author.as_str())
    .bind(Timestamp::now().as_unix_millis())
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(())
}

fn stale(alias: &Alias, latest: u32, based_on: u32) -> Error {
    Error::Conflict(format!(
        "{alias} is at version {latest}, the edit was based on {based_on}"
    ))
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

fn encode_card(card: &AgentCard) -> Result<String> {
    serde_json::to_string(card).map_err(|e| Error::Internal(e.to_string()))
}

#[derive(sqlx::FromRow)]
struct AgentRow {
    card: String,
    version: i64,
    disabled: i64,
}

impl AgentRow {
    fn into_agent(self) -> Result<Agent> {
        Ok(Agent {
            card: decode_card(&self.card)?,
            version: to_version(self.version)?,
            state: if self.disabled == 0 {
                AgentState::Active
            } else {
                AgentState::Disabled
            },
        })
    }
}

fn to_version(version: i64) -> Result<u32> {
    u32::try_from(version).map_err(|e| Error::Internal(e.to_string()))
}

/// A card as stored before hues came gets its hue from the alias.
fn decode_card(card: &str) -> Result<AgentCard> {
    let card: AgentCard = serde_json::from_str(card).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(card.with_hue())
}

/// The card to store, with its hue set.
fn valid(card: AgentCard) -> Result<AgentCard> {
    if card.hue.is_some_and(|hue| hue >= 360) {
        return Err(Error::InvalidInput("hue must be 0–359".to_owned()));
    }
    Ok(card.with_hue())
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
