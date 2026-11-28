//! Following Broker's notifications and serving agents one Run at a time (D12, D30, D86).

use crate::launcher::Launcher;
use crate::run::Stop;
use crate::store::RunStore;
use crate::transcript;
use async_trait::async_trait;
use control_plane_common::broker::{InboxDelivery, Notifications};
use control_plane_common::journal::{Component, Event, JournalWriter, Outcome};
use control_plane_common::registry::{AgentDirectory, Credentials};
use control_plane_common::runtime::{
    ActiveRun, ModelInfo, RunControl, RunFilter, RunRecord, TranscriptEntry,
};
use control_plane_common::{
    Alias, Caller, CorrelationId, Error, Result, RunId, SessionKey, Timestamp,
};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Root of agent workdirs, Pi sessions and per-Run files (D81). Absolute.
    pub data_dir: PathBuf,
    /// Our Pi extension, as the launcher sees it. Absolute.
    pub extension: PathBuf,
    /// Where the extension reaches the control plane API.
    pub api_url: String,
    /// Model providers for Pi, copied into every Run's config (D62).
    pub models_json: Option<PathBuf>,
    /// The model provider key, passed to Pi in the first slice (D32).
    pub provider_env: Vec<(String, String)>,
    /// The pause before each retry of a failed session. One failure more, and the
    /// session is stuck until the operator retries or skips it (D77).
    pub retry_delays: Vec<Duration>,
    pub limits: Limits,
}

/// Safeguards against a runaway chain (D100). A trip stops the chain until the
/// operator resumes it; a chain counts from its start or its last resume.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Runs one chain may take.
    pub runs_per_chain: u32,
    /// What one chain may spend, in millionths of a dollar.
    pub cost_per_chain_micros: u64,
    /// Runs one agent may start within `wake_window`, across all its chains.
    pub wakes_per_window: u32,
    pub wake_window: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            runs_per_chain: 30,
            cost_per_chain_micros: 5_000_000,
            wakes_per_window: 20,
            wake_window: Duration::from_secs(10 * 60),
        }
    }
}

pub struct Runtime {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) config: RuntimeConfig,
    pub(crate) directory: Arc<dyn AgentDirectory>,
    pub(crate) credentials: Arc<dyn Credentials>,
    pub(crate) inbox: Arc<dyn InboxDelivery>,
    pub(crate) journal: Arc<dyn JournalWriter>,
    pub(crate) launcher: Arc<dyn Launcher>,
    pub(crate) runs: RunStore,
    /// Agents being served, each with the signal that new mail arrived.
    serving: Mutex<HashMap<Alias, Arc<Notify>>>,
    /// Runs in progress by agent, each with the signal to abort it (D27).
    active: Mutex<HashMap<Alias, (ActiveRun, Arc<Stop>)>>,
    /// Pi's models, once read (D104). Held across the read, so one Pi answers for all.
    models: tokio::sync::Mutex<Option<Vec<ModelInfo>>>,
}

impl Runtime {
    /// `pool` is Runtime's own database, for the Runs it keeps (D97).
    pub async fn new(
        config: RuntimeConfig,
        pool: SqlitePool,
        directory: Arc<dyn AgentDirectory>,
        credentials: Arc<dyn Credentials>,
        inbox: Arc<dyn InboxDelivery>,
        journal: Arc<dyn JournalWriter>,
        launcher: Arc<dyn Launcher>,
    ) -> Result<Self> {
        // Pi runs in the agent's workdir, so a relative path would point elsewhere.
        for path in [&config.data_dir, &config.extension] {
            if !path.is_absolute() {
                return Err(Error::InvalidInput(format!(
                    "{} must be an absolute path",
                    path.display()
                )));
            }
        }
        let runs = RunStore::new(pool).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                runs,
                directory,
                credentials,
                inbox,
                journal,
                launcher,
                serving: Mutex::new(HashMap::new()),
                active: Mutex::new(HashMap::new()),
                models: tokio::sync::Mutex::new(None),
            }),
        })
    }

    /// Call once on startup. No Run survived the restart, so neither may its token (D24),
    /// and a Run still marked running has failed.
    /// Then Runtime wakes agents that already have mail and follows Broker's
    /// notifications from there on (D15, D86).
    pub async fn start(&self) -> Result<()> {
        self.inner.credentials.revoke_all_run_tokens().await?;
        self.inner.runs.abandon_unfinished().await?;
        // Subscribe first, so a message stored while reconciling is not missed.
        let notifications = self.inner.inbox.subscribe();
        self.inner.reconcile().await?;
        tokio::spawn(self.inner.clone().follow(notifications));
        Ok(())
    }
}

impl Inner {
    /// Wakes every agent with unread mail.
    async fn reconcile(self: &Arc<Self>) -> Result<()> {
        for agent in self.inbox.agents_with_unread().await? {
            self.wake(&agent);
        }
        Ok(())
    }

    async fn follow(self: Arc<Self>, mut notifications: Notifications) {
        loop {
            match notifications.recv().await {
                Ok(agent) => self.wake(&agent),
                // Notifications were dropped, but the inbox still has everything (D15).
                Err(RecvError::Lagged(_)) => {
                    let _ = self.reconcile().await;
                }
                Err(RecvError::Closed) => break,
            }
        }
    }

