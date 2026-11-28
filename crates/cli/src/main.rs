//! `sapctl`: the operator's client of the control plane API (D64, D87).
//!
//! Replies are printed as JSON, ready for `jq`. The credential comes only from
//! `SAPCTL_TOKEN`: an argument would show up in `ps` and the shell history.

mod client;
mod status;

use clap::{Args, Parser, Subcommand};
use client::Client;
use control_plane_common::broker::NewMessage;
use control_plane_common::registry::{Agent, AgentCard};
use control_plane_common::schedules::{NewSchedule, ScheduleUpdate};
use control_plane_common::status::Status;
use control_plane_common::tasks::{NewTask, TaskResult, TaskStatus, TaskUpdate};
use control_plane_common::{
    Alias, CorrelationId, MessageId, RunId, ScheduleId, SessionKey, TaskId, Timestamp,
};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about = "Operate the Agent Control Plane")]
struct Cli {
    /// The control plane API.
    #[arg(long, env = "SAPCTL_URL", default_value = "http://127.0.0.1:7700")]
    url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Agents in Registry.
    #[command(subcommand)]
    Agents(AgentsCommand),
    /// Sends a message to an agent.
    Send {
        to: Alias,
        body: String,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        task: Option<TaskId>,
        /// Continues the chain of that message (D41).
        #[arg(long)]
        reply_to: Option<MessageId>,
    },
    /// Unread messages of one session: `task-<id>` or `chain-<id>` (D78).
    Read { session: SessionKey },
    /// Your conversations, latest first; with an agent, your messages with it (D102),
    /// of one thread with `--session`. Showing them does not mark them read.
    Conversations {
        agent: Option<Alias>,
        /// One thread: `task-<id>` or `chain-<id>` (D105).
        #[arg(long, requires = "agent")]
        session: Option<SessionKey>,
        /// The page just before this message.
        #[arg(long, requires = "agent")]
        before: Option<MessageId>,
    },
    /// Tasks.
    #[command(subcommand)]
    Tasks(TasksCommand),
    /// Agent Journal of one chain, Run or task.
    Journal(JournalArgs),
    /// Every agent: its Run, unread mail and failing sessions (D77).
    Status {
        /// Print the API's JSON instead of tables.
        #[arg(long)]
        json: bool,
    },
    /// Delivers a failing or stuck session again (D77).
    Retry { agent: Alias, session: SessionKey },
    /// Marks a session's unread messages read without delivering them (D77).
    Skip { agent: Alias, session: SessionKey },
    /// Aborts the chain's Runs; its messages are kept but wake nobody (D27).
    Stop { chain: CorrelationId },
    /// Delivers a stopped chain again, with what arrived meanwhile (D27).
    Resume { chain: CorrelationId },
    /// Your threads with an agent, latest first: a session each (D105).
    Threads { agent: Alias },
    /// What the model saw and did in a Run (D102).
    Transcript { run: RunId },
    /// The newest Runs with what they spent (D97).
    Runs {
        #[arg(long)]
        agent: Option<Alias>,
        #[arg(long)]
        chain: Option<CorrelationId>,
    },
    /// The models Pi has a key for: what a card may name (D104).
    Models,
    /// Messages to agents on a cron schedule, from @scheduler (D107).
    #[command(subcommand)]
    Schedules(SchedulesCommand),
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// Creates an agent from its card, a JSON file (D69).
    Create {
        card: PathBuf,
    },
    List,
    /// Prints an agent's card with its version, the starting point of an edit.
    Get {
        alias: Alias,
    },
    /// Saves an edited card, a JSON file as `agents get` prints it (D101). Refused
    /// when the card changed since the version in the file.
    Edit {
        card: PathBuf,
    },
    /// Lists every version of an agent's card, newest first.
    Versions {
        alias: Alias,
    },
    /// Stops waking the agent and aborts its Run; its mail waits (D96).
    Disable {
        alias: Alias,
    },
    /// Wakes the agent again with the mail that waited.
    Enable {
        alias: Alias,
    },
    /// Removes the agent for good. Its history stays, and its alias is never given
    /// again (D96).
    Delete {
        alias: Alias,
    },
}

