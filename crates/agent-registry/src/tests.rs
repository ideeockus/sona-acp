use super::*;
use control_plane_common::registry::ModelRef;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn registry() -> SqliteRegistry {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    SqliteRegistry::new(pool).await.unwrap()
}

fn alias(name: &str) -> Alias {
    name.parse().unwrap()
}

fn operator() -> Caller {
    Caller::Operator {
        alias: alias("user"),
    }
}

fn card(name: &str) -> AgentCard {
    AgentCard {
        alias: alias(name),
        title: "Writes code".to_owned(),
        model: ModelRef {
            provider: "anthropic".to_owned(),
            id: "model".to_owned(),
        },
        allow_tools: vec!["read".to_owned()],
        allow_message_to: vec![alias("manager")],
        prompt: "You write code.".to_owned(),
    }
}

async fn registry_with_coder() -> SqliteRegistry {
    let registry = registry().await;
    registry
        .create_agent(&operator(), card("coder"))
        .await
        .unwrap();
    registry
}

#[tokio::test]
async fn created_agent_can_be_read_back() {
    let registry = registry_with_coder().await;
    let read = registry
        .get_agent(&operator(), &alias("coder"))
        .await
        .unwrap();
    assert_eq!(read, card("coder"));
    assert_eq!(
        registry.agent(&alias("coder")).await.unwrap(),
        card("coder")
    );
}

#[tokio::test]
async fn agents_are_listed_by_alias() {
    let registry = registry_with_coder().await;
    registry
        .create_agent(&operator(), card("architect"))
        .await
        .unwrap();
    let names: Vec<String> = registry
        .list_agents(&operator())
        .await
        .unwrap()
        .iter()
        .map(|c| c.alias.to_string())
        .collect();
    assert_eq!(names, ["@architect", "@coder"]);
}

#[tokio::test]
async fn unknown_agent_is_not_found() {
    let registry = registry().await;
    assert!(matches!(
        registry.agent(&alias("ghost")).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn an_agent_cannot_manage_agents() {
    let registry = registry_with_coder().await;
    let agent = Caller::Agent {
        alias: alias("coder"),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    };
    assert!(matches!(
        registry.create_agent(&agent, card("evil")).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        registry.get_agent(&agent, &alias("coder")).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        registry.list_agents(&agent).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn aliases_are_unique_across_kinds() {
    let registry = registry_with_coder().await;
    assert!(matches!(
        registry.create_agent(&operator(), card("coder")).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        registry.create_operator(&alias("coder")).await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn operator_credential_authenticates() {
    let registry = registry().await;
    let secret = registry.create_operator(&alias("user")).await.unwrap();
    assert!(secret.expose().starts_with(OPERATOR_PREFIX));
    assert_eq!(
        registry.authenticate(secret.expose()).await.unwrap(),
        operator()
    );
}

#[tokio::test]
async fn run_token_resolves_to_its_run() {
    let registry = registry_with_coder().await;
    let (run_id, correlation_id) = (RunId::new(), CorrelationId::new());
    let token = registry
        .issue_run_token(&alias("coder"), run_id, correlation_id)
        .await
        .unwrap();
    assert!(token.expose().starts_with(RUN_PREFIX));
    assert_eq!(
        registry.authenticate(token.expose()).await.unwrap(),
        Caller::Agent {
            alias: alias("coder"),
            run_id,
            correlation_id,
        }
    );
}

#[tokio::test]
async fn unknown_credential_is_rejected() {
    let registry = registry().await;
    assert_eq!(
        registry.authenticate("sona_plane_rt_nope").await,
        Err(Error::Unauthenticated)
    );
}

#[tokio::test]
async fn only_agents_get_run_tokens() {
    let registry = registry().await;
    registry.create_operator(&alias("user")).await.unwrap();
    assert!(matches!(
        registry
            .issue_run_token(&alias("user"), RunId::new(), CorrelationId::new())
            .await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn revoked_run_token_stops_working() {
    let registry = registry_with_coder().await;
    let run_id = RunId::new();
    let token = registry
        .issue_run_token(&alias("coder"), run_id, CorrelationId::new())
        .await
        .unwrap();
    registry.revoke_run_token(run_id).await.unwrap();
    assert_eq!(
        registry.authenticate(token.expose()).await,
        Err(Error::Unauthenticated)
    );
}

#[tokio::test]
async fn startup_revocation_keeps_the_operator() {
    let registry = registry_with_coder().await;
    let operator_secret = registry.create_operator(&alias("user")).await.unwrap();
    let token = registry
        .issue_run_token(&alias("coder"), RunId::new(), CorrelationId::new())
        .await
        .unwrap();
    registry.revoke_all_run_tokens().await.unwrap();
    assert!(registry.authenticate(token.expose()).await.is_err());
    assert!(
        registry
            .authenticate(operator_secret.expose())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn only_the_hash_is_stored() {
    let registry = registry_with_coder().await;
    let token = registry
        .issue_run_token(&alias("coder"), RunId::new(), CorrelationId::new())
        .await
        .unwrap();
    let stored: Vec<u8> = sqlx::query_scalar("SELECT hash FROM credentials")
        .fetch_one(&registry.pool)
        .await
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert_ne!(stored, token.expose().as_bytes());
}

#[tokio::test]
async fn finds_active_token_inside_text() {
    let registry = registry_with_coder().await;
    let run_id = RunId::new();
    let token = registry
        .issue_run_token(&alias("coder"), run_id, CorrelationId::new())
        .await
        .unwrap();
    let text = format!("кстати, вот токен: {} пока", token.expose());

    assert!(registry.contains_active_run_token(&text).await.unwrap());
    assert!(!registry.contains_active_run_token("clean").await.unwrap());
    assert!(
        !registry
            .contains_active_run_token("sona_plane_rt_короткий")
            .await
            .unwrap()
    );

    registry.revoke_run_token(run_id).await.unwrap();
    assert!(!registry.contains_active_run_token(&text).await.unwrap());
}
