use control_plane_agent_journal::{Clock, FileJournal};
use control_plane_common::journal::{
    Component, Decision, Event, JournalFilter, JournalReader, JournalWriter,
};
use control_plane_common::{Caller, CorrelationId, Error, RunId, TaskId, Timestamp};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// 2026-11-01T12:00:00Z
const NOV_1_NOON: i64 = 1_793_534_400_000;

/// A clock the test moves by hand.
struct TestClock(Arc<AtomicI64>);

impl TestClock {
    fn new() -> (Self, Clock) {
        let millis = Arc::new(AtomicI64::new(NOV_1_NOON));
        let read = millis.clone();
        let clock: Clock =
            Arc::new(move || Timestamp::from_unix_millis(read.load(Ordering::SeqCst)).unwrap());
        (Self(millis), clock)
    }

    fn advance_days(&self, days: i64) {
        self.0.fetch_add(days * DAY_MS, Ordering::SeqCst);
    }
}

/// A fresh directory under Cargo's target dir, so tests never write outside the project.
fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(uuid::Uuid::now_v7().to_string())
}

fn files(dir: &PathBuf) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn operator() -> Caller {
    Caller::Operator {
        alias: "user".parse().unwrap(),
    }
}

fn agent(correlation_id: CorrelationId) -> Caller {
    Caller::Agent {
        alias: "coder".parse().unwrap(),
        run_id: RunId::new(),
        correlation_id,
    }
}

#[tokio::test]
async fn reads_a_chain_back_in_order() {
    let (_clock, clock) = TestClock::new();
    let journal = FileJournal::open(dir(), 7, clock).await.unwrap();
    let chain = CorrelationId::new();

    journal
        .record(Event::new(&agent(chain), Component::Runtime, "run_started"))
        .await
        .unwrap();
    journal
        .record(Event {
            decision: Some(Decision::Deny),
            reason: Some("not granted".to_owned()),
            ..Event::new(&agent(chain), Component::Gateway, "bash")
        })
        .await
        .unwrap();
    journal
        .record(Event::new(
            &agent(CorrelationId::new()),
            Component::Runtime,
            "run_started",
        ))
        .await
        .unwrap();

    let entries = journal
        .read(&operator(), JournalFilter::Chain(chain))
        .await
        .unwrap();
    let actions: Vec<_> = entries.iter().map(|e| e.event.action.as_str()).collect();
    assert_eq!(actions, ["run_started", "bash"]);
    assert_eq!(entries[1].event.decision, Some(Decision::Deny));
}

#[tokio::test]
async fn filters_by_run_and_task() {
    let (_clock, clock) = TestClock::new();
    let journal = FileJournal::open(dir(), 7, clock).await.unwrap();
    let caller = agent(CorrelationId::new());
    let Caller::Agent { run_id, .. } = &caller else {
        unreachable!()
    };
    let task_id = TaskId::new();

    journal
        .record(Event {
            task_id: Some(task_id),
            ..Event::new(&caller, Component::TaskManager, "update_task")
        })
        .await
        .unwrap();

    let by_run = journal
        .read(&operator(), JournalFilter::Run(*run_id))
        .await
        .unwrap();
    let by_task = journal
        .read(&operator(), JournalFilter::Task(task_id))
        .await
        .unwrap();
    assert_eq!(by_run, by_task);
    assert_eq!(by_run.len(), 1);
}

#[tokio::test]
async fn only_the_operator_reads() {
    let (_clock, clock) = TestClock::new();
    let journal = FileJournal::open(dir(), 7, clock).await.unwrap();
    let chain = CorrelationId::new();
    assert!(matches!(
        journal
            .read(&agent(chain), JournalFilter::Chain(chain))
            .await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn closed_days_are_compressed_and_still_readable() {
    let (time, clock) = TestClock::new();
    let dir = dir();
    let journal = FileJournal::open(dir.clone(), 7, clock).await.unwrap();
    let chain = CorrelationId::new();

    journal
        .record(Event::new(&agent(chain), Component::Runtime, "day_one"))
        .await
        .unwrap();
    time.advance_days(1);
    journal
        .record(Event::new(&agent(chain), Component::Runtime, "day_two"))
        .await
        .unwrap();

    assert_eq!(files(&dir), ["2026-11-01.jsonl.gz", "2026-11-02.jsonl"]);
    let actions: Vec<_> = journal
        .read(&operator(), JournalFilter::Chain(chain))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event.action)
        .collect();
    assert_eq!(actions, ["day_one", "day_two"]);
}

#[tokio::test]
async fn compressed_days_expire_after_retention() {
    let (time, clock) = TestClock::new();
    let dir = dir();
    let journal = FileJournal::open(dir.clone(), 7, clock).await.unwrap();
    let event = || Event::new(&agent(CorrelationId::new()), Component::Runtime, "tick");

    journal.record(event()).await.unwrap(); // Nov 1
    time.advance_days(2);
    journal.record(event()).await.unwrap(); // Nov 3
    time.advance_days(7);
    journal.record(event()).await.unwrap(); // Nov 10: keeps Nov 3 and later

    assert_eq!(files(&dir), ["2026-11-03.jsonl.gz", "2026-11-10.jsonl"]);
}

#[tokio::test]
async fn reopening_closes_a_stale_day() {
    let (time, clock) = TestClock::new();
    let dir = dir();
    {
        let journal = FileJournal::open(dir.clone(), 7, clock.clone())
            .await
            .unwrap();
        journal
            .record(Event::new(
                &agent(CorrelationId::new()),
                Component::Runtime,
                "before_restart",
            ))
            .await
            .unwrap();
    }
    time.advance_days(1);
    FileJournal::open(dir.clone(), 7, clock).await.unwrap();
    assert_eq!(files(&dir), ["2026-11-01.jsonl.gz", "2026-11-02.jsonl"]);
}

#[tokio::test]
async fn each_entry_is_one_json_line() {
    let (_clock, clock) = TestClock::new();
    let dir = dir();
    let journal = FileJournal::open(dir.clone(), 7, clock).await.unwrap();
    for _ in 0..3 {
        journal
            .record(Event::new(
                &agent(CorrelationId::new()),
                Component::Runtime,
                "tick",
            ))
            .await
            .unwrap();
    }
    let text = std::fs::read_to_string(dir.join("2026-11-01.jsonl")).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    for line in lines {
        let json: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(json["timestamp"], "2026-11-01T12:00:00Z");
    }
}
