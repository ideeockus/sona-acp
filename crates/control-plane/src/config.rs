//! The configuration file, TOML, given by `--config`.
//!
//! No secrets live here: `provider_env` names variables, and their values come from
//! the control plane's own environment (D32).

use control_plane_common::Alias;
use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The operator (D3). `init` creates its credential.
    pub operator: Alias,
    /// Databases, the journal, agent workdirs and per-Run files (D81). Absolute.
    pub data_dir: PathBuf,
    /// Where the API listens: loopback, or the gateway of the agent network (D88).
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Where agents reach the API from their containers (D88).
    pub api_url: String,
    /// The Docker network agents join (D88).
    pub agent_network: Option<String>,
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
    #[serde(default)]
    pub limits: Limits,
}

/// Safeguards against a runaway chain (D100): `[limits]`, each key optional.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Runs one chain may take since it began or was last resumed.
    pub runs_per_chain: u32,
    /// Dollars one chain may spend since it began or was last resumed.
    pub cost_per_chain_usd: f64,
    /// Runs one agent may start within `wake_window_minutes`.
    pub wakes_per_agent: u32,
    pub wake_window_minutes: u32,
}

impl Default for Limits {
    fn default() -> Self {
        let limits = control_plane_agent_runtime::Limits::default();
        Self {
            runs_per_chain: limits.runs_per_chain,
            cost_per_chain_usd: limits.cost_per_chain_micros as f64 / 1e6,
            wakes_per_agent: limits.wakes_per_window,
            wake_window_minutes: (limits.wake_window.as_secs() / 60) as u32,
        }
    }
}

impl Limits {
    pub fn to_runtime(&self) -> control_plane_agent_runtime::Limits {
        control_plane_agent_runtime::Limits {
            runs_per_chain: self.runs_per_chain,
            cost_per_chain_micros: (self.cost_per_chain_usd * 1e6).round() as u64,
            wakes_per_window: self.wakes_per_agent,
            wake_window: Duration::from_secs(u64::from(self.wake_window_minutes) * 60),
        }
    }

    /// A zero limit would stop every chain at once.
    fn check(&self) -> Result<(), String> {
        if self.runs_per_chain == 0 || self.wakes_per_agent == 0 || self.wake_window_minutes == 0 {
            return Err("limits must be above zero".to_owned());
        }
        if !(self.cost_per_chain_usd.is_finite() && self.cost_per_chain_usd > 0.0) {
            return Err("limits.cost_per_chain_usd must be a positive number".to_owned());
        }
        Ok(())
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 7700))
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
        if !is_internal(config.listen.ip()) {
            return Err(format!(
                "listen {} must be a loopback or private address: the API never faces the internet",
                config.listen
            ));
        }
        config.limits.check()?;
        Ok(config)
    }
}

/// Loopback, or a private IPv4 address such as a Docker network gateway.
/// `0.0.0.0` is neither: it would listen on public interfaces too.
fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback(),
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
        api_url = "http://host.docker.internal:7700"
    "#;

    #[test]
    fn fills_defaults() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.listen, "127.0.0.1:7700".parse().unwrap());
        assert!(config.agent_network.is_none());
        assert_eq!(config.journal_retention_days, 7);
        assert!(config.provider_env.is_empty());
        assert!(config.models_json.is_none());
        let limits = config.limits.to_runtime();
        assert_eq!(limits.runs_per_chain, 30);
        assert_eq!(limits.cost_per_chain_micros, 5_000_000);
        assert_eq!(limits.wakes_per_window, 20);
        assert_eq!(limits.wake_window, Duration::from_secs(600));
    }

    #[test]
    fn takes_limits_in_part() {
        let config =
            Config::parse(&format!("{MINIMAL}\n[limits]\ncost_per_chain_usd = 0.5")).unwrap();
        let limits = config.limits.to_runtime();
        assert_eq!(limits.cost_per_chain_micros, 500_000);
        assert_eq!(limits.runs_per_chain, 30);
        for invalid in [
            "runs_per_chain = 0",
            "cost_per_chain_usd = -1.0",
            "wake_window_minutes = 0",
        ] {
            assert!(
                Config::parse(&format!("{MINIMAL}\n[limits]\n{invalid}")).is_err(),
                "{invalid}"
            );
        }
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
        let typo = format!("{MINIMAL}\nlisten_on = \"127.0.0.1:8080\"");
        assert!(Config::parse(&typo).is_err());
    }

    #[test]
    fn listens_only_on_internal_addresses() {
        let listen = |address: &str| Config::parse(&format!("{MINIMAL}\nlisten = \"{address}\""));
        assert!(listen("127.0.0.1:7700").is_ok());
        assert!(listen("172.30.0.1:7700").is_ok());
        assert!(listen("[::1]:7700").is_ok());
        for public in ["0.0.0.0:7700", "[::]:7700", "203.0.113.5:7700"] {
            assert!(listen(public).unwrap_err().contains("private"), "{public}");
        }
    }

    #[test]
    fn rejects_invalid_operator() {
        let invalid = MINIMAL.replace("\"user\"", "\"@User\"");
        assert!(Config::parse(&invalid).is_err());
    }
}
