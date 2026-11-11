//! Waking agents and serving their sessions one Run at a time (D12, D15, D30).

use crate::launcher::Launcher;
use async_trait::async_trait;
use control_plane_common::broker::{InboxDelivery, WakeSink};
use control_plane_common::journal::JournalWriter;
use control_plane_common::registry::{AgentDirectory, Credentials};
use control_plane_common::{Alias, Error, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::Notify;

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
    /// Agents being served, each with the signal that new mail arrived.
    serving: Mutex<HashMap<Alias, Arc<Notify>>>,
}

impl Runtime {
    pub fn new(
        config: RuntimeConfig,
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
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                directory,
                credentials,
                inbox,
                journal,
                launcher,
                serving: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Call once on startup: no Run survived the restart, so its token must not either,
    /// and agents with unread mail are woken, since a notification may have been lost
    /// (D15, D24).
    pub async fn start(&self) -> Result<()> {
        self.inner.credentials.revoke_all_run_tokens().await?;
        for agent in self.inner.inbox.agents_with_unread().await? {
            self.inner.wake(&agent);
        }
        Ok(())
    }
}

#[async_trait]
impl WakeSink for Runtime {
    async fn wake(&self, agent: &Alias) {
        self.inner.wake(agent);
    }
}

impl Inner {
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
            match self.inbox.next_batch(&agent).await {
                Ok(Some(batch)) => {
                    // A failed Run leaves its messages unread (D49). Until retries (D77)
                    // arrive, the agent waits for its next wake instead of looping.
                    if self.run(&agent, batch, &notify).await.is_err() {
                        break false;
                    }
                }
                Ok(None) => break true,
                Err(_) => break false,
            }
        };
        self.serving().remove(&agent);

        // A message stored between the last check and the removal woke nobody.
        if drained && matches!(self.inbox.next_batch(&agent).await, Ok(Some(_))) {
            self.wake(&agent);
        }
    }

    fn serving(&self) -> MutexGuard<'_, HashMap<Alias, Arc<Notify>>> {
        self.serving.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
