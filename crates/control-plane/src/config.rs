//! The configuration file, TOML, given by `--config`.
//!
//! No secrets live here: `provider_env` names variables, and their values come from
//! the control plane's own environment (D32).

use control_plane_common::Alias;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The operator (D3). `init` creates its credential.
    pub operator: Alias,
    /// Databases, the journal, agent workdirs and per-Run files (D81). Absolute.
    pub data_dir: PathBuf,
    /// The API port. The API listens on 127.0.0.1 only (D85).
    #[serde(default = "default_port")]
    pub port: u16,
    /// The image agents run in (D61).
    pub agent_image: String,
    /// The directory of our Pi extension. Absolute.
    pub extension: PathBuf,
    /// Model providers for Pi (D62). Absolute.
    pub models_json: Option<PathBuf>,
    /// Environment variables passed on to Pi, such as the provider key (D32).
    #[serde(default)]
    pub provider_env: Vec<String>,
    /// Days of closed journal files to keep (D74).
    #[serde(default = "default_retention_days")]
    pub journal_retention_days: u32,
}

fn default_port() -> u16 {
    7700
}

fn default_retention_days() -> u32 {
    7
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        // Agents run with another working directory, so a relative path would mislead.
        let paths = [
            Some(&config.data_dir),
            Some(&config.extension),
            config.models_json.as_ref(),
        ];
        for path in paths.into_iter().flatten() {
            if !path.is_absolute() {
                return Err(format!("{} must be an absolute path", path.display()));
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        operator = "user"
        data_dir = "/var/lib/control-plane"
        agent_image = "sona-plane-agent:dev"
        extension = "/opt/control-plane/extension"
    "#;

    #[test]
    fn fills_defaults() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.port, 7700);
        assert_eq!(config.journal_retention_days, 7);
        assert!(config.provider_env.is_empty());
        assert!(config.models_json.is_none());
    }

    #[test]
    fn example_stays_valid() {
        Config::parse(include_str!("../../../control-plane.example.toml")).unwrap();
    }

    #[test]
    fn rejects_relative_paths() {
        let relative = MINIMAL.replace("/opt/control-plane/extension", "extension");
        assert!(Config::parse(&relative).unwrap_err().contains("absolute"));
        let models = format!("{MINIMAL}\nmodels_json = \"models.json\"");
        assert!(Config::parse(&models).unwrap_err().contains("absolute"));
    }

    #[test]
    fn rejects_unknown_keys() {
        // A typo must not silently fall back to a default.
        let typo = format!("{MINIMAL}\nprot = 8080");
        assert!(Config::parse(&typo).is_err());
    }

    #[test]
    fn rejects_invalid_operator() {
        let invalid = MINIMAL.replace("\"user\"", "\"@User\"");
        assert!(Config::parse(&invalid).is_err());
    }
}
