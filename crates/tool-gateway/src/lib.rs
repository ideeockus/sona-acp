//! Tool Gateway: the grant and path policy for every tool call (D22, D39, D76).

mod paths;

use async_trait::async_trait;
use control_plane_common::gateway::{ToolCall, ToolGateway, Verdict};
use control_plane_common::journal::{Component, Decision, Event, JournalWriter};
use control_plane_common::layout::SKILLS_DIR;
use control_plane_common::registry::AgentDirectory;
use control_plane_common::{Caller, Error, Result};
use std::sync::Arc;

pub struct Gateway {
    directory: Arc<dyn AgentDirectory>,
    journal: Arc<dyn JournalWriter>,
}

impl Gateway {
    pub fn new(directory: Arc<dyn AgentDirectory>, journal: Arc<dyn JournalWriter>) -> Self {
        Self { directory, journal }
    }

    /// The policy itself, without recording.
    async fn decide(&self, caller: &Caller, call: &ToolCall) -> Result<Verdict> {
        let card = self.directory.agent(caller.alias()).await?;
        // A skill grants reading its files and nothing else (D109). Only the agent's
        // own enabled skills are mounted there, so the directory is the boundary.
        let reads_a_skill = call.tool == "read"
            && !card.skills.is_empty()
            && call
                .path
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p).starts_with(SKILLS_DIR));
        if !card.may_use(&call.tool) && !reads_a_skill {
            return Ok(deny(format!("{} is not granted", call.tool)));
        }
        Ok(match paths::check(&call.tool, call.path.as_deref()) {
            Ok(()) => Verdict::Allow,
            Err(reason) => deny(reason),
        })
    }
}

#[async_trait]
impl ToolGateway for Gateway {
    async fn check(&self, caller: &Caller, call: ToolCall) -> Result<Verdict> {
        if !matches!(caller, Caller::Agent { .. }) {
            return Err(Error::Forbidden(format!(
                "{} is not an agent",
                caller.alias()
            )));
        }
        let verdict = self.decide(caller, &call).await?;
        let (decision, reason) = match &verdict {
            Verdict::Allow => (Decision::Allow, None),
            Verdict::Deny { reason } => (Decision::Deny, Some(reason.clone())),
        };
        // If recording fails, the call fails too, and the extension blocks it (D38).
        self.journal
            .record(Event {
                decision: Some(decision),
                reason,
                path: call.path,
                ..Event::new(caller, Component::Gateway, call.tool)
            })
            .await?;
        Ok(verdict)
    }
}

fn deny(reason: impl Into<String>) -> Verdict {
    Verdict::Deny {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests;
