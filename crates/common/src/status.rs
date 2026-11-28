//! The operator's view of every agent at once (D77).

use crate::Alias;
use crate::broker::{FailingSession, InboxStatus, StoppedChain};
use crate::registry::{Agent, AgentState};
use crate::runtime::ActiveRun;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub agents: Vec<AgentStatus>,
    /// Chains that deliver nothing until resumed (D27, D100).
    pub stopped_chains: Vec<StoppedChain>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub alias: Alias,
    pub title: String,
    pub state: AgentState,
    /// `None` when the agent is idle.
    pub run: Option<ActiveRun>,
    pub unread: u32,
    pub failing: Vec<FailingSession>,
}

impl AgentStatus {
    /// One line per registered agent, from Registry, Broker and Agent Runtime.
    pub fn of(agents: Vec<Agent>, inboxes: Vec<InboxStatus>, runs: Vec<ActiveRun>) -> Vec<Self> {
        let mut inboxes = inboxes;
        let mut runs = runs;
        agents
            .into_iter()
            .map(|Agent { card, state, .. }| {
                let inbox = take(&mut inboxes, |i| i.agent == card.alias);
                Self {
                    run: take(&mut runs, |r| r.agent == card.alias),
                    unread: inbox.as_ref().map_or(0, |i| i.unread),
                    failing: inbox.map(|i| i.failing).unwrap_or_default(),
                    alias: card.alias,
                    title: card.title,
                    state,
                }
            })
            .collect()
    }
}

fn take<T>(items: &mut Vec<T>, matches: impl Fn(&T) -> bool) -> Option<T> {
    let at = items.iter().position(matches)?;
    Some(items.swap_remove(at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AgentCard, ModelRef};
    use crate::{CorrelationId, RunId, SessionKey, Timestamp};

    fn agent(alias: &str, state: AgentState) -> Agent {
        let card = AgentCard {
            alias: alias.parse().unwrap(),
            title: format!("{alias} title"),
            model: ModelRef {
                provider: "fake".to_owned(),
                id: "fake-1".to_owned(),
            },
            access: Vec::new(),
            allow_message_to: Vec::new(),
            prompt: String::new(),
            hue: None,
            skills: Vec::new(),
        };
        Agent {
            card,
            version: 1,
            state,
        }
    }

    #[test]
    fn joins_the_three_sources_by_agent() {
        let session = SessionKey::Chain(CorrelationId::new());
        let inbox = InboxStatus {
            agent: "manager".parse().unwrap(),
            unread: 2,
            failing: vec![FailingSession {
                session,
                failures: 3,
                last_error: "boom".to_owned(),
                stuck: true,
            }],
        };
        let run = ActiveRun {
            agent: "coder".parse().unwrap(),
            run_id: RunId::new(),
            session,
            correlation_id: CorrelationId::new(),
            started_at: Timestamp::now(),
        };

        let status = AgentStatus::of(
            vec![
                agent("coder", AgentState::Active),
                agent("manager", AgentState::Disabled),
            ],
            vec![inbox],
            vec![run.clone()],
        );
        assert_eq!(status[0].run, Some(run));
        assert_eq!((status[0].unread, status[0].failing.len()), (0, 0));
        assert_eq!(status[1].run, None);
        assert_eq!((status[1].unread, status[1].failing.len()), (2, 1));
        assert_eq!(status[1].state, AgentState::Disabled);
    }
}
