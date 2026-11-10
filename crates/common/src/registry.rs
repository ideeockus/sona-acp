//! The contract of Agent Registry.

use crate::{Alias, Caller, CorrelationId, Result, RunId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The model an agent runs on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

/// An agent definition (D69).
///
/// A declaration only: permissions are still checked on every call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCard {
    pub alias: Alias,
    /// One line for others: agent lists and the Run context.
    pub title: String,
    pub model: ModelRef,
    pub allow_tools: Vec<String>,
    pub allow_message_to: Vec<Alias>,
    /// Instructions for the agent itself.
    pub prompt: String,
}

/// A credential in clear text. Exists only between issuing and handing over.
///
/// Neither `Debug` nor serialization can leak it, so it stays out of logs by construction.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// What the operator calls. Exposed through the external API.
#[async_trait]
pub trait AgentRegistry: Send + Sync {
    /// Creates an agent. Operator only (D51).
    async fn create_agent(&self, caller: &Caller, card: AgentCard) -> Result<()>;

    /// Returns an agent, including its prompt. Operator only.
    async fn get_agent(&self, caller: &Caller, alias: &Alias) -> Result<AgentCard>;

    /// All agents, ordered by alias. Operator only.
    async fn list_agents(&self, caller: &Caller) -> Result<Vec<AgentCard>>;
}

/// What other components read. Internal, never exposed through the API.
#[async_trait]
pub trait AgentDirectory: Send + Sync {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard>;
}

/// Issuing and checking credentials. Internal, never exposed through the API.
#[async_trait]
pub trait Credentials: Send + Sync {
    /// Creates the operator and returns its credential, shown once (D71).
    async fn create_operator(&self, alias: &Alias) -> Result<Secret>;

    /// Issues the token of one Run (D68). It lives exactly as long as the Run (D24).
    async fn issue_run_token(
        &self,
        agent: &Alias,
        run_id: RunId,
        correlation_id: CorrelationId,
    ) -> Result<Secret>;

    async fn revoke_run_token(&self, run_id: RunId) -> Result<()>;

    /// Revokes every Run token. Called on startup, so a crash leaves no orphans (D24).
    async fn revoke_all_run_tokens(&self) -> Result<()>;

    /// Resolves a credential to its caller.
    async fn authenticate(&self, credential: &str) -> Result<Caller>;

    /// Whether `content` holds an active Run token, which must not leave the agent (D24).
    async fn contains_active_run_token(&self, content: &str) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Components hold each other as trait objects (D67).
    #[allow(dead_code)]
    struct DynCompatible(
        Arc<dyn AgentRegistry>,
        Arc<dyn AgentDirectory>,
        Arc<dyn Credentials>,
    );

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("sona_plane_rt_abc".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
    }
}
