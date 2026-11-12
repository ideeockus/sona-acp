//! One Run: issue a token, start Pi, deliver messages until it settles, clean up.

use crate::context::{notification, run_context};
use crate::launcher::{Mount, RunSpec};
use crate::rpc::{Command, CommandWriter, Event, EventReader};
use crate::runtime::Inner;
use control_plane_common::broker::{InboxBatch, Message};
use control_plane_common::journal::{Component, Event as JournalEvent, Outcome};
use control_plane_common::layout::{EXTENSION_DIR, PI_DIR, SESSIONS_DIR, WORKDIR};
use control_plane_common::registry::{AgentCard, Secret};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, RunId, SessionKey, TaskId,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

type Commands = CommandWriter<Box<dyn AsyncWrite + Send + Unpin>>;
type Events = EventReader<Box<dyn AsyncRead + Send + Unpin>>;

/// What one Run is about.
struct Run {
    /// The agent as it acts in this Run: its alias, `run_id` and chain.
    caller: Caller,
    card: AgentCard,
    batch: InboxBatch,
    run_id: RunId,
    correlation_id: CorrelationId,
    /// Per-Run files, removed afterwards (D81).
    dir: PathBuf,
}

impl Run {
    fn task_id(&self) -> Option<TaskId> {
        match self.batch.session {
            SessionKey::Task(id) => Some(id),
            SessionKey::Chain(_) => None,
        }
    }
}

impl Inner {
    /// Runs Pi for one batch. On success the session's cursor moves past everything
    /// the Run was told about (D49); on failure the messages stay unread.
    pub(crate) async fn run(&self, agent: &Alias, batch: InboxBatch, wake: &Notify) -> Result<()> {
        let run_id = RunId::new();
        // The Run continues the chain of the message that triggered it (D41).
        let correlation_id = batch
            .messages
            .first()
            .map(|m| m.correlation_id)
            .ok_or_else(|| Error::Internal("empty batch".to_owned()))?;
        let run = Run {
            caller: Caller::Agent {
                alias: agent.clone(),
                run_id,
                correlation_id,
            },
            card: self.directory.agent(agent).await?,
            batch,
            run_id,
            correlation_id,
            dir: self.config.data_dir.join("runs").join(run_id.to_string()),
        };

        self.record(&run, "run_started", None, None).await?;
        let token = self
            .credentials
            .issue_run_token(agent, run_id, correlation_id)
            .await?;

        let outcome = self.drive(&run, &token, wake).await;

        // Whatever happened, the token dies with the Run (D24).
        let revoked = self.credentials.revoke_run_token(run_id).await;
        // Nothing secret is left there once the token is revoked, so a failed removal is harmless.
        let _ = tokio::fs::remove_dir_all(&run.dir).await;

        match outcome.and_then(|last| revoked.map(|()| last)) {
            Ok(last) => {
                self.inbox
                    .advance_cursor(agent, run.batch.session, last)
                    .await?;
                self.record(&run, "run_finished", Some(Outcome::Ok), None)
                    .await
            }
            Err(error) => {
                let reason = Some(error.to_string());
                self.record(&run, "run_finished", Some(Outcome::Error), reason)
                    .await?;
                Err(error)
            }
        }
    }

    /// Starts Pi and talks to it. Returns the newest message the Run was told about.
    async fn drive(&self, run: &Run, token: &Secret, wake: &Notify) -> Result<MessageId> {
        let agent_dir = self
            .config
            .data_dir
            .join("agents")
            .join(run.card.alias.as_str());
        let work = agent_dir.join("work");
        let sessions = agent_dir.join("sessions");
        let pi_dir = run.dir.join("pi");
        for dir in [&work, &sessions, &pi_dir] {
            tokio::fs::create_dir_all(dir).await.map_err(io)?;
        }
        if let Some(models) = &self.config.models_json {
            tokio::fs::copy(models, pi_dir.join("models.json"))
                .await
                .map_err(io)?;
        }

        let context = run_context(
            &run.card.alias,
            run.batch.session,
            run.correlation_id,
            &self.addressees(&run.card).await,
        );
        let mount = |host: &Path, inside: &str, read_only| Mount {
            host: host.to_path_buf(),
            inside: inside.to_owned(),
            read_only,
        };
        let spec = RunSpec {
            name: format!("sona-run-{}", run.run_id),
            args: self.pi_args(run, context),
            env: self.pi_env(token),
            mounts: vec![
                mount(&work, WORKDIR, false),
                mount(&sessions, SESSIONS_DIR, false),
                mount(&pi_dir, PI_DIR, false),
                mount(&self.config.extension, EXTENSION_DIR, true),
            ],
        };

        let launched = self.launcher.start(spec).await?;
        let mut commands = CommandWriter::new(launched.stdin);
        let mut events = EventReader::new(launched.stdout);
        let mut process = launched.process;

        let talked = self.converse(run, &mut commands, &mut events, wake).await;
        // Closing stdin lets a settled Pi exit; a broken one is killed.
        let _ = commands.close().await;
        if talked.is_err() {
            let _ = process.kill().await;
        }
        let exit = process.wait().await?;
        let last = talked?;
        if exit != Some(0) {
            return Err(Error::Internal(format!("Pi exited with {exit:?}")));
        }
        Ok(last)
    }