    /// Signals the agent's active Run, or starts serving the agent.
    fn wake(self: &Arc<Self>, agent: &Alias) {
        let mut serving = self.serving();
        if let Some(notify) = serving.get(agent) {
            notify.notify_one();
            return;
        }
        let notify = Arc::new(Notify::new());
        serving.insert(agent.clone(), notify.clone());
        tokio::spawn(self.clone().serve(agent.clone(), notify));
    }

    /// Runs the agent's sessions one after another until its inbox is empty (D30, D57).
    async fn serve(self: Arc<Self>, agent: Alias, notify: Arc<Notify>) {
        let drained = loop {
            let batch = match self.inbox.next_batch(&agent).await {
                Ok(Some(batch)) => batch,
                Ok(None) => break true,
                Err(_) => break false,
            };
            let session = batch.session;
            // A failed Run leaves its messages unread (D49), so the next batch retries it.
            if let Err(error) = self.run(&agent, batch, &notify).await
                && self.failed(&agent, session, &error).await.is_err()
            {
                break false;
            }
        };
        self.serving().remove(&agent);

        // A message stored between the last check and the removal woke nobody.
        if drained && matches!(self.inbox.next_batch(&agent).await, Ok(Some(_))) {
            self.wake(&agent);
        }
    }

    /// Counts the failure, then pauses before the retry or stops the session (D77).
    /// The agent's other sessions wait meanwhile: one Run at a time (D30).
    async fn failed(&self, agent: &Alias, session: SessionKey, error: &Error) -> Result<()> {
        let error = error.to_string();
        let failures = self.inbox.record_failure(agent, session, &error).await?;
        let attempt = usize::try_from(failures).unwrap_or(usize::MAX);
        if let Some(delay) = self.config.retry_delays.get(attempt - 1) {
            tokio::time::sleep(*delay).await;
            return Ok(());
        }
        self.inbox.mark_stuck(agent, session).await?;
        let (task_id, correlation_id) = match session {
            SessionKey::Task(id) => (Some(id), None),
            SessionKey::Chain(id) => (None, Some(id)),
        };
        self.journal
            .record(Event {
                actor: agent.clone(),
                component: Component::Runtime,
                action: "session_stuck".to_owned(),
                decision: None,
                result: Some(Outcome::Error),
                reason: Some(error),
                run_id: None,
                correlation_id,
                task_id,
                path: None,
            })
            .await
    }

    /// The agent's workdir and Pi sessions live here, across Runs (D81).
    pub(crate) fn agent_dir(&self, agent: &Alias) -> PathBuf {
        self.config.data_dir.join("agents").join(agent.as_str())
    }

    fn serving(&self) -> MutexGuard<'_, HashMap<Alias, Arc<Notify>>> {
        self.serving.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn active(&self) -> MutexGuard<'_, HashMap<Alias, (ActiveRun, Arc<Stop>)>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl RunControl for Runtime {
    async fn active_runs(&self, caller: &Caller) -> Result<Vec<ActiveRun>> {
        require_operator(caller)?;
        let mut runs: Vec<ActiveRun> = self
            .inner
            .active()
            .values()
            .map(|(run, _)| run.clone())
            .collect();
        runs.sort_by(|a, b| a.agent.as_str().cmp(b.agent.as_str()));
        Ok(runs)
    }

    async fn abort_chain(&self, caller: &Caller, chain: CorrelationId) -> Result<()> {
        require_operator(caller)?;
        for (run, stop) in self.inner.active().values() {
            if run.correlation_id == chain {
                stop.signal("the chain was stopped");
            }
        }
        Ok(())
    }

    async fn abort_agent(&self, caller: &Caller, agent: &Alias, reason: &str) -> Result<()> {
        require_operator(caller)?;
        if let Some((_, stop)) = self.inner.active().get(agent) {
            stop.signal(reason);
        }
        Ok(())
    }

    async fn runs(&self, caller: &Caller, filter: RunFilter) -> Result<Vec<RunRecord>> {
        require_operator(caller)?;
        self.inner.runs.list(&filter).await
    }

    async fn run(&self, caller: &Caller, run_id: RunId) -> Result<RunRecord> {
        require_operator(caller)?;
        self.inner.runs.get(run_id).await
    }

    async fn transcript(&self, caller: &Caller, run_id: RunId) -> Result<Vec<TranscriptEntry>> {
        require_operator(caller)?;
        let run = self.inner.runs.get(run_id).await?;
        let sessions = self.inner.agent_dir(&run.agent).join("sessions");
        let to = run.finished_at.unwrap_or_else(Timestamp::now);
        transcript::read(&sessions, run.session, run.started_at, to).await
    }

    async fn models(&self, caller: &Caller) -> Result<Vec<ModelInfo>> {
        require_operator(caller)?;
        let mut models = self.inner.models.lock().await;
        if let Some(models) = &*models {
            return Ok(models.clone());
        }
        // A failure is not kept: the next request asks Pi again.
        let read = self.inner.read_models().await?;
        *models = Some(read.clone());
        Ok(read)
    }
}

fn require_operator(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        other => Err(Error::Forbidden(format!(
            "{} is not the operator",
            other.alias()
        ))),
    }
}