#[derive(Subcommand)]
enum TasksCommand {
    Create {
        title: String,
        #[arg(long, default_value = "")]
        context: String,
        /// The one responsible for moving the task on (D72).
        #[arg(long)]
        assignee: Option<Alias>,
        #[arg(long = "member")]
        members: Vec<Alias>,
        #[arg(long)]
        parent: Option<TaskId>,
    },
    Get {
        id: TaskId,
    },
    /// The newest tasks, newest first; `--status needs_human` shows what waits for you.
    List {
        #[arg(long, value_parser = parse_status)]
        status: Option<TaskStatus>,
    },
    Update {
        id: TaskId,
        /// open, in_progress, needs_human, done or cancelled (D53).
        #[arg(long, value_parser = parse_status)]
        status: Option<TaskStatus>,
        #[arg(long)]
        result: Option<String>,
        #[arg(long = "link", requires = "result")]
        links: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchedulesCommand {
    List,
    /// A message to the agent at every fire, each in a chain of its own.
    Create {
        agent: Alias,
        /// Five fields: minute hour day-of-month month day-of-week, e.g. "0 9 * * 1-5".
        cron: String,
        body: String,
        /// An IANA zone such as Europe/Moscow; UTC when not given.
        #[arg(long)]
        timezone: Option<String>,
        #[arg(long)]
        subject: Option<String>,
    },
    Enable {
        id: ScheduleId,
    },
    Disable {
        id: ScheduleId,
    },
    Delete {
        id: ScheduleId,
    },
}

/// At most one filter; none shows the latest entries.
#[derive(Args)]
#[group(required = false, multiple = false)]
struct JournalArgs {
    #[arg(long)]
    chain: Option<CorrelationId>,
    #[arg(long)]
    run: Option<RunId>,
    #[arg(long)]
    task: Option<TaskId>,
    #[arg(long)]
    agent: Option<Alias>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("sapctl: {e}");
            ExitCode::FAILURE
        }
    }
}

/// What to print: the reply as JSON, or as tables for `status`.
async fn run(cli: Cli) -> Result<String, String> {
    let token = std::env::var("SAPCTL_TOKEN").map_err(|_| "SAPCTL_TOKEN is not set")?;
    let tables = matches!(cli.command, Command::Status { json: false });
    let (method, path, body) = request(cli.command)?;
    let reply = Client::new(&cli.url, token)
        .call(method, &path, body)
        .await?;
    Ok(match reply {
        Some(reply) if tables => {
            let status: Status =
                serde_json::from_value(reply).map_err(|e| format!("invalid reply: {e}"))?;
            status::render(&status, Timestamp::now())
        }
        Some(reply) => {
            if reply["chain_stopped"] == true {
                let chain = reply["correlation_id"].as_str().unwrap_or_default();
                eprintln!(
                    "sapctl: chain {chain} is stopped; the message waits for `sapctl resume {chain}`"
                );
            }
            format!("{reply:#}\n")
        }
        None => String::new(),
    })
}

