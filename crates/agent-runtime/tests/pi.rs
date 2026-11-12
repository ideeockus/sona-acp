//! Against a real Pi in the agent image. Needs Docker and the image, so CI skips it:
//!
//! ```text
//! docker build -t sona-plane-agent:dev images/agent
//! cargo test -p control-plane-agent-runtime -- --ignored
//! ```

use control_plane_agent_runtime::launcher::{DockerLauncher, Launcher, Mount, RunSpec};
use control_plane_agent_runtime::rpc::{Command, CommandWriter, Event, EventReader};
use control_plane_common::RunId;
use std::path::PathBuf;

#[tokio::test]
#[ignore = "needs Docker and the sona-plane-agent:dev image"]
async fn real_pi_answers_over_rpc_in_a_container() {
    // Everything Pi writes stays under Cargo's target dir.
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("pi-{}", RunId::new()));
    let mount = |name: &str, inside: &str| {
        let host = root.join(name);
        std::fs::create_dir_all(&host).unwrap();
        Mount {
            host,
            inside: inside.to_owned(),
            read_only: false,
        }
    };
    let spec = RunSpec {
        name: format!("sona-test-{}", RunId::new()),
        args: [
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--no-skills",
        ]
        .map(String::from)
        .to_vec(),
        env: [
            ("HOME", "/agent/pi"),
            ("PI_CODING_AGENT_DIR", "/agent/pi"),
            ("PI_OFFLINE", "1"),
            ("PI_SKIP_VERSION_CHECK", "1"),
            ("PI_TELEMETRY", "0"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .to_vec(),
        mounts: vec![mount("work", "/work"), mount("pi", "/agent/pi")],
    };
    let launched = DockerLauncher::new("sona-plane-agent:dev")
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
