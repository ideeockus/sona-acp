//! Where Pi runs: a container per Run, a sandbox later (D58, D82).

use async_trait::async_trait;
use control_plane_common::layout::WORKDIR;
use control_plane_common::{Error, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};

/// Everything a launcher needs to start Pi for one Run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSpec {
    /// Unique per Run, e.g. for the container name.
    pub name: String,
    /// Pi's arguments, with paths as the environment sees them.
    pub args: Vec<String>,
    /// The complete environment of Pi. Nothing comes from the host (spec §5.4).
    pub env: Vec<(String, String)>,
    pub mounts: Vec<Mount>,
}

/// A host directory made visible inside the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub host: PathBuf,
    pub inside: String,
    pub read_only: bool,
}

/// A started Pi: its stdio and a handle on the process.
pub struct Launched {
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    pub process: Box<dyn Process>,
}

#[async_trait]
pub trait Process: Send {
    /// Waits for the exit. `None` means killed by a signal.
    async fn wait(&mut self) -> Result<Option<i32>>;

    async fn kill(&mut self) -> Result<()>;
}

#[async_trait]
pub trait Launcher: Send + Sync {
    async fn start(&self, spec: RunSpec) -> Result<Launched>;
}

/// Runs Pi in a fresh container per Run, through the `docker` CLI (D83).
pub struct DockerLauncher {
    image: String,
}

impl DockerLauncher {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
        }
    }

    /// The `docker run` arguments. Environment values are not among them: `-e NAME`
    /// takes each value from the CLI's own environment, so no token shows up in `ps`.
    fn args(&self, spec: &RunSpec) -> Vec<String> {
        let mut args: Vec<String> = [
            "run",
            "--interactive",
            "--rm",
            "--name",
            &spec.name,
            "--workdir",
            WORKDIR,
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp",
        ]
        .map(str::to_owned)
        .to_vec();
        for mount in &spec.mounts {
            let mut volume = format!("{}:{}", mount.host.display(), mount.inside);
            if mount.read_only {
                volume.push_str(":ro");
            }
            args.extend(["--volume".to_owned(), volume]);
        }
        for (name, _) in &spec.env {
            args.extend(["--env".to_owned(), name.clone()]);
        }
        args.push(self.image.clone());
        args.extend(spec.args.iter().cloned());
        args
    }
}

#[async_trait]
impl Launcher for DockerLauncher {
    async fn start(&self, spec: RunSpec) -> Result<Launched> {
        // The CLI keeps the host environment it needs to find Docker; the container
        // gets only the variables named in its arguments.
        let mut child = Command::new("docker")
            .args(self.args(&spec))
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Pi writes diagnostics to stderr; they go to the server's stderr for now.
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Internal(format!("cannot start docker: {e}")))?;

        let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        Ok(Launched {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            process: Box::new(Container {
                name: spec.name,
                cli: child,
            }),
        })
    }
}

struct Container {
    name: String,
    cli: Child,
}

#[async_trait]
impl Process for Container {
    async fn wait(&mut self) -> Result<Option<i32>> {
        let status = self.cli.wait().await.map_err(io)?;
        Ok(status.code())
    }

    /// Killing the CLI would leave the container running, so the container is killed.
    async fn kill(&mut self) -> Result<()> {
        let status = Command::new("docker")
            .args(["kill", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(io)?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Internal(format!("docker kill {} failed", self.name)))
        }
    }
}

fn missing(stream: &str) -> Error {
    Error::Internal(format!("docker has no {stream}"))
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_stay_out_of_the_command_line() {
        let spec = RunSpec {
            name: "sona-run-1".to_owned(),
            args: vec!["--mode".to_owned(), "rpc".to_owned()],
            env: vec![(
                "SONA_PLANE_RUN_TOKEN".to_owned(),
                "sona_plane_rt_secret".to_owned(),
            )],
            mounts: vec![
                Mount {
                    host: PathBuf::from("/data/agents/coder/work"),
                    inside: "/work".to_owned(),
                    read_only: false,
                },
                Mount {
                    host: PathBuf::from("/opt/extension"),
                    inside: "/agent/extension".to_owned(),
                    read_only: true,
                },
            ],
        };
        let args = DockerLauncher::new("sona-plane-agent:dev").args(&spec);
        let line = args.join(" ");

        assert!(!line.contains("secret"));
        assert!(line.contains("--env SONA_PLANE_RUN_TOKEN"));
        assert!(line.contains("--volume /data/agents/coder/work:/work "));
        assert!(line.contains("--volume /opt/extension:/agent/extension:ro"));
        for hardening in [
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--read-only",
        ] {
            assert!(line.contains(hardening), "missing {hardening}");
        }
        // Pi's arguments come after the image.
        assert!(line.ends_with("sona-plane-agent:dev --mode rpc"));
    }
}