/// The API request a command stands for. Path parts are typed values, so they
/// need no escaping.
fn request(command: Command) -> Result<(Method, String, Option<Value>), String> {
    Ok(match command {
        Command::Agents(AgentsCommand::Create { card }) => (
            Method::POST,
            "/v1/agents".into(),
            Some(json(read_json::<AgentCard>(&card)?)),
        ),
        Command::Agents(AgentsCommand::List) => (Method::GET, "/v1/agents".into(), None),
        Command::Agents(AgentsCommand::Get { alias }) => {
            (Method::GET, format!("/v1/agents/{}", alias.as_str()), None)
        }
        Command::Agents(AgentsCommand::Edit { card }) => edit(read_json(&card)?),
        Command::Agents(AgentsCommand::Versions { alias }) => (
            Method::GET,
            format!("/v1/agents/{}/versions", alias.as_str()),
            None,
        ),
        Command::Agents(AgentsCommand::Disable { alias }) => (
            Method::POST,
            format!("/v1/agents/{}/disable", alias.as_str()),
            None,
        ),
        Command::Agents(AgentsCommand::Enable { alias }) => (
            Method::POST,
            format!("/v1/agents/{}/enable", alias.as_str()),
            None,
        ),
        Command::Agents(AgentsCommand::Delete { alias }) => (
            Method::DELETE,
            format!("/v1/agents/{}", alias.as_str()),
            None,
        ),
        Command::Send {
            to,
            body,
            subject,
            task,
            reply_to,
        } => {
            let message = NewMessage {
                to,
                body,
                subject,
                task_id: task,
                in_reply_to: reply_to,
                attachments: Vec::new(),
            };
            (Method::POST, "/v1/messages".into(), Some(json(message)))
        }
        Command::Read { session } => (
            Method::GET,
            format!("/v1/sessions/{session}/messages"),
            None,
        ),
        Command::Tasks(TasksCommand::Create {
            title,
            context,
            assignee,
            members,
            parent,
        }) => {
            let task = NewTask {
                title,
                context,
                parent_id: parent,
                assignee,
                members,
                deadline: None,
            };
            (Method::POST, "/v1/tasks".into(), Some(json(task)))
        }
        Command::Tasks(TasksCommand::Get { id }) => (Method::GET, format!("/v1/tasks/{id}"), None),
        Command::Tasks(TasksCommand::List { status }) => {
            let path = match status {
                Some(status) => format!("/v1/tasks?status={}", json(status).as_str().unwrap_or("")),
                None => "/v1/tasks".to_owned(),
            };
            (Method::GET, path, None)
        }
        Command::Tasks(TasksCommand::Update {
            id,
            status,
            result,
            links,
            note,
        }) => {
            let update = TaskUpdate {
                status,
                result: result.map(|text| TaskResult { text, links }),
                note,
            };
            (Method::PATCH, format!("/v1/tasks/{id}"), Some(json(update)))
        }
        Command::Journal(JournalArgs {
            chain,
            run,
            task,
            agent,
        }) => {
            // clap allows at most one of them.
            let filter = match (chain, run, task, agent) {
                (Some(id), ..) => format!("?chain={id}"),
                (_, Some(id), ..) => format!("?run={id}"),
                (_, _, Some(id), _) => format!("?task={id}"),
                (.., Some(alias)) => format!("?agent={}", alias.as_str()),
                _ => String::new(),
            };
            (Method::GET, format!("/v1/journal{filter}"), None)
        }
        Command::Status { .. } => (Method::GET, "/v1/status".into(), None),
        Command::Retry { agent, session } => (
            Method::POST,
            format!("/v1/agents/{}/sessions/{session}/retry", agent.as_str()),
            None,
        ),
        Command::Skip { agent, session } => (
            Method::POST,
            format!("/v1/agents/{}/sessions/{session}/skip", agent.as_str()),
            None,
        ),
        Command::Stop { chain } => (Method::POST, format!("/v1/chains/{chain}/stop"), None),
        Command::Resume { chain } => (Method::POST, format!("/v1/chains/{chain}/resume"), None),
        Command::Conversations { agent: None, .. } => {
            (Method::GET, "/v1/conversations".into(), None)
        }
        Command::Conversations {
            agent: Some(agent),
            session,
            before,
        } => {
            let mut query = Vec::new();
            if let Some(session) = session {
                query.push(format!("session={session}"));
            }
            if let Some(before) = before {
                query.push(format!("before={before}"));
            }
            let mut path = format!("/v1/conversations/{}/messages", agent.as_str());
            if !query.is_empty() {
                path.push_str(&format!("?{}", query.join("&")));
            }
            (Method::GET, path, None)
        }
        Command::Threads { agent } => (
            Method::GET,
            format!("/v1/conversations/{}/threads", agent.as_str()),
            None,
        ),
        Command::Transcript { run } => (Method::GET, format!("/v1/runs/{run}/transcript"), None),
        Command::Models => (Method::GET, "/v1/models".to_owned(), None),
        Command::Schedules(SchedulesCommand::List) => (Method::GET, "/v1/schedules".into(), None),
        Command::Schedules(SchedulesCommand::Create {
            agent,
            cron,
            body,
            timezone,
            subject,
        }) => {
            let schedule = NewSchedule {
                agent,
                cron,
                timezone,
                subject,
                body,
            };
            (Method::POST, "/v1/schedules".into(), Some(json(schedule)))
        }
        Command::Schedules(SchedulesCommand::Enable { id }) => {
            let update = ScheduleUpdate {
                enabled: Some(true),
                ..ScheduleUpdate::default()
            };
            (
                Method::PATCH,
                format!("/v1/schedules/{id}"),
                Some(json(update)),
            )
        }
        Command::Schedules(SchedulesCommand::Disable { id }) => {
            let update = ScheduleUpdate {
                enabled: Some(false),
                ..ScheduleUpdate::default()
            };
            (
                Method::PATCH,
                format!("/v1/schedules/{id}"),
                Some(json(update)),
            )
        }
        Command::Schedules(SchedulesCommand::Delete { id }) => {
            (Method::DELETE, format!("/v1/schedules/{id}"), None)
        }
        Command::Runs { agent, chain } => {
            let mut query = Vec::new();
            if let Some(agent) = agent {
                query.push(format!("agent={}", agent.as_str()));
            }
            if let Some(chain) = chain {
                query.push(format!("chain={chain}"));
            }
            let query = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };
            (Method::GET, format!("/v1/runs{query}"), None)
        }
    })
}

