use super::*;
use control_plane_common::{CorrelationId, RunId};
use snapshot::frontmatter;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn library(name: &str) -> SqliteSkills {
    // One connection that never closes: every connection gets its own in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let root = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/test-skills"
    ))
    .join(format!("skills-{name}-{}", uuid::Uuid::now_v7()));
    SqliteSkills::new(pool, root).await.unwrap()
}

fn agent(name: &str) -> Caller {
    Caller::Agent {
        alias: name.parse().unwrap(),
        run_id: RunId::new(),
        correlation_id: CorrelationId::new(),
    }
}

fn operator() -> Caller {
    Caller::Operator {
        alias: "user".parse().unwrap(),
    }
}

fn skill(name: &str, body: &str) -> Vec<ProposedFile> {
    vec![
        ProposedFile {
            path: "SKILL.md".to_owned(),
            content: format!(
                "---\nname: {name}\ndescription: Use when exporting CSV.\n---\n\n{body}\n"
            ),
        },
        ProposedFile {
            path: "scripts/export.sh".to_owned(),
            content: "echo export\n".to_owned(),
        },
    ]
}

#[test]
fn frontmatter_reads_plain_quoted_and_block_values() {
    let plain = "---\nname: csv-export\ndescription: Export tables as CSV.\n---\nbody";
    assert_eq!(
        frontmatter(plain).unwrap(),
        ("csv-export".to_owned(), "Export tables as CSV.".to_owned())
    );
    let quoted =
        "---\nname: \"pdf\"\ndescription: 'Read PDFs: text and tables.'\nlicense: MIT\n---\n";
    assert_eq!(
        frontmatter(quoted).unwrap().1,
        "Read PDFs: text and tables."
    );
    let folded = "---\nname: pdf\ndescription: >\n  Read PDFs.\n  Use for reports.\nmetadata:\n  a: b\n---\n";
    assert_eq!(
        frontmatter(folded).unwrap().1,
        "Read PDFs. Use for reports."
    );

    for bad in [
        "no frontmatter",
        "---\nname: Bad_Name\ndescription: x\n---\n",
        "---\nname: a--b\ndescription: x\n---\n",
        "---\nname: fine\n---\n",
    ] {
        assert!(
            matches!(frontmatter(bad), Err(Error::InvalidInput(_))),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn a_proposed_skill_arrives_disabled_and_mounts_once_enabled() {
    let lib = library("propose").await;
    let proposed = lib
        .propose(&agent("coder"), skill("csv-export", "Write CSV."))
        .await
        .unwrap();
    assert_eq!(proposed.name, "csv-export");
    assert!(!proposed.enabled);
    assert_eq!(
        proposed.source,
        SkillSource::Proposed {
            by: "coder".parse().unwrap()
        }
    );
    // Disabled: nothing to mount.
    let names = vec!["csv-export".to_owned(), "missing".to_owned()];
    assert!(lib.enabled(&names).await.unwrap().is_empty());

    let detail = lib.get_skill(&operator(), "csv-export").await.unwrap();
    assert!(detail.instructions.contains("Write CSV."));
    let paths: Vec<&str> = detail.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["SKILL.md", "scripts/export.sh"]);

    lib.set_enabled(&operator(), "csv-export", true)
        .await
        .unwrap();
    let mounts = lib.enabled(&names).await.unwrap();
    assert_eq!(mounts.len(), 1);
    let (name, dir) = &mounts[0];
    assert_eq!(name, "csv-export");
    assert_eq!(
        std::fs::read_to_string(dir.join("scripts/export.sh")).unwrap(),
        "echo export\n"
    );
}

#[tokio::test]
async fn the_same_agent_may_revise_its_skill_and_must_be_reviewed_again() {
    let lib = library("revise").await;
    let first = lib
        .propose(&agent("coder"), skill("csv-export", "v1"))
        .await
        .unwrap();
    lib.set_enabled(&operator(), "csv-export", true)
        .await
        .unwrap();
    // The same content changes nothing, not even the review.
    let same = lib
        .propose(&agent("coder"), skill("csv-export", "v1"))
        .await
        .unwrap();
    assert!(same.enabled);
    let second = lib
        .propose(&agent("coder"), skill("csv-export", "v2"))
        .await
        .unwrap();
    assert_ne!(second.version, first.version);
    assert!(!second.enabled);
    // Another agent may not take the name.
    assert!(matches!(
        lib.propose(&agent("manager"), skill("csv-export", "mine"))
            .await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn proposals_are_checked() {
    let lib = library("checks").await;
    let coder = agent("coder");
    let mut escaping = skill("csv-export", "x");
    escaping[1].path = "../outside.sh".to_owned();
    let mut absolute = skill("csv-export", "x");
    absolute[1].path = "/etc/passwd".to_owned();
    let no_manifest = vec![ProposedFile {
        path: "README.md".to_owned(),
        content: "hi".to_owned(),
    }];
    let huge = vec![
        skill("big", "x").remove(0),
        ProposedFile {
            path: "blob.txt".to_owned(),
            content: "x".repeat(control_plane_common::skills::MAX_SKILL_BYTES),
        },
    ];
    for files in [escaping, absolute, no_manifest, huge] {
        assert!(matches!(
            lib.propose(&coder, files).await,
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(matches!(
        lib.propose(&operator(), skill("csv-export", "x")).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn the_library_is_the_operators() {
    let lib = library("operator").await;
    lib.propose(&agent("coder"), skill("csv-export", "x"))
        .await
        .unwrap();
    let coder = agent("coder");
    assert!(matches!(
        lib.list_skills(&coder).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        lib.set_enabled(&coder, "csv-export", true).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        lib.update_skill(&operator(), "csv-export").await,
        Err(Error::InvalidInput(_))
    ));
    let git = GitSkill {
        url: "file:///etc".to_owned(),
        git_ref: None,
        path: None,
    };
    assert!(matches!(
        lib.add_from_git(&operator(), git).await,
        Err(Error::InvalidInput(_))
    ));

    lib.delete_skill(&operator(), "csv-export").await.unwrap();
    assert!(lib.list_skills(&operator()).await.unwrap().is_empty());
    assert!(matches!(
        lib.delete_skill(&operator(), "csv-export").await,
        Err(Error::NotFound(_))
    ));
}
