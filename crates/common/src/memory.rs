//! The contract of Agent Memory (D16, D31, D108).

use crate::{Alias, Caller, CorrelationId, MemoryId, Result, RunId, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Records one agent may keep. At a limit `add` fails and the agent decides what to
/// delete (D31).
pub const MAX_RECORDS: usize = 200;
/// Bytes of one record's text: a fact or a decision, not a document.
pub const MAX_RECORD_BYTES: usize = 2 * 1024;
/// Bytes of all of one agent's records together.
pub const MAX_TOTAL_BYTES: usize = 64 * 1024;

/// One thing an agent chose to remember, with the Run that wrote it, so a record a
/// prompt injection planted can be found and removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub agent: Alias,
    pub text: String,
    pub run_id: Option<RunId>,
    pub correlation_id: Option<CorrelationId>,
    pub created_at: Timestamp,
}

/// Every agent has its own memory; nothing of it goes into the prompt by itself, the
/// agent searches it (D16). An agent reaches only its own; the operator reads and
/// deletes any.
#[async_trait]
pub trait AgentMemory: Send + Sync {
    /// The caller's own memory. Agents only.
    async fn add(&self, caller: &Caller, text: &str) -> Result<MemoryRecord>;

    /// The newest first.
    async fn list(&self, caller: &Caller, agent: &Alias) -> Result<Vec<MemoryRecord>>;

    /// Records containing the query, best matches first, at most twenty.
    async fn search(
        &self,
        caller: &Caller,
        agent: &Alias,
        query: &str,
    ) -> Result<Vec<MemoryRecord>>;

    async fn delete(&self, caller: &Caller, agent: &Alias, id: MemoryId) -> Result<()>;
}
