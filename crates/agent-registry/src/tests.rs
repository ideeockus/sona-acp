use super::*;
use control_plane_common::registry::{Access, AgentState, ModelRef, default_hue};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn memory_pool() -> SqlitePool {
    // One connection that never closes: every connection gets its own in-memory database.
    SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap()
}

async fn registry() -> SqliteRegistry {
    SqliteRegistry::new(memory_pool().await).await.unwrap()
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
        access: vec![Access::ReadFiles],
        allow_message_to: vec![alias("manager")],
        prompt: "You write code.".to_owned(),
        hue: None,
        skills: Vec::new(),
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
    assert_eq!((read.card, read.version), (card("coder").with_hue(), 1));
    assert_eq!(
        registry.agent(&alias("coder")).await.unwrap(),
        card("coder").with_hue()
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
        .map(|c| c.card.alias.to_string())
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
async fn no_agent_takes_the_schedulers_alias() {
    let registry = registry_with_coder().await;
    assert!(
        registry
            .create_agent(&operator(), card("scheduler"))
            .await
            .is_err()
    );
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

#[tokio::test]
async fn cards_from_before_move_to_groups_and_become_version_one() {
    let pool = memory_pool().await;
    let mut before = sqlx::migrate!();
    before.migrations = before.migrations[..1].to_vec().into();
    before.run(&pool).await.unwrap();
    let old = r#"{"alias": "coder", "title": "", "model": {"provider": "p", "id": "m"},
                  "allow_tools": ["read_messages", "read", "edit", "write", "send_message"],
                  "allow_message_to": [], "prompt": ""}"#;
    // Agents exist only after `init` created the operator.
    sqlx::query(
        "INSERT INTO principals (alias, kind, card)
         VALUES ('user', 'operator', NULL), ('coder', 'agent', ?)",
    )
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();

    let registry = SqliteRegistry::new(pool).await.unwrap();
    let mut access = registry.agent(&alias("coder")).await.unwrap().access;
    access.sort_by_key(|a| a.tools()[0]);
    assert_eq!(
        access,
        [Access::ReadFiles, Access::SendMessages, Access::WriteFiles]
    );
    // The card from before is version 1, saved by the operator.
    let versions = registry
        .agent_versions(&operator(), &alias("coder"))
        .await
        .unwrap();
    assert_eq!(
        (versions.len(), versions[0].version, &versions[0].author),
        (1, 1, &alias("user"))
    );
}

#[tokio::test]
async fn a_card_gets_a_hue_from_its_alias_unless_given_one() {
    let registry = registry_with_coder().await;
    let coder = registry.agent(&alias("coder")).await.unwrap();
    assert_eq!(coder.hue, Some(default_hue(&alias("coder"))));

    let painted = AgentCard {
        hue: Some(25),
        ..card("painter")
    };
    registry.create_agent(&operator(), painted).await.unwrap();
    assert_eq!(
        registry.agent(&alias("painter")).await.unwrap().hue,
        Some(25)
    );

    let invalid = AgentCard {
        hue: Some(360),
        ..card("invalid")
    };
    assert!(matches!(
        registry.create_agent(&operator(), invalid).await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
async fn an_edit_saves_the_next_version_and_keeps_the_previous() {
    let registry = registry_with_coder().await;
    let mut edit = registry
        .get_agent(&operator(), &alias("coder"))
        .await
        .unwrap();
    edit.card.prompt = "You write tests first.".to_owned();
    let saved = registry
        .update_agent(&operator(), edit.clone())
        .await
        .unwrap();
    assert_eq!(saved.version, 2);
    assert_eq!(registry.agent(&alias("coder")).await.unwrap(), saved.card);
    assert_eq!(registry.record(&alias("coder")).await.unwrap(), saved);

    let versions = registry
        .agent_versions(&operator(), &alias("coder"))
        .await
        .unwrap();
    let numbers: Vec<u32> = versions.iter().map(|v| v.version).collect();
    assert_eq!(numbers, [2, 1]);
    assert_eq!(versions[0].author, alias("user"));
    assert_eq!(versions[1].card.prompt, "You write code.");
}

#[tokio::test]
async fn an_edit_based_on_an_old_version_is_refused() {
    let registry = registry_with_coder().await;
    let base = registry
        .get_agent(&operator(), &alias("coder"))
        .await
        .unwrap();
    let mut first = base.clone();
    first.card.title = "First".to_owned();
    registry.update_agent(&operator(), first).await.unwrap();

    let mut second = base;
    second.card.title = "Second".to_owned();
    assert!(matches!(
        registry.update_agent(&operator(), second).await,
        Err(Error::Conflict(_))
    ));
    let current = registry.agent(&alias("coder")).await.unwrap();
    assert_eq!(current.title, "First");
}

#[tokio::test]
async fn saving_the_same_card_makes_no_version() {
    let registry = registry_with_coder().await;
    let same = registry
        .get_agent(&operator(), &alias("coder"))
        .await
        .unwrap();
    let saved = registry.update_agent(&operator(), same).await.unwrap();
    assert_eq!(saved.version, 1);
    let versions = registry
        .agent_versions(&operator(), &alias("coder"))
        .await
        .unwrap();
    assert_eq!(versions.len(), 1);
}

#[tokio::test]
async fn only_the_operator_edits_and_unknown_agents_are_not_found() {
    let registry = registry_with_coder().await;
    let edit = registry
        .get_agent(&operator(), &alias("coder"))
        .await
        .unwrap();
    let agent = Caller::Agent {
        alias: alias("coder"),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    };
    assert!(matches!(
        registry.update_agent(&agent, edit.clone()).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        registry.agent_versions(&agent, &alias("coder")).await,
        Err(Error::Forbidden(_))
    ));
    let ghost = Agent {
        card: card("ghost"),
        version: 1,
        state: AgentState::Active,
    };
    assert!(matches!(
        registry.update_agent(&operator(), ghost).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn a_disabled_agent_loses_its_run_tokens_until_enabled() {
    let registry = registry_with_coder().await;
    let coder = alias("coder");
    let token = registry
        .issue_run_token(&coder, RunId::new(), CorrelationId::new())
        .await
        .unwrap();

    registry.disable_agent(&operator(), &coder).await.unwrap();
    let record = registry.record(&coder).await.unwrap();
    assert_eq!(record.state, AgentState::Disabled);
    assert!(matches!(
        registry.authenticate(token.expose()).await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        registry
            .issue_run_token(&coder, RunId::new(), CorrelationId::new())
            .await,
        Err(Error::InvalidInput(_))
    ));

    registry.enable_agent(&operator(), &coder).await.unwrap();
    assert_eq!(
        registry.record(&coder).await.unwrap().state,
        AgentState::Active
    );
    assert!(matches!(
        registry.disable_agent(&operator(), &alias("ghost")).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn a_deleted_agent_is_gone_but_its_alias_stays_taken() {
    let registry = registry_with_coder().await;
    let coder = alias("coder");
    registry.delete_agent(&operator(), &coder).await.unwrap();

    assert!(matches!(
        registry.agent(&coder).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        registry.agent_versions(&operator(), &coder).await,
        Err(Error::NotFound(_))
    ));
    assert!(registry.list_agents(&operator()).await.unwrap().is_empty());
    let Err(Error::InvalidInput(reason)) = registry.create_agent(&operator(), card("coder")).await
    else {
        panic!("a deleted agent's alias was given again");
    };
    assert!(reason.contains("deleted"), "{reason}");
    assert!(matches!(
        registry.delete_agent(&operator(), &coder).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn only_the_operator_disables_and_deletes() {
    let registry = registry_with_coder().await;
    let agent = Caller::Agent {
        alias: alias("coder"),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    };
    let coder = alias("coder");
    for denied in [
        registry.disable_agent(&agent, &coder).await,
        registry.enable_agent(&agent, &coder).await,
        registry.delete_agent(&agent, &coder).await,
    ] {
        assert!(matches!(denied, Err(Error::Forbidden(_))));
    }
}

#[tokio::test]
async fn the_operator_opens_a_session_with_the_credential() {
    let pool = memory_pool().await;
    let registry = SqliteRegistry::new(pool.clone()).await.unwrap();
    let credential = registry.create_operator(&alias("user")).await.unwrap();

    let session = registry.open_session(credential.expose()).await.unwrap();
    assert!(session.secret.expose().starts_with("sona_plane_ss_"));
    assert_eq!(
        registry
            .session_caller(session.secret.expose())
            .await
            .unwrap(),
        operator()
    );
    // A session is for the cookie only, never a bearer credential.
    assert!(matches!(
        registry.authenticate(session.secret.expose()).await,
        Err(Error::Unauthenticated)
    ));

    registry
        .close_session(session.secret.expose())
        .await
        .unwrap();
    assert!(matches!(
        registry.session_caller(session.secret.expose()).await,
        Err(Error::Unauthenticated)
    ));

    let ended = registry.open_session(credential.expose()).await.unwrap();
    sqlx::query("UPDATE sessions SET expires_at = 0")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        registry.session_caller(ended.secret.expose()).await,
        Err(Error::Unauthenticated)
    ));
}

#[tokio::test]
async fn only_the_operator_credential_opens_a_session() {
    let registry = registry_with_coder().await;
    let token = registry
        .issue_run_token(&alias("coder"), RunId::new(), CorrelationId::new())
        .await
        .unwrap();
    assert!(matches!(
        registry.open_session(token.expose()).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        registry.open_session("sona_plane_op_nope").await,
        Err(Error::Unauthenticated)
    ));
}

fn github(to: &[&str]) -> Adapter {
    Adapter {
        alias: alias("github"),
        title: "Issues of the billing repo".to_owned(),
        allow_message_to: to.iter().map(|a| alias(a)).collect(),
    }
}

#[tokio::test]
async fn an_adapter_signs_in_with_its_own_credential() {
    let registry = registry_with_coder().await;
    let secret = registry
        .create_adapter(&operator(), github(&["coder"]))
        .await
        .unwrap();
    assert!(secret.expose().starts_with(ADAPTER_PREFIX));
    assert_eq!(
        registry.authenticate(secret.expose()).await.unwrap(),
        Caller::Adapter {
            alias: alias("github")
        }
    );
    assert_eq!(
        registry.adapter(&alias("github")).await.unwrap(),
        github(&["coder"])
    );
    // An adapter is not an agent, and its alias is taken.
    assert!(registry.agent(&alias("github")).await.is_err());
    assert!(
        registry
            .create_agent(&operator(), card("github"))
            .await
            .is_err()
    );

    // A new credential: the old one stops at once.
    let rotated = registry
        .rotate_adapter(&operator(), &alias("github"))
        .await
        .unwrap();
    assert!(matches!(
        registry.authenticate(secret.expose()).await,
        Err(Error::Unauthenticated)
    ));
    registry.authenticate(rotated.expose()).await.unwrap();

    registry
        .delete_adapter(&operator(), &alias("github"))
        .await
        .unwrap();
    assert!(matches!(
        registry.authenticate(rotated.expose()).await,
        Err(Error::Unauthenticated)
    ));
    // Never given again.
    assert!(
        registry
            .create_adapter(&operator(), github(&[]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_adapter_writes_only_to_agents_that_exist_and_only_the_operator_manages_it() {
    let registry = registry_with_coder().await;
    assert!(matches!(
        registry
            .create_adapter(&operator(), github(&["ghost"]))
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        registry
            .create_adapter(
                &operator(),
                Adapter {
                    title: " ".to_owned(),
                    ..github(&[])
                }
            )
            .await,
        Err(Error::InvalidInput(_))
    ));
    let agent = Caller::Agent {
        alias: alias("coder"),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    };
    assert!(matches!(
        registry.create_adapter(&agent, github(&["coder"])).await,
        Err(Error::Forbidden(_))
    ));
    registry
        .create_adapter(&operator(), github(&[]))
        .await
        .unwrap();
    let updated = registry
        .update_adapter(&operator(), github(&["coder"]))
        .await
        .unwrap();
    assert_eq!(updated.allow_message_to, [alias("coder")]);
    assert_eq!(
        registry.list_adapters(&operator()).await.unwrap(),
        [updated]
    );
}

#[tokio::test]
async fn principals_survive_the_rebuild_for_adapters() {
    let pool = memory_pool().await;
    let mut before = sqlx::migrate!();
    before.migrations = before.migrations[..6].to_vec().into();
    before.run(&pool).await.unwrap();
    let old = SqliteRegistry { pool: pool.clone() };
    let secret = old.create_operator(&alias("user")).await.unwrap();
    old.create_agent(&operator(), card("coder")).await.unwrap();
    let session = old.open_session(secret.expose()).await.unwrap();

    let registry = SqliteRegistry::new(pool).await.unwrap();
    assert_eq!(
        registry.authenticate(secret.expose()).await.unwrap(),
        operator()
    );
    assert_eq!(
        registry.agent(&alias("coder")).await.unwrap().title,
        "Writes code"
    );
    assert_eq!(
        registry
            .agent_versions(&operator(), &alias("coder"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        registry
            .session_caller(session.secret.expose())
            .await
            .unwrap(),
        operator()
    );
    registry
        .create_adapter(&operator(), github(&["coder"]))
        .await
        .unwrap();
}
