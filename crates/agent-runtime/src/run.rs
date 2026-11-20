//! One Run: issue a token, start Pi, deliver messages until it settles, clean up.

use crate::context::{notification, run_context};
use crate::launcher::{Mount, RunSpec};
use crate::rpc::{Command, CommandWriter, Event, EventReader};
use crate::runtime::Inner;
use control_plane_common::broker::{InboxBatch, Message};
use control_plane_common::journal::{Component, Event as JournalEvent, Outcome};
use control_plane_common::layout::{EXTENSION_DIR, PI_DIR, SESSIONS_DIR, WORKDIR};
use control_plane_common::registry::{AgentCard, AgentState, Secret};
use control_plane_common::runtime::{ActiveRun, RunOutcome, RunRecord, Usage};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, MessageId, Result, RunId, SessionKey, TaskId, Timestamp,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

type Commands = CommandWriter<Box<dyn AsyncWrite + Send + Unpin>>;
type Events = EventReader<Box<dyn AsyncRead + Send + Unpin>>;

/// How Pi stopped talking.
enum Ended {
    /// It settled; the newest message it was told about.
    Settled(MessageId),
    /// Pi was aborted, for the reason given to the stop (D27, D101).
    Stopped,
}

/// The signal to abort a Run, with why. The first reason given wins.
#[derive(Default)]
pub(crate) struct Stop {
    notify: Notify,
    reason: OnceLock<String>,
}

impl Stop {
    pub(crate) fn signal(&self, reason: &str) {
        let _ = self.reason.set(reason.to_owned());
        self.notify.notify_one();
    }

    fn reason(&self) -> &str {
        self.reason.get().map_or("stopped", String::as_str)
    }
}

/// What one Run is about.
struct Run {
    /// The agent as it acts in this Run: its alias, `run_id` and chain.
    caller: Caller,
    /// The card as it was when the Run began: a later edit applies to the next (D95).
    card: AgentCard,
    card_version: u32,
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
    /// the Run was told about (D49); on failure or a stop the messages stay unread.
    pub(crate) async fn run(&self, agent: &Alias, batch: InboxBatch, wake: &Notify) -> Result<()> {
        let run_id = RunId::new();
        // The Run continues the chain of the message that triggered it (D41).
        let correlation_id = batch
            .messages
            .first()
            .map(|m| m.correlation_id)
            .ok_or_else(|| Error::Internal("empty batch".to_owned()))?;
        let current = match self.directory.record(agent).await {
            Ok(current) if current.state == AgentState::Active => current,
            // Disabled or deleted since Broker gave the batch: nothing to run (D96).
            Ok(_) | Err(Error::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error),
        };
        let run = Run {
            caller: Caller::Agent {
                alias: agent.clone(),
                run_id,
                correlation_id,
            },
            card: current.card,
            card_version: current.version,
            batch,
            run_id,
            correlation_id,
            dir: self.config.data_dir.join("runs").join(run_id.to_string()),
        };

        // Visible to the aborts before checking the chain and the agent: the operator
        // changes them first, so either this check or the abort catches the Run (D27).
        let stop = Arc::new(Stop::default());
        let active = ActiveRun {
            agent: agent.clone(),
            run_id,
            session: run.batch.session,
            correlation_id,
            started_at: Timestamp::now(),
        };
        self.active().insert(agent.clone(), (active, stop.clone()));
        let outcome = match self.admit(&run).await {
            Ok(true) => self.issue_and_drive(&run, wake, &stop).await.map(Some),
            Ok(false) => Ok(None),
            Err(error) => Err(error),
        };
        self.active().remove(agent);

        match outcome {
            Ok(Some(Ended::Settled(last))) => {
                self.inbox
                    .advance_cursor(agent, run.batch.session, last)
                    .await?;
                self.runs.finish(run_id, RunOutcome::Ok, None).await?;
                self.record(&run, "run_finished", Some(Outcome::Ok), None)
                    .await
            }
            Ok(Some(Ended::Stopped)) => {
                let reason = stop.reason();
                self.runs
                    .finish(run_id, RunOutcome::Stopped, Some(reason))
                    .await?;
                self.record(&run, "run_finished", None, Some(reason.to_owned()))
                    .await
            }
            // The chain or the agent was stopped, or a safeguard stopped the chain now:
            // nothing happened.
            Ok(None) => Ok(()),
            Err(error) => {
                let reason = error.to_string();
                // The Run may have failed before it was stored; then nothing is updated.
                self.runs
                    .finish(run_id, RunOutcome::Error, Some(&reason))
                    .await?;
                self.record(&run, "run_finished", Some(Outcome::Error), Some(reason))
                    .await?;
                Err(error)
            }
        }
    }