/// Parsed here first, so a malformed card fails before it reaches the API.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn edit(agent: Agent) -> (Method, String, Option<Value>) {
    let path = format!("/v1/agents/{}", agent.card.alias.as_str());
    (Method::PUT, path, Some(json(agent)))
}

fn json(value: impl Serialize) -> Value {
    // Our own DTOs: string keys only, so serialization cannot fail.
    serde_json::to_value(value).expect("DTOs serialize to JSON")
}

fn parse_status(value: &str) -> Result<TaskStatus, String> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|_| "expected open, in_progress, needs_human, done or cancelled".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use serde_json::json;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(["sapctl"].iter().chain(args))
    }

    fn request_of(args: &[&str]) -> (Method, String, Option<Value>) {
        request(parse(args).unwrap().command).unwrap()
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn send_builds_a_message() {
        let (method, path, body) = request_of(&["send", "coder", "hi", "--subject", "s"]);
        assert_eq!((method, path.as_str()), (Method::POST, "/v1/messages"));
        assert_eq!(body.unwrap()["to"], "coder");
    }

    #[test]
    fn bad_values_fail_before_any_request() {
        assert!(parse(&["send", "@Coder", "hi"]).is_err());
        assert!(parse(&["read", "inbox"]).is_err());
        assert!(parse(&["tasks", "get", "42"]).is_err());
        assert!(
            parse(&[
                "tasks",
                "update",
                &TaskId::new().to_string(),
                "--status",
                "closed"
            ])
            .is_err()
        );
    }

    #[test]
    fn update_puts_links_into_the_result() {
        let id = TaskId::new().to_string();
        let (method, path, body) = request_of(&[
            "tasks", "update", &id, "--status", "done", "--result", "ok", "--link", "a", "--link",
            "b",
        ]);
        assert_eq!((method, path), (Method::PATCH, format!("/v1/tasks/{id}")));
        assert_eq!(
            body.unwrap(),
            json!({"status": "done", "result": {"text": "ok", "links": ["a", "b"]}, "note": null})
        );
        // A link without a result has nowhere to go.
        assert!(parse(&["tasks", "update", &id, "--link", "a"]).is_err());
    }

    #[test]
    fn retry_and_skip_name_an_agent_and_a_session() {
        let session = SessionKey::Task(TaskId::new()).to_string();
        let (method, path, body) = request_of(&["retry", "coder", &session]);
        assert_eq!(method, Method::POST);
        assert_eq!(path, format!("/v1/agents/coder/sessions/{session}/retry"));
        assert!(body.is_none());
        let (_, path, _) = request_of(&["skip", "coder", &session]);
        assert!(path.ends_with("/skip"));
        assert!(parse(&["retry", "coder", "inbox"]).is_err());
    }

    #[test]
    fn stop_and_resume_name_a_chain() {
        let chain = CorrelationId::new();
        let (method, path, _) = request_of(&["stop", &chain.to_string()]);
        assert_eq!(
            (method, path),
            (Method::POST, format!("/v1/chains/{chain}/stop"))
        );
        let (_, path, _) = request_of(&["resume", &chain.to_string()]);
        assert_eq!(path, format!("/v1/chains/{chain}/resume"));
        assert!(parse(&["stop", "chain-1"]).is_err());
    }

    #[test]
    fn runs_take_optional_filters() {
        assert_eq!(request_of(&["runs"]).1, "/v1/runs");
        let chain = CorrelationId::new();
        let (_, path, _) = request_of(&["runs", "--agent", "coder", "--chain", &chain.to_string()]);
        assert_eq!(path, format!("/v1/runs?agent=coder&chain={chain}"));
    }

    #[test]
    fn an_edit_puts_the_card_with_its_version() {
        let agent: Agent = serde_json::from_value(json!({
            "alias": "coder", "title": "", "model": {"provider": "p", "id": "m"},
            "access": [], "allow_message_to": [], "prompt": "", "hue": 25, "version": 3
        }))
        .unwrap();
        let (method, path, body) = edit(agent);
        assert_eq!((method, path.as_str()), (Method::PUT, "/v1/agents/coder"));
        let body = body.unwrap();
        assert_eq!((&body["version"], &body["hue"]), (&json!(3), &json!(25)));
        let (_, path, _) = request_of(&["agents", "versions", "coder"]);
        assert_eq!(path, "/v1/agents/coder/versions");
    }

    #[test]
    fn agents_are_disabled_enabled_and_deleted_by_alias() {
        let (method, path, _) = request_of(&["agents", "disable", "coder"]);
        assert_eq!(
            (method, path.as_str()),
            (Method::POST, "/v1/agents/coder/disable")
        );
        let (_, path, _) = request_of(&["agents", "enable", "coder"]);
        assert_eq!(path, "/v1/agents/coder/enable");
        let (method, path, _) = request_of(&["agents", "delete", "coder"]);
        assert_eq!(
            (method, path.as_str()),
            (Method::DELETE, "/v1/agents/coder")
        );
    }

    #[test]
    fn tasks_are_listed_by_status() {
        assert_eq!(request_of(&["tasks", "list"]).1, "/v1/tasks");
        let (_, path, _) = request_of(&["tasks", "list", "--status", "needs_human"]);
        assert_eq!(path, "/v1/tasks?status=needs_human");
    }

    #[test]
    fn conversations_list_or_page_a_thread() {
        assert_eq!(request_of(&["conversations"]).1, "/v1/conversations");
        let before = MessageId::new();
        let (_, path, _) = request_of(&["conversations", "coder", "--before", &before.to_string()]);
        assert_eq!(
            path,
            format!("/v1/conversations/coder/messages?before={before}")
        );
        assert!(parse(&["conversations", "--before", &before.to_string()]).is_err());
    }

    #[test]
    fn a_transcript_names_its_run() {
        let run = RunId::new();
        let (_, path, _) = request_of(&["transcript", &run.to_string()]);
        assert_eq!(path, format!("/v1/runs/{run}/transcript"));
    }

    #[test]
    fn journal_takes_at_most_one_filter() {
        let run = RunId::new().to_string();
        let (_, path, _) = request_of(&["journal", "--run", &run]);
        assert_eq!(path, format!("/v1/journal?run={run}"));
        assert_eq!(request_of(&["journal"]).1, "/v1/journal");
        assert_eq!(
            request_of(&["journal", "--agent", "coder"]).1,
            "/v1/journal?agent=coder"
        );
        let chain = CorrelationId::new().to_string();
        assert!(parse(&["journal", "--run", &run, "--chain", &chain]).is_err());
    }
}
