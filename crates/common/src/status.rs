//! The operator's view of every agent at once (D77).

use crate::broker::{FailingSession, InboxStatus};
use crate::registry::AgentCard;
use crate::runtime::ActiveRun;
use crate::{Alias, CorrelationId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub agents: Vec<AgentStatus>,
    /// Chains the operator stopped (D27).
    pub stopped_chains: Vec<CorrelationId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub alias: Alias,
    pub title: String,
    /// `None` when the agent is idle.
    pub run: Option<ActiveRun>,
    pub unread: u32,
    pub failing: Vec<FailingSession>,
}

impl AgentStatus {
    /// One line per registered agent, from Registry, Broker and Agent Runtime.
    pub fn of(cards: Vec<AgentCard>, inboxes: Vec<InboxStatus>, runs: Vec<ActiveRun>) -> Vec<Self> {
        let mut inboxes = inboxes;
        let mut runs = runs;
        cards
            .into_iter()
            .map(|card| {
                let inbox = take(&mut inboxes, |i| i.agent == card.alias);
                Self {
                    run: take(&mut runs, |r| r.agent == card.alias),
                    unread: inbox.as_ref().map_or(0, |i| i.unread),
                    failing: inbox.map(|i| i.failing).unwrap_or_default(),
                    alias: card.alias,
                    title: card.title,
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
    use crate::registry::ModelRef;
    use crate::{CorrelationId, RunId, SessionKey, Timestamp};

    fn card(alias: &str) -> AgentCard {
        AgentCard {
            alias: alias.parse().unwrap(),
            title: format!("{alias} title"),
            model: ModelRef {
                provider: "fake".to_owned(),
                id: "fake-1".to_owned(),
            },
            allow_tools: Vec::new(),
            allow_message_to: Vec::new(),
            prompt: String::new(),
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
            vec![card("coder"), card("manager")],
            vec![inbox],
            vec![run.clone()],
        );
        assert_eq!(status[0].run, Some(run));
        assert_eq!((status[0].unread, status[0].failing.len()), (0, 0));
        assert_eq!(status[1].run, None);
        assert_eq!((status[1].unread, status[1].failing.len()), (2, 1));
    }
}