    /// Whether the chain may take the Run: it is not stopped, and no safeguard trips.
    /// A trip stops the chain until the operator resumes it (D100).
    async fn admit(&self, run: &Run) -> Result<bool> {
        if self.inbox.is_stopped(run.correlation_id).await? {
            return Ok(false);
        }
        match self.directory.record(run.caller.alias()).await {
            Ok(agent) if agent.state == AgentState::Active => {}
            Ok(_) | Err(Error::NotFound(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let Some(reason) = self.tripped(run).await? else {
            return Ok(true);
        };
        self.inbox.halt_chain(run.correlation_id, &reason).await?;
        self.record(run, "chain_halted", None, Some(reason)).await?;
        Ok(false)
    }

    /// The safeguard this Run would break, if any (D100).
    async fn tripped(&self, run: &Run) -> Result<Option<String>> {
        let limits = &self.config.limits;
        let since = self.inbox.resumed_at(run.correlation_id).await?;
        let (runs, cost) = self.runs.chain_spent(run.correlation_id, since).await?;
        if runs >= limits.runs_per_chain {
            return Ok(Some(format!(
                "the chain reached its limit of {} Runs",
                limits.runs_per_chain
            )));
        }
        if cost >= limits.cost_per_chain_micros {
            return Ok(Some(format!(
                "the chain reached its limit of {} (spent {})",
                dollars(limits.cost_per_chain_micros),
                dollars(cost)
            )));
        }
        let window = i64::try_from(limits.wake_window.as_millis())
            .map_err(|e| Error::Internal(e.to_string()))?;
        let window_start = Timestamp::from_unix_millis(Timestamp::now().as_unix_millis() - window)?;
        let agent = run.caller.alias();
        let wakes = self.runs.started_since(agent, window_start).await?;
        if wakes >= limits.wakes_per_window {
            return Ok(Some(format!(
                "{agent} reached its limit of {} wakes in {} minutes",
                limits.wakes_per_window,
                limits.wake_window.as_secs() / 60
            )));
        }
        Ok(None)
    }

    /// Records the start, issues the token and runs Pi; the token dies with the Run.
    async fn issue_and_drive(&self, run: &Run, wake: &Notify, stop: &Stop) -> Result<Ended> {
        let agent = run.caller.alias();
        self.runs
            .begin(&RunRecord {
                run_id: run.run_id,
                agent: agent.clone(),
                card_version: run.card_version,
                session: run.batch.session,
                correlation_id: run.correlation_id,
                started_at: Timestamp::now(),
                finished_at: None,
                outcome: RunOutcome::Running,
                reason: None,
                usage: Usage::default(),
            })
            .await?;
        self.record(run, "run_started", None, None).await?;
        let token = self
            .credentials
            .issue_run_token(agent, run.run_id, run.correlation_id)
            .await?;
        let outcome = self.drive(run, &token, wake, stop).await;

        // Whatever happened, the token dies with the Run (D24).
        let revoked = self.credentials.revoke_run_token(run.run_id).await;
        // Nothing secret is left there once the token is revoked, so a failed removal is harmless.
        let _ = tokio::fs::remove_dir_all(&run.dir).await;
        outcome.and_then(|ended| revoked.map(|()| ended))
    }

    /// Starts Pi and talks to it.
    async fn drive(&self, run: &Run, token: &Secret, wake: &Notify, stop: &Stop) -> Result<Ended> {
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
            env: self.pi_env(run, token),
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

        let talked = self
            .converse(run, &mut commands, &mut events, wake, stop)
            .await;
        // Closing stdin lets a settled Pi exit; a broken one is killed.
        let _ = commands.close().await;
        if talked.is_err() {
            let _ = process.kill().await;
        }
        let exit = process.wait().await?;
        let ended = talked?;
        if exit != Some(0) {
            return Err(Error::Internal(format!("Pi exited with {exit:?}")));
        }
        Ok(ended)
    }

    /// Prompts Pi and follows it until `agent_settled`, steering in new messages
    /// of the same session as they arrive (D12) and aborting it on a stop (D27).
    async fn converse(
        &self,
        run: &Run,
        commands: &mut Commands,
        events: &mut Events,
        wake: &Notify,
        stop: &Stop,
    ) -> Result<Ended> {
        let messages = &run.batch.messages;
        commands
            .send(&Command::Prompt {
                message: notification(messages),
            })
            .await?;
        let mut delivered: HashSet<MessageId> = messages.iter().map(|m| m.id).collect();
        let mut last = newest(messages)?;
        // How the model's latest answer ended. Pi retries the provider itself and settles
        // normally when it gives up, so this is how a failed model reaches us (D77).
        let mut model_error = None;
        let mut stopping = false;
        // An abort before Pi starts working is a no-op, and the prompt would run anyway:
        // a stop that early is sent once Pi starts (D27).
        let mut working = false;

        loop {
            tokio::select! {
                event = events.next() => match event? {
                    Some(Event::MessageEnd { message }) if message.role == "assistant" => {
                        model_error = message.model_error();
                        if let Some(usage) = &message.usage {
                            self.runs.add_usage(run.run_id, usage.to_usage()).await?;
                        }
                    }
                    Some(Event::AgentStart) => {
                        working = true;
                        if stopping {
                            commands.send(&Command::Abort).await?;
                        }
                    }
                    Some(Event::AgentSettled) if stopping => return Ok(Ended::Stopped),
                    Some(Event::AgentSettled) => {
                        return match model_error {
                            Some(error) => Err(Error::Internal(format!("the model failed: {error}"))),
                            None => Ok(Ended::Settled(last)),
                        };
                    }
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
                () = stop.notify.notified(), if !stopping => {
                    stopping = true;
                    if working {
                        commands.send(&Command::Abort).await?;
                    }
                }
                () = wake.notified(), if !stopping => {
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
            &card.tools().join(","),
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
    fn pi_env(&self, run: &Run, token: &Secret) -> Vec<(String, String)> {
        let session = run.batch.session.to_string();
        let mut env: Vec<(String, String)> = [
            ("SONA_PLANE_RUN_TOKEN", token.expose()),
            ("SONA_PLANE_API_URL", &self.config.api_url),
            // What `read_messages` reads, so the model never has to copy it (D78).
            ("SONA_PLANE_SESSION", &session),
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

fn dollars(micros: u64) -> String {
    format!("${:.2}", micros as f64 / 1e6)
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
