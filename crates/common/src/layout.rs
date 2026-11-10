//! Paths inside an agent's environment. Agent Runtime mounts them, Tool Gateway enforces them.

/// The agent's home: its files and working copies. Read-write, always this path (D59).
pub const WORKDIR: &str = "/work";

/// The agent's skills. Read-only (D59).
pub const SKILLS_DIR: &str = "/agent/skills";
