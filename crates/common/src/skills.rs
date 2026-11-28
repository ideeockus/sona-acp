//! The contract of the skill library (D109).
//!
//! The operator keeps the library: a skill comes from a git repository pinned to a
//! commit, or an agent proposes one. Either way it arrives disabled and is enabled
//! after review. An agent sees a skill only when its card lists it and the library has
//! it enabled; it never installs one itself.

use crate::{Alias, Caller, Result, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Files one skill may hold, and their total size: instructions and small scripts.
pub const MAX_SKILL_FILES: usize = 200;
pub const MAX_SKILL_BYTES: usize = 5 * 1024 * 1024;

/// A skill in the library, at the version it holds now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// From its `SKILL.md`: lowercase letters, digits and hyphens.
    pub name: String,
    /// When the model should use it, from `SKILL.md`.
    pub description: String,
    pub source: SkillSource,
    /// The content's hash: a new one means new content to review.
    pub version: String,
    pub enabled: bool,
    pub added_by: Alias,
    pub added_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillSource {
    /// `path` in the repository at `url`, fetched at `git_ref`, which was `commit`.
    Git {
        url: String,
        git_ref: String,
        path: String,
        commit: String,
    },
    /// Written by an agent and proposed through `skill_propose`.
    Proposed { by: Alias },
}

/// A skill with what it says and holds, for review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDetail {
    #[serde(flatten)]
    pub skill: Skill,
    /// `SKILL.md` in full.
    pub instructions: String,
    pub files: Vec<SkillFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFile {
    /// Relative to the skill's directory.
    pub path: String,
    pub size: u64,
}

/// Where to fetch a skill from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSkill {
    /// `https://` only.
    pub url: String,
    /// A branch, tag or commit; the remote's default branch when not given.
    #[serde(default)]
    pub git_ref: Option<String>,
    /// The skill's directory in the repository; its root when not given.
    #[serde(default)]
    pub path: Option<String>,
}

/// One file of a proposed skill. Text only: a skill is instructions and scripts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedFile {
    pub path: String,
    pub content: String,
}

/// What the operator does with the library, and what an agent may propose.
#[async_trait]
pub trait SkillLibrary: Send + Sync {
    /// Operator only. By name.
    async fn list_skills(&self, caller: &Caller) -> Result<Vec<Skill>>;

    async fn get_skill(&self, caller: &Caller, name: &str) -> Result<SkillDetail>;

    /// Fetches the skill and adds it, disabled. Operator only.
    async fn add_from_git(&self, caller: &Caller, source: GitSkill) -> Result<Skill>;

    /// Fetches the skill's source again. New content disables it until reviewed.
    async fn update_skill(&self, caller: &Caller, name: &str) -> Result<Skill>;

    async fn set_enabled(&self, caller: &Caller, name: &str, enabled: bool) -> Result<Skill>;

    async fn delete_skill(&self, caller: &Caller, name: &str) -> Result<()>;

    /// An agent's skill, disabled until the operator enables it. A name taken by
    /// another skill is refused; the same agent may propose its own skill again.
    async fn propose(&self, caller: &Caller, files: Vec<ProposedFile>) -> Result<Skill>;
}

/// What Runtime mounts for a Run.
#[async_trait]
pub trait SkillMounts: Send + Sync {
    /// The directories of those named skills that exist and are enabled.
    async fn enabled(&self, names: &[String]) -> Result<Vec<(String, PathBuf)>>;
}
