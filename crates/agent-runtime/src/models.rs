//! The models Pi can run with the control plane's keys and providers (D104).
//!
//! Pi knows its catalog and which keys it has, so a short-lived Pi answers. It runs
//! like a Run, in the same image and with the same environment, minus the agent.

use crate::launcher::{Mount, RunSpec};
use crate::rpc::{self, Command, CommandWriter, Event, EventReader};
use crate::runtime::Inner;
use control_plane_common::layout::PI_DIR;
use control_plane_common::runtime::ModelInfo;
use control_plane_common::{Error, Result, RunId};
use std::path::Path;
use std::time::Duration;

/// Pi answers in a second or two; longer means a stuck Docker.
const ASK_TIMEOUT: Duration = Duration::from_secs(60);

impl Inner {
    pub(crate) async fn read_models(&self) -> Result<Vec<ModelInfo>> {
        // Pi writes its auth and model stores here; nothing in it outlives the answer.
        let pi_dir = self
            .config
            .data_dir
            .join("models")
            .join(RunId::new().to_string());
        self.prepare_pi_dir(&pi_dir).await?;
        let models = self.ask_pi(&pi_dir).await;
        let _ = tokio::fs::remove_dir_all(&pi_dir).await;
        models
    }

    async fn ask_pi(&self, pi_dir: &Path) -> Result<Vec<ModelInfo>> {
        let spec = RunSpec {
            name: format!("sona-models-{}", RunId::new()),
            args: ["--mode", "rpc", "--no-session"]
                .map(str::to_owned)
                .to_vec(),
            env: self.pi_home_env(),
            mounts: vec![Mount {
                host: pi_dir.to_path_buf(),
                inside: PI_DIR.to_owned(),
                read_only: false,
            }],
        };
        let launched = self.launcher.start(spec).await?;
        let mut commands = CommandWriter::new(launched.stdin);
        let mut events = EventReader::new(launched.stdout);
        let mut process = launched.process;

        let answer = tokio::time::timeout(ASK_TIMEOUT, async {
            commands.send(&Command::GetAvailableModels).await?;
            answer(&mut events).await
        })
        .await
        .unwrap_or_else(|_| {
            Err(Error::Internal(
                "Pi did not list its models in time".to_owned(),
            ))
        });
        let _ = commands.close().await;
        // Dropping the CLI would leave the container running: kill it by name.
        if answer.is_err() {
            let _ = process.kill().await;
        }
        let _ = process.wait().await;
        answer
    }
}

async fn answer<R: tokio::io::AsyncRead + Unpin>(
    events: &mut EventReader<R>,
) -> Result<Vec<ModelInfo>> {
    loop {
        match events.next().await? {
            Some(Event::Response {
                command,
                success,
                error,
                data,
            }) if command == "get_available_models" => {
                return match (success, data) {
                    (true, Some(data)) => rpc::available_models(data),
                    _ => Err(Error::Internal(format!(
                        "Pi refused to list models: {}",
                        error.unwrap_or_default()
                    ))),
                };
            }
            Some(_) => {}
            None => return Err(Error::Internal("Pi exited without its models".to_owned())),
        }
    }
}
