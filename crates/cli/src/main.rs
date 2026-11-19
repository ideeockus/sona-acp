//! `sapctl`: the operator's client of the control plane API (D64, D87).
//!
//! Replies are printed as JSON, ready for `jq`. The credential comes only from
//! `SAPCTL_TOKEN`: an argument would show up in `ps` and the shell history.

mod client;
mod status;

use clap::{Args, Parser, Subcommand};
use client::Client;
use control_plane_common::broker::NewMessage;
use control_plane_common::registry::AgentCard;
use control_plane_common::status::AgentStatus;
use control_plane_common::tasks::{NewTask, TaskResult, TaskStatus, TaskUpdate};
use control_plane_common::{Alias, CorrelationId, MessageId, RunId, SessionKey, TaskId, Timestamp};
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
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// Creates an agent from its card, a JSON file (D69).
    Create {
        card: PathBuf,
    },
    List,
    Get {
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
    Update {
        id: TaskId,
        /// open, in_progress, needs_owner, done or cancelled (D53).
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

#[derive(Args)]
#[group(required = true, multiple = false)]
struct JournalArgs {
    #[arg(long)]
    chain: Option<CorrelationId>,
    #[arg(long)]
    run: Option<RunId>,
    #[arg(long)]
    task: Option<TaskId>,
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
            let agents: Vec<AgentStatus> =
                serde_json::from_value(reply).map_err(|e| format!("invalid reply: {e}"))?;
            status::render(&agents, Timestamp::now())
        }
        Some(reply) => format!("{reply:#}\n"),
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
            Some(json(read_card(&card)?)),
        ),
        Command::Agents(AgentsCommand::List) => (Method::GET, "/v1/agents".into(), None),
        Command::Agents(AgentsCommand::Get { alias }) => {
            (Method::GET, format!("/v1/agents/{}", alias.as_str()), None)
        }
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
        Command::Journal(JournalArgs { chain, run, task }) => {
            let filter = match (chain, run, task) {
                (Some(id), _, _) => format!("chain={id}"),
                (_, Some(id), _) => format!("run={id}"),
                (_, _, Some(id)) => format!("task={id}"),
                // clap requires exactly one of them.
                _ => unreachable!(),
            };
            (Method::GET, format!("/v1/journal?{filter}"), None)
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
    })
}

/// Parsed here first, so a malformed card fails before it reaches the API.
fn read_card(path: &Path) -> Result<AgentCard, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn json(value: impl Serialize) -> Value {
    // Our own DTOs: string keys only, so serialization cannot fail.
    serde_json::to_value(value).expect("DTOs serialize to JSON")
}

fn parse_status(value: &str) -> Result<TaskStatus, String> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|_| "expected open, in_progress, needs_owner, done or cancelled".to_owned())
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
    fn journal_takes_exactly_one_filter() {
        let run = RunId::new().to_string();
        let (_, path, _) = request_of(&["journal", "--run", &run]);
        assert_eq!(path, format!("/v1/journal?run={run}"));
        assert!(parse(&["journal"]).is_err());
        let chain = CorrelationId::new().to_string();
        assert!(parse(&["journal", "--run", &run, "--chain", &chain]).is_err());
    }
}
