//! The contract of Agent Runtime towards the operator.

use crate::{Alias, Caller, CorrelationId, Result, RunId, SessionKey, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A Run in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRun {
    pub agent: Alias,
    pub run_id: RunId,
    pub session: SessionKey,
    pub correlation_id: CorrelationId,
    pub started_at: Timestamp,
}

/// What the operator asks of Agent Runtime. Exposed through the API.
#[async_trait]
pub trait RunControl: Send + Sync {
    /// The operator only. At most one per agent (D30).
    async fn active_runs(&self, caller: &Caller) -> Result<Vec<ActiveRun>>;

    /// Aborts the chain's active Runs (D27). Stop the chain in Broker first, so that
    /// nothing starts it again.
    async fn abort_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()>;
}
