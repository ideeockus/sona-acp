//! Against a real Pi. Needs `pi` on PATH, so CI skips it:
//! `cargo test -p control-plane-agent-runtime -- --ignored`.

use control_plane_agent_runtime::launcher::{Launcher, LocalLauncher, RunSpec};
use control_plane_agent_runtime::rpc::{Command, CommandWriter, Event, EventReader};
use std::path::PathBuf;

#[tokio::test]
#[ignore = "needs Pi installed"]
async fn real_pi_answers_over_rpc() {
    // Everything Pi writes stays under Cargo's target dir.
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pi");
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let dir = |name: &str| root.join(name).to_string_lossy().into_owned();

    let spec = RunSpec {
        args: [
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--no-skills",
        ]
        .map(String::from)
        .to_vec(),
        env: vec![
            ("HOME".to_owned(), dir("home")),
            ("PI_CODING_AGENT_DIR".to_owned(), dir("agent")),
            ("PI_OFFLINE".to_owned(), "1".to_owned()),
            ("PI_SKIP_VERSION_CHECK".to_owned(), "1".to_owned()),
            ("PI_TELEMETRY".to_owned(), "0".to_owned()),
        ],
        workdir,
    };
    let launched = LocalLauncher::new("pi", std::env::var("PATH").unwrap())
        .start(spec)
        .await
        .unwrap();
    let mut commands = CommandWriter::new(launched.stdin);
    let mut events = EventReader::new(launched.stdout);
    let mut process = launched.process;

    // Abort with nothing running needs no model, so no provider is involved.
    commands.send(&Command::Abort).await.unwrap();
    loop {
        match events.next().await.unwrap().expect("Pi closed stdout") {
            Event::Response {
                command, success, ..
            } => {
                assert_eq!(command, "abort");
                assert!(success);
                break;
            }
            Event::Other => continue,
            other => panic!("unexpected {other:?}"),
        }
    }
    commands.close().await.unwrap();
    assert_eq!(process.wait().await.unwrap(), Some(0));
}
