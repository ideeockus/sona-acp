//! The contract of Tool Gateway.

use crate::{Caller, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A tool call as our Pi extension reports it, before Pi runs it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    /// For file tools: the path already resolved by the extension inside the
    /// agent's environment, where the files are (D76).
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Allow,
    /// Pi blocks the call, and the model sees the reason.
    Deny {
        reason: String,
    },
}

/// What our Pi extension calls on every tool call. Exposed through the external API.
///
/// Arguments of the call never reach the Gateway, so they never reach the journal.
#[async_trait]
pub trait ToolGateway: Send + Sync {
    /// Agents only. Every call is recorded in Agent Journal, allowed or not.
    async fn check(&self, caller: &Caller, call: ToolCall) -> Result<Verdict>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Components hold each other as trait objects (D67).
    #[allow(dead_code)]
    struct DynCompatible(Arc<dyn ToolGateway>);

    #[test]
    fn verdict_serializes_with_a_tag() {
        let deny = Verdict::Deny {
            reason: "not granted".to_owned(),
        };
        let json = serde_json::to_value(&deny).unwrap();
        assert_eq!(json["verdict"], "deny");
        assert_eq!(json["reason"], "not granted");
    }
}
