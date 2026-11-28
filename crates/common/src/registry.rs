//! The contract of Agent Registry.

use crate::{Alias, Caller, CorrelationId, Result, RunId, Timestamp};
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
    /// What the agent may do, by group (D99).
    pub access: Vec<Access>,
    pub allow_message_to: Vec<Alias>,
    /// Instructions for the agent itself.
    pub prompt: String,
    /// The agent's color in the UI, 0–359 (D101). Registry fills it from the alias
    /// when not given, so it comes back always set.
    #[serde(default)]
    pub hue: Option<u16>,
    /// Skills from the library (D109). A skill grants reading its own files, like a
    /// tool; one the library has disabled is not mounted.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// An agent as Registry keeps it: its card, the card's version (D95) and whether it
/// works (D96). An edit sends it back with the version it was based on; the state
/// changes only by disabling or enabling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    #[serde(flatten)]
    pub card: AgentCard,
    pub version: u32,
    #[serde(default)]
    pub state: AgentState,
}

/// A disabled agent is not woken and takes no messages; its history stays (D96).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    #[default]
    Active,
    Disabled,
}

/// One version of a card: who saved it and when (D95).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentVersion {
    pub version: u32,
    pub author: Alias,
    pub saved_at: Timestamp,
    pub card: AgentCard,
}

/// A group of tools granted together (D99). The one place that says which tools a
/// group holds: Gateway checks against it, and Runtime shows the model the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadFiles,
    WriteFiles,
    Shell,
    Tasks,
    SendMessages,
    Files,
}

impl Access {
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Self::ReadFiles => &["read", "ls", "grep", "find"],
            Self::WriteFiles => &["write", "edit"],
            Self::Shell => &["bash"],
            Self::Tasks => &["task_create", "task_get", "task_update"],
            Self::SendMessages => &["send_message"],
            Self::Files => &["file_upload", "file_download", "file_list"],
        }
    }
}

/// A hue from the alias: the same agent gets the same color until someone repaints it.
/// FNV-1a, fixed here because a change would repaint agents that never chose a color.
pub fn default_hue(alias: &Alias) -> u16 {
    let hash = alias.as_str().bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    (hash % 360) as u16
}

/// Tools every agent has, whatever its grant (D94). A prompt only says that mail
/// arrived (D80), so an agent without `read_messages` could not learn why it woke.
pub const ALWAYS_GRANTED: &[&str] = &[
    "read_messages",
    // Every agent has its own memory (D108).
    "memory_add",
    "memory_list",
    "memory_search",
    "memory_delete",
    // A skill an agent wrote goes to the library, disabled (D109).
    "skill_propose",
];

impl AgentCard {
    /// The card with its hue set: as given, or derived from the alias.
    pub fn with_hue(mut self) -> Self {
        self.hue.get_or_insert_with(|| default_hue(&self.alias));
        self
    }

    /// The tools the agent may use: what is always granted, then its groups'.
    pub fn tools(&self) -> Vec<&'static str> {
        let mut tools = ALWAYS_GRANTED.to_vec();
        for tool in self.access.iter().flat_map(|a| a.tools()) {
            if !tools.contains(tool) {
                tools.push(tool);
            }
        }
        // Pi lists skills only to a model that can read; Gateway keeps such a read
        // inside the skills (D109).
        if !self.skills.is_empty() && !tools.contains(&"read") {
            tools.push("read");
        }
        tools
    }

    pub fn may_use(&self, tool: &str) -> bool {
        ALWAYS_GRANTED.contains(&tool) || self.access.iter().any(|a| a.tools().contains(&tool))
    }
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

/// A browser session: its secret for the cookie and when it ends (D103).
#[derive(Debug)]
pub struct Session {
    pub secret: Secret,
    pub expires_at: Timestamp,
}

/// What the operator calls. Exposed through the external API.
#[async_trait]
pub trait AgentRegistry: Send + Sync {
    /// Creates an agent. Operator only (D51).
    async fn create_agent(&self, caller: &Caller, card: AgentCard) -> Result<()>;

    /// Returns an agent, including its prompt. Operator only.
    async fn get_agent(&self, caller: &Caller, alias: &Alias) -> Result<Agent>;

    /// All agents, ordered by alias. Operator only.
    async fn list_agents(&self, caller: &Caller) -> Result<Vec<Agent>>;

