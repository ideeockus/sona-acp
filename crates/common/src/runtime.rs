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

/// Tokens and money a Run spent, summed over the model's answers (D97).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens, cache reads and writes included.
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// US dollars times a million, so sums stay exact.
    pub cost_micros: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.cost_micros += other.cost_micros;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Running,
    Ok,
    Error,
    /// Aborted because its chain was stopped (D27).
    Stopped,
}

/// A Run as Agent Runtime keeps it (D97).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub agent: Alias,
    /// The version of the agent's card the Run ran with (D95).
    pub card_version: u32,
    pub session: SessionKey,
    pub correlation_id: CorrelationId,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub outcome: RunOutcome,
    pub reason: Option<String>,
    pub usage: Usage,
}

/// Which Runs to list. Empty means all; newest first either way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunFilter {
    pub agent: Option<Alias>,
    pub chain: Option<CorrelationId>,
}

/// What the operator asks of Agent Runtime. Exposed through the API.
#[async_trait]
pub trait RunControl: Send + Sync {
    /// The operator only. At most one per agent (D30).
    async fn active_runs(&self, caller: &Caller) -> Result<Vec<ActiveRun>>;

    /// Aborts the chain's active Runs (D27). Stop the chain in Broker first, so that
    /// nothing starts it again.
    async fn abort_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()>;

    /// Aborts the agent's Run, as when it is disabled or deleted (D101). The Run is
    /// kept as stopped with `reason`. Operator only.
    async fn abort_agent(&self, caller: &Caller, agent: &Alias, reason: &str) -> Result<()>;

    /// The newest Runs that match, at most a hundred. The operator only.
    async fn runs(&self, caller: &Caller, filter: RunFilter) -> Result<Vec<RunRecord>>;

    async fn run(&self, caller: &Caller, run_id: RunId) -> Result<RunRecord>;
}
