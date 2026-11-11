//! The local launcher, with `sh` standing in for Pi.

use control_plane_agent_runtime::launcher::{Launcher, LocalLauncher, RunSpec};
use control_plane_agent_runtime::rpc::{Command, CommandWriter, Event, EventReader};
use std::path::PathBuf;

fn workdir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

fn sh(script: &str, env: &[(&str, &str)]) -> RunSpec {
    RunSpec {
        args: vec!["-c".to_owned(), script.to_owned()],
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        workdir: workdir(),
    }
}

fn launcher() -> LocalLauncher {
    LocalLauncher::new("sh", std::env::var("PATH").unwrap())
}

#[tokio::test]
async fn talks_jsonl_over_stdio() {
    // Echoes the first command back as the `command` of a response, then settles.
    let script = r#"read line; printf '{"type":"response","command":%s,"success":true}\n{"type":"agent_settled"}\n' "$(printf '%s' "$line" | sed 's/.*"type":\("[a-z]*"\).*/\1/')""#;
    let launched = launcher().start(sh(script, &[])).await.unwrap();
    let mut commands = CommandWriter::new(launched.stdin);
    let mut events = EventReader::new(launched.stdout);
    let mut process = launched.process;

    commands.send(&Command::Abort).await.unwrap();
    assert_eq!(
        events.next().await.unwrap(),
        Some(Event::Response {
            command: "abort".to_owned(),
            success: true,
            error: None
        })
    );
    assert_eq!(events.next().await.unwrap(), Some(Event::AgentSettled));
    commands.close().await.unwrap();
    assert_eq!(process.wait().await.unwrap(), Some(0));
}

#[tokio::test]
async fn host_environment_is_not_inherited() {
    // HOME is set in the test process, but must not reach the child.
    let script =
        r#"printf '{"type":"response","command":"%s|%s","success":true}\n' "$RUN_TOKEN" "$HOME""#;
    let launched = launcher()
        .start(sh(script, &[("RUN_TOKEN", "t")]))
        .await
        .unwrap();
    let mut events = EventReader::new(launched.stdout);
    let Some(Event::Response { command, .. }) = events.next().await.unwrap() else {
        panic!("no response");
    };
    assert_eq!(command, "t|");
}

#[tokio::test]
async fn kill_stops_a_hanging_process() {
    let launched = launcher().start(sh("sleep 60", &[])).await.unwrap();
    let mut process = launched.process;
    process.kill().await.unwrap();
    assert_eq!(process.wait().await.unwrap(), None);
}

#[tokio::test]
async fn missing_program_is_an_error() {
    let launcher = LocalLauncher::new("/nonexistent/pi", "/usr/bin");
    assert!(launcher.start(sh("", &[])).await.is_err());
}