    /// Prompts Pi and follows it until `agent_settled`, steering in new messages
    /// of the same session as they arrive (D12).
    async fn converse(
        &self,
        run: &Run,
        commands: &mut Commands,
        events: &mut Events,
        wake: &Notify,
    ) -> Result<MessageId> {
        let messages = &run.batch.messages;
        commands
            .send(&Command::Prompt {
                message: notification(messages),
            })
            .await?;
        let mut delivered: HashSet<MessageId> = messages.iter().map(|m| m.id).collect();
        let mut last = newest(messages)?;

        loop {
            tokio::select! {
                event = events.next() => match event? {
                    Some(Event::AgentSettled) => return Ok(last),
                    Some(Event::ToolExecutionEnd { tool_name, is_error, .. }) => {
                        let result = if is_error { Outcome::Error } else { Outcome::Ok };
                        self.record(run, &tool_name, Some(result), None).await?;
                    }
                    Some(Event::Response { success: false, command, error }) => {
                        let error = error.unwrap_or_default();
                        return Err(Error::Internal(format!("Pi refused {command}: {error}")));
                    }
                    Some(_) => {}
                    None => return Err(Error::Internal("Pi exited before settling".to_owned())),
                },
                () = wake.notified() => {
                    let fresh: Vec<Message> = self
                        .inbox
                        .unread_in(run.caller.alias(), run.batch.session)
                        .await?
                        .into_iter()
                        .filter(|m| !delivered.contains(&m.id))
                        .collect();
                    // Mail for other sessions waits for its own Run (D57).
                    if !fresh.is_empty() {
                        commands
                            .send(&Command::Steer { message: notification(&fresh) })
                            .await?;
                        last = newest(&fresh)?;
                        delivered.extend(fresh.iter().map(|m| m.id));
                    }
                }
            }
        }
    }

    /// Pi's command line for this agent and session, in container paths (D62, D80).
    fn pi_args(&self, run: &Run, context: String) -> Vec<String> {
        let card = &run.card;
        [
            "--mode",
            "rpc",
            "--model",
            &format!("{}/{}", card.model.provider, card.model.id),
            "--session-dir",
            SESSIONS_DIR,
            "--session-id",
            &run.batch.session.to_string(),
            "--system-prompt",
            &card.prompt,
            "--append-system-prompt",
            &context,
            // The model sees only granted tools; Gateway checks every call anyway (D22).
            "--tools",
            &card.allow_tools.join(","),
            "--no-extensions",
            "-e",
            EXTENSION_DIR,
            "--no-skills",
            "--no-context-files",
            "--no-approve",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// The complete environment of Pi: nothing comes from the host (spec §5.4).
    fn pi_env(&self, token: &Secret) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = [
            ("SONA_PLANE_RUN_TOKEN", token.expose()),
            ("SONA_PLANE_API_URL", &self.config.api_url),
            ("PI_CODING_AGENT_DIR", PI_DIR),
            ("HOME", PI_DIR),
            ("PI_OFFLINE", "1"),
            ("PI_SKIP_VERSION_CHECK", "1"),
            ("PI_TELEMETRY", "0"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .to_vec();
        env.extend(self.config.provider_env.iter().cloned());
        env
    }

    /// The agent's allowed recipients, with titles for those who are agents.
    async fn addressees(&self, card: &AgentCard) -> Vec<(Alias, Option<String>)> {
        let mut addressees = Vec::new();
        for alias in &card.allow_message_to {
            let title = self.directory.agent(alias).await.ok().map(|c| c.title);
            addressees.push((alias.clone(), title));
        }
        addressees
    }

    async fn record(
        &self,
        run: &Run,
        action: &str,
        result: Option<Outcome>,
        reason: Option<String>,
    ) -> Result<()> {
        self.journal
            .record(JournalEvent {
                result,
                reason,
                task_id: run.task_id(),
                ..JournalEvent::new(&run.caller, Component::Runtime, action)
            })
            .await
    }
}

fn newest(messages: &[Message]) -> Result<MessageId> {
    messages
        .last()
        .map(|m| m.id)
        .ok_or_else(|| Error::Internal("empty batch".to_owned()))
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}