    /// Saves an edited card as the next version (D95). `Conflict` when the edit was
    /// based on a version that is no longer the latest. Everything but the alias
    /// may change. Operator only.
    async fn update_agent(&self, caller: &Caller, edit: Agent) -> Result<Agent>;

    /// Every version of an agent's card, newest first. Operator only.
    async fn agent_versions(&self, caller: &Caller, alias: &Alias) -> Result<Vec<AgentVersion>>;

    /// Stops waking the agent and ends its Run tokens at once (D96, D101). Operator only.
    async fn disable_agent(&self, caller: &Caller, alias: &Alias) -> Result<()>;

    async fn enable_agent(&self, caller: &Caller, alias: &Alias) -> Result<()>;

    /// Removes the card for good. The history stays, and so does the alias: a new
    /// agent under it would inherit another's messages and sessions (D96). Operator only.
    async fn delete_agent(&self, caller: &Caller, alias: &Alias) -> Result<()>;
}

/// What other components read. Internal, never exposed through the API.
#[async_trait]
pub trait AgentDirectory: Send + Sync {
    async fn agent(&self, alias: &Alias) -> Result<AgentCard>;

    /// The card with its version and state: a Run records the version it ran with
    /// (D95), and nobody delivers to a disabled agent (D96).
    async fn record(&self, alias: &Alias) -> Result<Agent>;
}

/// An external sender such as `@github` (D35, D71, D110). It brings events in as
/// messages to the agents it may write to, and reads nothing: agents cannot message it.
/// What it sends is marked external, never to be taken for instructions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adapter {
    pub alias: Alias,
    /// What it brings, e.g. "GitHub issues of the billing repo".
    pub title: String,
    pub allow_message_to: Vec<Alias>,
}

/// The operator's management of adapters. The credential is shown once.
#[async_trait]
pub trait Adapters: Send + Sync {
    async fn create_adapter(&self, caller: &Caller, adapter: Adapter) -> Result<Secret>;

    /// By alias.
    async fn list_adapters(&self, caller: &Caller) -> Result<Vec<Adapter>>;

    /// Title and recipients; the alias never changes.
    async fn update_adapter(&self, caller: &Caller, adapter: Adapter) -> Result<Adapter>;

    /// A new credential; the old one stops working at once.
    async fn rotate_adapter(&self, caller: &Caller, alias: &Alias) -> Result<Secret>;

    /// Its credential is revoked and its alias never given again, as for an agent.
    async fn delete_adapter(&self, caller: &Caller, alias: &Alias) -> Result<()>;
}

/// What Broker asks of an adapter, internal.
#[async_trait]
pub trait AdapterDirectory: Send + Sync {
    async fn adapter(&self, alias: &Alias) -> Result<Adapter>;
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

    /// Opens a browser session for the operator, who proves it with the credential
    /// (D103). The session is a credential of its own, shown once, for a cookie.
    async fn open_session(&self, credential: &str) -> Result<Session>;

    /// The caller of a session. `Unauthenticated` when it is unknown or expired.
    async fn session_caller(&self, session: &str) -> Result<Caller>;

    async fn close_session(&self, session: &str) -> Result<()>;
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
    fn groups_grant_their_tools_and_messages_and_memory_always() {
        let card = AgentCard {
            alias: "coder".parse().unwrap(),
            title: String::new(),
            model: ModelRef {
                provider: "fake".to_owned(),
                id: "fake-1".to_owned(),
            },
            access: vec![Access::ReadFiles, Access::SendMessages],
            allow_message_to: Vec::new(),
            prompt: String::new(),
            hue: None,
            skills: Vec::new(),
        };
        assert_eq!(
            card.tools(),
            [
                "read_messages",
                "memory_add",
                "memory_list",
                "memory_search",
                "memory_delete",
                "skill_propose",
                "read",
                "ls",
                "grep",
                "find",
                "send_message"
            ]
        );
        assert!(card.may_use("read_messages"));
        assert!(card.may_use("grep"));
        assert!(!card.may_use("bash"));
        assert!(!card.may_use("write"));
    }

    #[test]
    fn a_hue_not_given_comes_from_the_alias() {
        let alias: Alias = "coder".parse().unwrap();
        assert_eq!(default_hue(&alias), default_hue(&alias));
        assert!(default_hue(&alias) < 360);
        assert_ne!(
            default_hue(&alias),
            default_hue(&"manager".parse().unwrap())
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("sona_plane_rt_abc".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
    }
}
