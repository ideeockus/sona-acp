use super::*;
use control_plane_common::registry::{Secret, Session};
use control_plane_common::{CorrelationId, RunId};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const LEAKED: &str = "sona_plane_rt_leaked";

/// Only `LEAKED` counts as an active Run token.
struct OneActiveToken;

#[async_trait]
impl Credentials for OneActiveToken {
    async fn create_operator(&self, _: &Alias) -> Result<Secret> {
        unimplemented!()
    }
    async fn issue_run_token(&self, _: &Alias, _: RunId, _: CorrelationId) -> Result<Secret> {
        unimplemented!()
    }
    async fn revoke_run_token(&self, _: RunId) -> Result<()> {
        unimplemented!()
    }
    async fn revoke_all_run_tokens(&self) -> Result<()> {
        unimplemented!()
    }
    async fn authenticate(&self, _: &str) -> Result<Caller> {
        unimplemented!()
    }
    async fn contains_active_run_token(&self, content: &str) -> Result<bool> {
        Ok(content.contains(LEAKED))
    }
    async fn open_session(&self, _: &str) -> Result<Session> {
        unimplemented!()
    }
    async fn session_caller(&self, _: &str) -> Result<Caller> {
        unimplemented!()
    }
    async fn close_session(&self, _: &str) -> Result<()> {
        unimplemented!()
    }
}

async fn memory() -> SqliteMemory {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    SqliteMemory::new(pool, Arc::new(OneActiveToken))
        .await
        .unwrap()
}

fn alias(name: &str) -> Alias {
    name.parse().unwrap()
}

fn agent(name: &str) -> Caller {
    Caller::Agent {
        alias: alias(name),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    }
}

fn operator() -> Caller {
    Caller::Operator {
        alias: alias("user"),
    }
}

#[tokio::test]
async fn an_agent_keeps_records_with_the_run_that_wrote_them() {
    let m = memory().await;
    let coder = agent("coder");
    let record = m
        .add(&coder, "  Репозиторий биллинга — gitlab.example/billing  ")
        .await
        .unwrap();
    let Caller::Agent { run_id, .. } = &coder else {
        unreachable!()
    };
    assert_eq!(record.text, "Репозиторий биллинга — gitlab.example/billing");
    assert_eq!(record.run_id, Some(*run_id));
    m.add(&coder, "The operator prefers short answers.")
        .await
        .unwrap();

    let list = m.list(&coder, &alias("coder")).await.unwrap();
    let texts: Vec<&str> = list.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "The operator prefers short answers.",
            "Репозиторий биллинга — gitlab.example/billing"
        ]
    );
}

#[tokio::test]
async fn search_finds_a_substring_in_any_language() {
    let m = memory().await;
    let coder = agent("coder");
    m.add(&coder, "Отчёты биллинга выгружаем в CSV")
        .await
        .unwrap();
    m.add(&coder, "Deploys happen on Fridays").await.unwrap();
    m.add(&coder, "Use UTF-8 everywhere").await.unwrap();
    let me = alias("coder");

    let found = m.search(&coder, &me, "отчёт").await.unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].text.starts_with("Отчёты"));
    assert_eq!(m.search(&coder, &me, "deploy").await.unwrap().len(), 1);
    // Shorter than a trigram: a plain scan.
    assert_eq!(m.search(&coder, &me, "8").await.unwrap().len(), 1);
    // FTS syntax is just text.
    assert!(m.search(&coder, &me, "\"OR*:").await.unwrap().is_empty());
    assert!(matches!(
        m.search(&coder, &me, " ").await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn memory_is_the_agents_own_and_the_operator_sees_all() {
    let m = memory().await;
    let record = m.add(&agent("coder"), "secret plan").await.unwrap();
    let manager = agent("manager");
    assert!(matches!(
        m.list(&manager, &alias("coder")).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        m.search(&manager, &alias("coder"), "plan").await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        m.delete(&manager, &alias("coder"), record.id).await,
        Err(Error::Forbidden(_))
    ));
    // Nor can a record be deleted through another agent's memory.
    assert!(matches!(
        m.delete(&manager, &alias("manager"), record.id).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        m.add(&operator(), "x").await,
        Err(Error::Forbidden(_))
    ));

    assert_eq!(m.list(&operator(), &alias("coder")).await.unwrap().len(), 1);
    m.delete(&operator(), &alias("coder"), record.id)
        .await
        .unwrap();
    assert!(
        m.list(&operator(), &alias("coder"))
            .await
            .unwrap()
            .is_empty()
    );
    // Gone from the index too.
    assert!(
        m.search(&operator(), &alias("coder"), "plan")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn limits_make_the_agent_choose_what_to_forget() {
    let m = memory().await;
    let coder = agent("coder");
    assert!(matches!(
        m.add(&coder, &"x".repeat(MAX_RECORD_BYTES + 1)).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        m.add(&coder, &format!("token {LEAKED}")).await,
        Err(Error::Forbidden(_))
    ));
    // Total size: 32 records of 2 KiB fill 64 KiB.
    for i in 0..MAX_TOTAL_BYTES / MAX_RECORD_BYTES {
        let text = format!("{i:04}{}", "y".repeat(MAX_RECORD_BYTES - 4));
        m.add(&coder, &text).await.unwrap();
    }
    let full = m.add(&coder, "one more").await;
    assert!(
        matches!(&full, Err(Error::InvalidInput(e)) if e.contains("memory is full")),
        "{full:?}"
    );
    // Another agent's memory is its own.
    m.add(&agent("manager"), "fine").await.unwrap();
    // Deleting makes room.
    let oldest = m
        .list(&coder, &alias("coder"))
        .await
        .unwrap()
        .pop()
        .unwrap();
    m.delete(&coder, &alias("coder"), oldest.id).await.unwrap();
    m.add(&coder, "one more").await.unwrap();
}

#[tokio::test]
async fn the_number_of_records_is_capped_too() {
    let m = memory().await;
    let coder = agent("coder");
    for i in 0..MAX_RECORDS {
        m.add(&coder, &format!("fact {i}")).await.unwrap();
    }
    assert!(matches!(
        m.add(&coder, "fact 200").await,
        Err(Error::InvalidInput(_))
    ));
}
