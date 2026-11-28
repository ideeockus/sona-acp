//! `sapctl status` for a human: agents, their Runs and failing sessions (D77).

use control_plane_common::Timestamp;
use control_plane_common::registry::AgentState;
use control_plane_common::status::Status;

pub fn render(status: &Status, now: Timestamp) -> String {
    let agents = &status.agents;
    // Bare aliases, as the commands take them.
    let mut rows = vec![row(["AGENT", "STATE", "RUN", "SESSION", "SINCE", "UNREAD"])];
    for agent in agents {
        let unread = agent.unread.to_string();
        rows.push(match &agent.run {
            Some(run) => vec![
                agent.alias.as_str().to_owned(),
                "running".to_owned(),
                run.run_id.to_string(),
                run.session.to_string(),
                since(run.started_at, now),
                unread,
            ],
            None => {
                let state = match agent.state {
                    AgentState::Active => "idle",
                    AgentState::Disabled => "disabled",
                };
                row([agent.alias.as_str(), state, "-", "-", "-", &unread])
            }
        });
    }
    let mut out = table(rows);

    let failing: Vec<_> = agents
        .iter()
        .flat_map(|a| a.failing.iter().map(move |f| (a, f)))
        .collect();
    if !failing.is_empty() {
        let mut rows = vec![row([
            "FAILING",
            "SESSION",
            "FAILURES",
            "STATE",
            "LAST ERROR",
        ])];
        for (agent, session) in &failing {
            let state = if session.stuck { "stuck" } else { "retrying" };
            rows.push(vec![
                agent.alias.as_str().to_owned(),
                session.session.to_string(),
                session.failures.to_string(),
                state.to_owned(),
                session.last_error.clone(),
            ]);
        }
        out.push('\n');
        out.push_str(&table(rows));
        for (agent, session) in failing.iter().filter(|(_, s)| s.stuck) {
            let target = format!("{} {}", agent.alias.as_str(), session.session);
            out.push_str(&format!("\nsapctl retry {target}\nsapctl skip  {target}\n"));
        }
    }

    if !status.stopped_chains.is_empty() {
        out.push_str("\nSTOPPED CHAIN\n");
        for stopped in &status.stopped_chains {
            let chain = stopped.chain;
            let why = stopped
                .reason
                .as_deref()
                .unwrap_or("stopped by the operator");
            out.push_str(&format!("{chain}  {why}\n  sapctl resume {chain}\n"));
        }
    }
    out
}

fn row<const N: usize>(cells: [&str; N]) -> Vec<String> {
    cells.map(str::to_owned).to_vec()
}

/// Columns padded to their widest cell, two spaces apart.
fn table(rows: Vec<Vec<String>>) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// How long ago, in the largest whole unit.
fn since(then: Timestamp, now: Timestamp) -> String {
    let seconds = (now.as_unix_millis() - then.as_unix_millis()).max(0) / 1000;
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_plane_common::broker::{FailingSession, StoppedChain};
    use control_plane_common::runtime::ActiveRun;
    use control_plane_common::status::AgentStatus;
    use control_plane_common::{CorrelationId, RunId, SessionKey};

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_unix_millis(millis).unwrap()
    }

    #[test]
    fn shows_runs_and_how_to_unblock_stuck_sessions() {
        let session = SessionKey::Chain(CorrelationId::new());
        let run_id = RunId::new();
        let agents = vec![
            AgentStatus {
                alias: "coder".parse().unwrap(),
                title: String::new(),
                state: AgentState::Active,
                run: Some(ActiveRun {
                    agent: "coder".parse().unwrap(),
                    run_id,
                    session,
                    correlation_id: CorrelationId::new(),
                    started_at: at(0),
                }),
                unread: 1,
                failing: Vec::new(),
            },
            AgentStatus {
                alias: "manager".parse().unwrap(),
                title: String::new(),
                state: AgentState::Disabled,
                run: None,
                unread: 0,
                failing: vec![FailingSession {
                    session,
                    failures: 3,
                    last_error: "Pi exited".to_owned(),
                    stuck: true,
                }],
            },
        ];
        let stopped = CorrelationId::new();
        let status = Status {
            agents,
            stopped_chains: vec![StoppedChain {
                chain: stopped,
                stopped_at: at(0),
                reason: Some("the chain reached its limit of 30 Runs".to_owned()),
            }],
        };
        let out = render(&status, at(150_000));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("AGENT    STATE     RUN"));
        assert!(lines[1].starts_with(&format!("coder    running   {run_id}  {session}  2m")));
        assert!(lines[2].starts_with("manager  disabled  -"));
        assert!(out.contains("3         stuck  Pi exited"));
        assert!(out.contains(&format!("sapctl retry manager {session}")));
        assert!(out.contains(&format!(
            "{stopped}  the chain reached its limit of 30 Runs\n  sapctl resume {stopped}"
        )));
    }

    #[test]
    fn rounds_down_to_the_largest_unit() {
        assert_eq!(since(at(0), at(59_999)), "59s");
        assert_eq!(since(at(0), at(3_600_000)), "1h");
        assert_eq!(since(at(10), at(0)), "0s");
    }
}
