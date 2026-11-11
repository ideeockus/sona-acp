//! Where Pi runs: a local process now, a container and a sandbox later (D58).

use async_trait::async_trait;
use control_plane_common::{Error, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};

/// Everything a launcher needs to start Pi for one Run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSpec {
    pub args: Vec<String>,
    /// The complete environment. Nothing is inherited from the host (spec §5.4).
    pub env: Vec<(String, String)>,
    pub workdir: PathBuf,
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

/// Runs Pi as a child process on this machine. For development (D58).
pub struct LocalLauncher {
    program: PathBuf,
    path: String,
}

impl LocalLauncher {
    /// `path` becomes `PATH` of the child: Pi is a Node script and must find `node`.
    pub fn new(program: impl Into<PathBuf>, path: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            path: path.into(),
        }
    }
}

#[async_trait]
impl Launcher for LocalLauncher {
    async fn start(&self, spec: RunSpec) -> Result<Launched> {
        let mut child = Command::new(&self.program)
            .args(&spec.args)
            .env_clear()
            .env("PATH", &self.path)
            .envs(spec.env)
            .current_dir(&spec.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Pi writes diagnostics to stderr; they go to the server's stderr for now.
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Error::Internal(format!("cannot start {}: {e}", self.program.display()))
            })?;

        let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        Ok(Launched {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            process: Box::new(LocalProcess(child)),
        })
    }
}

struct LocalProcess(Child);

#[async_trait]
impl Process for LocalProcess {
    async fn wait(&mut self) -> Result<Option<i32>> {
        let status = self.0.wait().await.map_err(internal)?;
        Ok(status.code())
    }

    async fn kill(&mut self) -> Result<()> {
        self.0.kill().await.map_err(internal)
    }
}

fn missing(stream: &str) -> Error {
    Error::Internal(format!("child has no {stream}"))
}

fn internal(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}
